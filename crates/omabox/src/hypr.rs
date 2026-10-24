//! The box's Hyprland when a call to it gets no answer: gone (how it ended, #232), held by gdb at a
//! crash, or hung (#124, findings 181-182, 350, 353), in words; and what a reload's config errors are
//! (#246). As bin/omabox's hung_text, hypr_lost, hypr_down_text, hypr_answers, hypr_json and
//! hypr_errors_note.

use crate::boxes::{box_alive, box_env, box_home_file, hypr_end, run_read, tail};
use crate::cat;
use crate::exec::{ErrTo, Out, on_box};
use crate::exit::{Ctx, Res, say};
use crate::jobs::box_pids;
use crate::json;
use crate::procfs;
use crate::shell;
use std::time::Duration;

/// HYPR_LIMIT: how long a hyprctl call is given.
pub const HYPR_LIMIT: &str = "10";

fn words(v: &[&str]) -> Vec<Vec<u8>> {
    v.iter().map(|w| w.as_bytes().to_vec()).collect()
}

/// The host pid of the box's Hyprland: Some(None) when none runs, None when it cannot tell. Its pid
/// as the box numbers it, from its instance lock (a box-written file: only a number taken), matched
/// to the host's through NSpid.
fn hypr_host_pid(ctx: &Ctx) -> Option<Option<Vec<u8>>> {
    let pids = box_pids(ctx);
    if pids.is_empty() {
        return None;
    }
    let is = box_env(&ctx.d, "HYPRLAND_INSTANCE_SIGNATURE")?;
    let is_s = String::from_utf8_lossy(&is).into_owned();
    let link = |rel: &str| ctx.at(rel).is_symlink();
    let lock = if link("run/hypr") || link(&format!("run/hypr/{is_s}")) {
        Vec::new()
    } else {
        run_read(&ctx.d, &format!("hypr/{is_s}/hyprland.lock"))
            .map(|v| v.split(|&c| c == b'\n').next().unwrap_or_default().to_vec())
            .unwrap_or_default()
    };
    if shell::re_match("^[1-9][0-9]{0,6}$", &lock) {
        for p in pids {
            let s = std::fs::read(shell::path(&cat!("/proc/", p, "/status"))).unwrap_or_default();
            if procfs::status_last(&s, "NSpid") == lock && procfs::running(&p) {
                return Some(Some(p));
            }
        }
    }
    Some(None)
}

/// hypr_gone: for `ls`, at one look (no wait): a box up whose Hyprland did not answer because it is
/// gone, in the moment before the box ends with it (finding 350): its end's first word (crashed,
/// exited) when labwc (or an interactive box's session.sh) said how, else going. None for one that
/// runs, or a box it cannot tell of.
pub fn hypr_gone(ctx: &Ctx) -> Option<String> {
    if hypr_host_pid(ctx)?.is_some() {
        return None;
    }
    Some(match hypr_end(&ctx.d) {
        Some(e) => e.split(' ').next().unwrap_or_default().to_string(),
        None => "going".to_string(),
    })
}

/// A process's state as /proc/PID/status says it ("S (sleeping)", "t (tracing stop)").
fn pid_state(pid: &[u8]) -> Vec<u8> {
    let s = std::fs::read(shell::path(&cat!("/proc/", pid, "/status"))).unwrap_or_default();
    let mut out = Vec::new();
    for l in s.split(|&c| c == b'\n') {
        if let Some(r) = l.strip_prefix(b"State:") {
            let i = r
                .iter()
                .position(|c| !c.is_ascii_whitespace())
                .unwrap_or(r.len());
            out.extend_from_slice(&r[i..]);
            out.push(b'\n');
        }
    }
    shell::strip_nl(&out)
}

/// The fatal signal gdb --watch caught in the box's Hyprland, in its log's last watch of it.
pub fn hypr_gdb_sig(ctx: &Ctx) -> Option<String> {
    let f = box_home_file(&ctx.d, "gdb.log")?;
    let text = tail(&f, 1_048_576);
    let (mut w, mut s) = (false, String::new());
    for l in text.split(|&c| c == b'\n') {
        if l.starts_with(b"== omabox gdb --watch ") {
            w = l.starts_with(b"== omabox gdb --watch Hyprland ");
            s.clear();
            continue;
        }
        if w && shell::re_match(
            r"^(Thread [0-9]+ .*|Program) received signal SIG[A-Z0-9]+",
            l,
        ) {
            if let Some(m) = shell::re_group("(SIG[A-Z0-9]+)", l, 1) {
                s = String::from_utf8_lossy(&m).into_owned();
            }
        }
    }
    (!s.is_empty()).then_some(s)
}

/// A box that went down, or is going, in words.
pub fn hypr_down_text(ctx: &Ctx) -> Vec<u8> {
    let n = &ctx.name;
    let Some(end) = hypr_end(&ctx.d) else {
        return cat!(
            "box '",
            n,
            "' went down (why: omabox log -b ",
            n,
            " all; clear it with: omabox down ",
            n,
            ")"
        );
    };
    let hint = if end.starts_with("crashed") {
        if hypr_gdb_sig(ctx).is_some() {
            cat!("its backtrace: omabox log -b ", n, " gdb --crash")
        } else {
            b"a backtrace: the same call in a new box after omabox gdb --watch; boxes keep no core dumps".to_vec()
        }
    } else {
        cat!("why: omabox log -b ", n, " all")
    };
    cat!(
        "box '",
        n,
        "' went down: its Hyprland ",
        end,
        " (",
        hint,
        "; clear it with: omabox down ",
        n,
        ")"
    )
}

/// Whether the box's Hyprland is gone, or held by gdb --watch at a fatal signal, after a call to it
/// got no answer (#232): up to 2 s for the box or labwc's line to say how it ended; one that runs
/// (twice, 0.2 s apart) is not lost. Some(what happened), None for one that runs or cannot be told.
pub fn hypr_lost(ctx: &Ctx) -> Res<Option<Vec<u8>>> {
    let (mut run, mut hp) = (0, None);
    for _ in 0..10 {
        if !box_alive(ctx)? {
            return Ok(Some(hypr_down_text(ctx)));
        }
        let Some(h) = hypr_host_pid(ctx) else {
            return Ok(None);
        };
        hp = h;
        if let Some(p) = &hp {
            let st = pid_state(p);
            // gdb --watch stops it at the signal (tracing stop, t) while it writes the backtraces.
            if st.starts_with(b"t") {
                if let Some(sig) = hypr_gdb_sig(ctx) {
                    let n = &ctx.name;
                    return Ok(Some(cat!(
                        "box '",
                        n,
                        "': its Hyprland crashed (",
                        sig,
                        "), held by gdb --watch while it writes the backtrace (omabox log -b ",
                        n,
                        " gdb --crash; the box goes down after it)"
                    )));
                }
            } else {
                run += 1;
            }
            if run >= 2 {
                return Ok(None);
            }
        } else if hypr_end(&ctx.d).is_some() {
            return Ok(Some(hypr_down_text(ctx)));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if hp.is_some() {
        return Ok(None);
    }
    Ok(Some(hypr_down_text(ctx)))
}

/// A box whose PID 1 lives but whose Hyprland does not answer, in words (IN: how long it was given);
/// one gone meanwhile said so instead.
pub fn hung_text(ctx: &Ctx, within: &str) -> Res<Vec<u8>> {
    if let Some(t) = hypr_lost(ctx)? {
        return Ok(t);
    }
    let n = &ctx.name;
    let within = if within.is_empty() {
        String::new()
    } else {
        format!(" in {within}")
    };
    Ok(cat!(
        "box '",
        n,
        "': its Hyprland did not answer",
        within,
        " (hung? omabox log -b ",
        n,
        "; omabox down ",
        n,
        ")"
    ))
}

/// The box's Hyprland answers over IPC within 2 s. (Never inside `$(...)`: on_box's refusal ends
/// the command, its words to /dev/null with the rest.)
pub fn hypr_answers(ctx: &Ctx) -> Res<bool> {
    hypr_answers_in(ctx, "2")
}

/// hypr_answers SECS: the same, given SECS.
pub fn hypr_answers_in(ctx: &Ctx, secs: &str) -> Res<bool> {
    if !box_alive(ctx)? {
        return Ok(false);
    }
    let r = on_box(
        ctx,
        &words(&["timeout", secs, "hyprctl", "version"]),
        false,
        Out::Null,
        ErrTo::Null,
    );
    Ok(r.or_exit()?.rc == 0)
}

/// `hyprctl -j ARGS` in the box, its answer checked as JSON (#125, finding 182): one that does not
/// answer is said in words and the command ends (at most 64 MiB read).
pub fn hypr_json(ctx: &Ctx, args: &[&str]) -> Res<Vec<u8>> {
    let mut argv = words(&["timeout", "6", "hyprctl", "-j"]);
    argv.extend(words(args));
    let r = on_box(
        ctx,
        &argv,
        false,
        Out::Capture(Some(67_108_864)),
        ErrTo::Null,
    );
    let out = if r.rc == 0 {
        shell::subst(&r.out)
    } else {
        Vec::new()
    };
    let (vals, ok) = json::values(&out);
    if ok && !vals.is_empty() {
        return Ok(cat!(out, "\n"));
    }
    if !box_alive(ctx)? {
        return Err(ctx.die(hypr_down_text(ctx)));
    }
    let t = hung_text(ctx, "")?;
    Err(ctx.die(t))
}

/// The config's errors at its last load (#246), said after `hyprctl reload`, which answers ok
/// whatever they are: control characters dropped, 20 lines and 300 characters a line at most. A
/// Hyprland that does not answer is said: false (finding 353).
pub fn hypr_errors_note(ctx: &Ctx) -> Res<bool> {
    let r = on_box(
        ctx,
        &words(&["timeout", HYPR_LIMIT, "hyprctl", "configerrors"]),
        false,
        Out::Capture(Some(65536)),
        ErrTo::Null,
    );
    let rc = r.rc;
    let t: Vec<u8> = r
        .out
        .iter()
        .copied()
        .filter(|&c| !(c <= 0x08 || (0x0b..=0x1f).contains(&c) || c == 0x7f))
        .collect();
    let t = shell::strip_nl(&t);
    let n = &ctx.name;
    if rc != 0 && rc != 141 {
        if let Some(t) = hypr_lost(ctx)? {
            say(t);
            return Ok(false);
        }
        if hypr_answers(ctx)? {
            say(cat!(
                "box '",
                n,
                "': could not read its config's errors (hyprctl configerrors: exit ",
                rc.to_string(),
                ")"
            ));
            return Ok(true);
        }
        say(cat!(
            "box '",
            n,
            "': its Hyprland did not answer hyprctl configerrors, so the config's errors are unknown ",
            "(hung? its threads: omabox gdb -b ",
            n,
            "; its log: omabox log -b ",
            n,
            "; then omabox down ",
            n,
            ")"
        ));
        return Ok(false);
    }
    let lines: Vec<&[u8]> = t
        .split(|&c| c == b'\n')
        .filter(|l| !l.iter().all(|c| matches!(c, b' ' | b'\t')))
        .take(20)
        .collect();
    if lines.is_empty() {
        return Ok(true);
    }
    say(cat!(
        "box '",
        n,
        "': its config has errors (the rest of the config loaded):"
    ));
    for l in lines {
        say(cat!("  ", shell::head_chars(l, 300)));
    }
    Ok(true)
}
