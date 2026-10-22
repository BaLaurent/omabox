//! The user's own Hyprland, for what omabox does there on purpose (interactive windows, keys-to-box,
//! peek): its session (host_session), hyprctl on it, and the host side of interactive mode
//! (share/passthrough.lua), installed with hyprctl eval: runtime only, never in the user's config.

use crate::cat;
use crate::exit::{Ctx, Res, say};
use crate::jq::{self, Args};
use crate::shell;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::process::Stdio;

/// What toggles the passthrough, as share/passthrough.lua is told it.
pub const PASS_KEY: &str = "SUPER + ALT + ESCAPE";
/// The signature the instances call is made under (the agent guard's).
const GUARD: &str = "omabox-guard";

/// host_session: the user's Hyprland session (its signature): the environment's when it names a
/// live one, else the only one in the runtime dir; several and none named are refused (finding 65).
pub fn host_session(ctx: &Ctx) -> Result<Vec<u8>, Vec<u8>> {
    host_session_wl(ctx).map(|(sig, _)| sig)
}

/// host_session's signature and HOST_WL, its Wayland socket's path.
pub fn host_session_wl(ctx: &Ctx) -> Result<(Vec<u8>, Vec<u8>), Vec<u8>> {
    let rt = shell::env_or_empty("XDG_RUNTIME_DIR");
    let sig = shell::env_or_empty("HYPRLAND_INSTANCE_SIGNATURE");
    let all = crate::sig::command("hyprctl")
        .args(["-j", "instances"])
        .env("HYPRLAND_INSTANCE_SIGNATURE", GUARD)
        .stdin(Stdio::inherit())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| shell::subst(&o.stdout))
        .filter(|a| jq::jq(&["-e", r#"type == "array""#], Some(a), jq::Err::Null).ok())
        .unwrap_or(b"[]".to_vec());
    let live = !sig.is_empty()
        && Args::new(&["-e"])
            .kv("--arg", "s", &sig)
            .arg("any(.[]; .instance == $s)")
            .run(Some(&all), jq::Err::Null)
            .ok();
    let mut sig = sig;
    if !live {
        match jq::jq(&["length"], Some(&all), jq::Err::Inherit)
            .text()
            .as_slice()
        {
            b"1" => sig = jq::jq(&["-r", ".[0].instance"], Some(&all), jq::Err::Inherit).text(),
            b"0" => {
                return Err(cat!(
                    "no Hyprland session in ",
                    rt,
                    "/hypr: this needs the user's desktop"
                ));
            }
            _ => {
                let all_s = jq::jq(
                    &["-r", r#"map(.instance) | join(", ")"#],
                    Some(&all),
                    jq::Err::Inherit,
                )
                .text();
                return Err(cat!(
                    "several Hyprland sessions: set HYPRLAND_INSTANCE_SIGNATURE to one of ",
                    all_s
                ));
            }
        }
    }
    let wl = Args::new(&["-r"])
        .kv("--arg", "s", &sig)
        .arg("first(.[] | select(.instance == $s) | .wl_socket) // empty")
        .run(Some(&all), jq::Err::Inherit)
        .text();
    let sock = std::fs::metadata(shell::path(&cat!(rt, "/", wl)));
    if !(shell::re_match("^[A-Za-z0-9._-]+$", &wl) && sock.is_ok_and(|m| m.file_type().is_socket()))
    {
        return Err(cat!("Hyprland session ", sig, ": no Wayland socket found"));
    }
    let _ = ctx;
    Ok((sig, cat!(rt, "/", wl)))
}

/// `host_session` called by itself: its refusal ends the command (die).
pub fn host_session_die(ctx: &Ctx) -> Res<(Vec<u8>, Vec<u8>)> {
    host_session_wl(ctx).map_err(|m| ctx.die(m))
}

/// `$(host_hyprctl ARGS...)`: hyprctl on the user's session, its status and stdout (its stderr this
/// process's, or none with QUIET: `2>/dev/null`). host_session's refusal is said (unless QUIET) and
/// is the call's failure, status 1, as its die ends the `$(...)`.
pub fn host_hyprctl(ctx: &Ctx, args: &[&[u8]], quiet: bool) -> (i32, Vec<u8>) {
    let sig = match host_session(ctx) {
        Ok(s) => s,
        Err(m) => {
            if !quiet {
                say(m);
            }
            return (i32::from(ctx.die_rc.unwrap_or(1)), Vec::new());
        }
    };
    let mut c = crate::sig::command("hyprctl");
    c.args(args.iter().map(|a| OsStr::from_bytes(a)))
        .env("HYPRLAND_INSTANCE_SIGNATURE", OsStr::from_bytes(&sig))
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(if quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        });
    // (the status read inside: a child's death by SIGINT is the foreground's, sig::FG_INT)
    match crate::sig::foreground(|| {
        c.output()
            .map(|o| (crate::exec::status(o.status).0, o.stdout))
    }) {
        Ok(o) => o,
        Err(_) => {
            if !quiet {
                let _ = std::io::Write::write_all(
                    &mut std::io::stderr(),
                    b"hyprctl: command not found\n",
                );
            }
            (127, Vec::new())
        }
    }
}

/// host_eval LUA: Lua on the host Hyprland (`out=$(host_hyprctl eval "$1" 2>&1)`); its errors are
/// ours to report. host_session's refusal is said through it, as the words it got.
pub fn host_eval(ctx: &Ctx, lua: &[u8]) -> Res<()> {
    let out = match host_session(ctx) {
        Err(m) => cat!("omabox: ", m),
        Ok(sig) => {
            let mut c = crate::sig::command("hyprctl");
            c.arg("eval")
                .arg(OsStr::from_bytes(lua))
                .env("HYPRLAND_INSTANCE_SIGNATURE", OsStr::from_bytes(&sig))
                .stdin(Stdio::inherit());
            // (2>&1: one pipe for both, in the order said)
            match crate::exec::pipe() {
                Ok((r, w)) => {
                    let w2 = w.try_clone().ok();
                    c.stdout(Stdio::from(w));
                    if let Some(w2) = w2 {
                        c.stderr(Stdio::from(w2));
                    }
                    // (a foreground child: a trap on its way waits for it, and nothing new starts)
                    crate::sig::foreground(|| {
                        let child = c.spawn();
                        drop(c);
                        let got = crate::exec::read_all(r, None);
                        match child {
                            Ok(mut ch) => {
                                let _ = ch.wait().map(crate::exec::status);
                                shell::subst(&got)
                            }
                            Err(_) => b"hyprctl: command not found".to_vec(),
                        }
                    })
                }
                Err(_) => Vec::new(),
            }
        }
    };
    if out != b"ok" {
        return Err(ctx.die(cat!("host hyprctl eval failed: ", out)));
    }
    Ok(())
}

/// lua_str V: V for a Lua string literal, or (said) nothing: no quote, backslash or newline.
pub fn lua_str(ctx: &Ctx, v: &[u8]) -> Vec<u8> {
    if v.iter().any(|&c| matches!(c, b'\'' | b'"' | b'\\' | b'\n')) {
        let _ = ctx;
        say(cat!(
            "unsafe characters in a path for the host Hyprland: ",
            v
        ));
        return Vec::new();
    }
    v.to_vec()
}

/// install_passthrough: share/passthrough.lua on the host, with the theme's colours read now (a
/// theme change reloads the config, so it is installed again with the new ones).
pub fn install_passthrough(ctx: &Ctx) -> Res<()> {
    let home = shell::env_or_empty("HOME");
    let mut colors = cat!(home, "/.local/state/omarchy/current/theme/colors.toml");
    if !shell::path(&colors).is_file() {
        colors = cat!(home, "/.config/omarchy/current/theme/colors.toml");
    }
    let script = std::fs::read(crate::root().join("share/passthrough.lua")).unwrap_or_default();
    let lua = cat!(
        "(function(...)\n",
        shell::strip_nl(&script),
        "\nend)('",
        lua_str(ctx, &ctx.boxes),
        "', '",
        PASS_KEY,
        "', '",
        lua_str(ctx, &colors),
        "')"
    );
    host_eval(ctx, &lua)
}
