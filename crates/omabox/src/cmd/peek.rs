//! `omabox peek [-b NAME] [--fps N] [--workspace WS] [--monitor NAME] [--focus]`: a live, view-only
//! window of a headless box on the user's workspace 9 (or the configured one, finding 70), opened
//! without focus by the host Hyprland's exec rules: frames copied out of the box (tools/peek),
//! nothing sent in. --focus (the bar widget's click: the user asked) then brings it forward, and for
//! an interactive box its own window; nothing is focused unasked. The host's Lua gets checked,
//! tame values only (finding 63).

use crate::args::{optarg, unknown_opt};
use crate::boxes::{BoxPid, box_env, box_pid, meta, need_box};
use crate::cat;
use crate::deskwin::ws_check;
use crate::exit::{Ctx, Res, say};
use crate::host::{host_eval, host_hyprctl, host_session_die, lua_str};
use crate::hypr::hypr_json;
use crate::jq::{self, Args};
use crate::marks::peek_pid;
use crate::screen::main_screen;
use crate::select::select_box;
use crate::settle::ws_text;
use crate::shell::{self, strip_nl};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    // An INT to omabox alone while --focus waits: bash waits for its child and goes on (trap.rs).
    crate::trap::int_as_bash();
    let (mut name, mut fps, mut focus, mut ws, mut monitor) =
        (Vec::new(), b"10".to_vec(), false, Vec::new(), Vec::new());
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let v = args.get(i + 1);
        match a.as_slice() {
            b"-b" | b"--box" => {
                optarg(ctx, a, v)?;
                name = args[i + 1].clone();
                i += 1;
            }
            b"--fps" => {
                optarg(ctx, a, v)?;
                fps = args[i + 1].clone();
                i += 1;
            }
            b"--workspace" => {
                optarg(ctx, a, v)?;
                let w = &args[i + 1];
                ws = ws_check(w).ok_or_else(|| {
                    ctx.die(cat!(
                        "--workspace: 1-99, special (the scratchpad, SUPER+S) or special:NAME, got ",
                        w
                    ))
                })?;
                i += 1;
            }
            b"--focus" => focus = true,
            b"--monitor" => {
                optarg(ctx, a, v)?;
                let m = &args[i + 1];
                if !shell::re_match("^[A-Za-z0-9_-]{1,32}$", m) {
                    return Err(ctx.die(cat!(
                        "peek: --monitor takes a monitor's name (omabox monitor list), got ",
                        m
                    )));
                }
                monitor = m.clone();
                i += 1;
            }
            w if w.starts_with(b"-") => return Err(unknown_opt(ctx, "peek", a)),
            w => {
                if !name.is_empty() {
                    return Err(ctx.die(cat!("peek: one box name, got ", name, " and ", w)));
                }
                name = w.to_vec();
            }
        }
        i += 1;
    }
    if !shell::re_match("^[1-9][0-9]?$", &fps) {
        return Err(ctx.die(cat!("peek: --fps is 1 to 99, got ", fps)));
    }
    select_box(ctx, &name)?;
    need_box(ctx)?;
    // (bash's host_session: its reads of the host are `$(...)`s, sig::subst: an INT to omabox alone
    // meanwhile lets the step finish, then ends it, 130, before anything is focused)
    crate::sig::subst(|| host_session_die(ctx))?;
    if meta(&ctx.d, "mode").0 == b"interactive" {
        // omabox-wlfd connects and execs bwrap, so the host sees the window as the outer bwrap's.
        if focus {
            let pid = match box_pid(&ctx.d) {
                BoxPid::Pid(p) => crate::procfs::proc_ppid(&p).unwrap_or_default(),
                _ => Vec::new(),
            };
            return focus_pid(ctx, &pid);
        }
        say(cat!(
            "box '",
            ctx.name,
            "' is interactive: it is already a window on ",
            ws_text(&meta(&ctx.d, "workspace").0)
        ));
        return Ok(());
    }
    // The screen it shows (#122): the main one, or --monitor's; one peek window each.
    let mons = strip_nl(&crate::sig::subst(|| hypr_json(ctx, &["monitors"]))?);
    let r = crate::sig::subst(|| {
        jq::jq(
            &["-r", &format!("{}.name // empty", main_screen(ctx))],
            Some(&mons),
            jq::Err::Inherit,
        )
    });
    if !r.ok() {
        return Err(crate::exit::Exit(u8::try_from(r.rc).unwrap_or(1)));
    }
    let mut output = r.text();
    if !monitor.is_empty() {
        let has = Args::new(&["-e"])
            .kv("--arg", "n", &monitor)
            .arg("any(.[]; .name == $n and (.disabled | not))")
            .run(Some(&mons), jq::Err::Inherit)
            .ok();
        if !has {
            let names = crate::sig::subst(|| {
                jq::jq(
                    &["-r", r#"map(.name) | join(", ")"#],
                    Some(&mons),
                    jq::Err::Inherit,
                )
            })
            .text();
            return Err(ctx.die(cat!(
                "peek: box '",
                ctx.name,
                "' has no monitor ",
                monitor,
                " (",
                names,
                ")"
            )));
        }
        if monitor == output {
            monitor.clear();
        }
    }
    if !monitor.is_empty() {
        output = monitor.clone();
    }
    if !shell::re_match("^[A-Za-z0-9_-]+$", &output) {
        return Err(ctx.die(cat!(
            "peek: box '",
            ctx.name,
            "' reports no usable output (",
            output,
            ")"
        )));
    }
    if let Some(pid) = crate::sig::subst(|| peek_pid(ctx, Some(&output))) {
        if focus {
            return focus_pid(ctx, &pid);
        }
        let of = if monitor.is_empty() {
            Vec::new()
        } else {
            cat!(" of ", monitor)
        };
        say(cat!("box '", ctx.name, "' already has a peek window", of));
        return Ok(());
    }
    if ws.is_empty() {
        ws = crate::sig::subst(crate::conf::workspace);
    }
    ws_note(&ws);
    let peek = crate::root().join("tools/peek/omabox-peek");
    let peek = shell::bytes(&peek).to_vec();
    if !crate::exec::executable(&peek) {
        return Err(ctx.die(cat!("peek: missing ", peek, ": run install.sh")));
    }
    // peek runs on the host (it is a window there) and connects to the box's socket from the host's
    // side of its runtime dir, which the box can write: only a real socket, no symlink (finding 63;
    // tools/peek also checks every frame it is sent).
    let Some(wd) = crate::sig::subst(|| box_env(&ctx.d, "WAYLAND_DISPLAY")) else {
        return Err(ctx.die(cat!("peek: box '", ctx.name, "' has no WAYLAND_DISPLAY")));
    };
    let sock = cat!(ctx.d, "/run/", wd);
    let is_sock =
        std::fs::symlink_metadata(shell::path(&sock)).is_ok_and(|m| m.file_type().is_socket());
    if !is_sock {
        return Err(ctx.die(cat!("peek: ", sock, " is not the box's Wayland socket")));
    }
    // A marks file (finding 85) only we can read; peek reads what is added to it. Fresh for the
    // box's first peek window, shared by the others (#164: made anew, the windows open lost it).
    let marks = ctx.at("marks");
    if peek_pid(ctx, None).is_none() {
        let _ = std::fs::remove_file(&marks);
    }
    if !marks.is_file() {
        if let Err(e) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&marks)
        {
            // (`(umask 077; : > FILE)`: bash's own line, the subshell's failure ends it)
            // (bash's redirection line, less its "SCRIPT: line N: ")
            crate::shots::say_raw(&cat!(
                shell::bytes(&marks),
                ": ",
                crate::shots::why(&e),
                "\n"
            ));
            return Err(crate::exit::Exit(1));
        }
    }
    let marker = peek_marker(ctx);
    let env = if marker.is_empty() {
        Vec::new()
    } else {
        cat!("env ", marker, " ")
    };
    let mon_t = if monitor.is_empty() {
        Vec::new()
    } else {
        cat!(" · ", monitor)
    };
    // Launched by the host Hyprland: its exec rules put exactly this window on that workspace,
    // unfocused.
    let lua = cat!(
        "hl.exec_cmd('",
        env,
        lua_str(ctx, &peek),
        " --box ",
        lua_str(ctx, &sock),
        " --output ",
        output,
        " --fps ",
        fps,
        " --title \"omabox peek: ",
        ctx.name,
        mon_t,
        "\" --marks ",
        lua_str(ctx, shell::bytes(&marks)),
        "', { workspace = '",
        ws,
        " silent', no_initial_focus = true })"
    );
    // (host_eval's `out=$(host_hyprctl eval ...)`: a substitution's step)
    crate::sig::subst(|| host_eval(ctx, &lua))?;
    say(cat!(
        "peek at box '",
        ctx.name,
        "' on ",
        ws_text(&ws),
        " (view only; close it with SUPER+W, or it goes with the box)"
    ));
    if !focus {
        return Ok(());
    }
    // (bash's `$SECONDS + 5`: whole seconds)
    let deadline = crate::settle::secs() + 5;
    let pid = loop {
        if let Some(pid) = crate::sig::subst(|| peek_pid(ctx, Some(&output))) {
            let (rc, cl) = host_hyprctl(ctx, &[b"-j", b"clients"], false);
            let seen = Args::new(&["-e"])
                .kv("--argjson", "p", &pid)
                .arg("any(.pid == $p)")
                .run(Some(&shell::strip_nl(&cl)), jq::Err::Inherit)
                .ok();
            if rc == 0 && seen {
                break pid;
            }
        }
        if crate::settle::secs() >= deadline {
            return Err(ctx.die(cat!(
                "peek: its window did not come up on your desktop in 5 s, so nothing was focused (omabox peek -b ",
                ctx.name,
                " without --focus opens it on ",
                ws_text(&ws),
                "; omabox ls: is the box up?)"
            )));
        }
        crate::trap::sleep(b"0.05");
    };
    focus_pid(ctx, &pid)
}

/// ws_note WS: one line when WS is the scratchpad on an Omarchy whose scratchpad is its Quake
/// console (4.5: default/hypr/qconsole.lua, told by the host's tree, never its version; #292,
/// finding 423).
pub fn ws_note(ws: &[u8]) {
    if ws != b"special:scratchpad" {
        return;
    }
    let om = shell::env("OMARCHY_PATH")
        .filter(|v| !v.is_empty())
        .unwrap_or(b"/usr/share/omarchy".to_vec());
    if shell::path(&cat!(om, "/default/hypr/qconsole.lua")).is_file() {
        say(
            "note: on this Omarchy the scratchpad is its Quake console: a half-height panel that starts the default agent when it opens empty; special:omabox gives box windows a special workspace of their own, full size",
        );
    }
}

/// Who asked for a peek window, for its process's environment (finding 121), where the suite's
/// leak detector reads it: OMABOX_SUITE=ID for a command the suite ran, OMABOX_PEEK_BY=you for
/// `omabox peek` itself; nothing else.
fn peek_marker(ctx: &Ctx) -> Vec<u8> {
    let suite = shell::env_or_empty("OMABOX_SUITE");
    if !suite.is_empty() {
        if shell::re_match("^[A-Za-z0-9_-]{1,32}$", &suite) {
            return cat!("OMABOX_SUITE=", suite);
        }
        return Vec::new();
    }
    if ctx.main_cmd == b"peek" {
        return b"OMABOX_PEEK_BY=you".to_vec();
    }
    Vec::new()
}

/// focus_pid PID: a host window focused by its pid (Hyprland switches to its workspace).
fn focus_pid(ctx: &Ctx, pid: &[u8]) -> Res<()> {
    if !shell::re_match("^[0-9]+$", pid) {
        let lead = if ctx.main_cmd.is_empty() {
            Vec::new()
        } else {
            cat!(ctx.main_cmd, ": ")
        };
        return Err(ctx.die(cat!(
            lead,
            "no window to focus (the box's window is gone? omabox ls)"
        )));
    }
    crate::sig::subst(|| {
        host_eval(
            ctx,
            &cat!("hl.dispatch(hl.dsp.focus({ window = 'pid:", pid, "' }))"),
        )
    })
}
