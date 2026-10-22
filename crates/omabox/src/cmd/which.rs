//! `omabox which PID` (#172): the box a host process is in. A box's shell has the desktop's command
//! line (/usr/bin/quickshell -n -p /usr/share/omarchy/shell), so a host pgrep shows two "desktop
//! shells" while a box runs. By pid namespace: the process's, or the first readable one of its
//! ancestors' (a sandbox in the box, Chromium's or a nested box, has a pid namespace of its own),
//! against each live box's (box_pid checks its recorded `pidns`). The walk stops at this omabox's
//! own pid namespace: the host's. Then OMABOX_BOX in its environment, which `up` gives the whole
//! session (a box this omabox does not list: another state dir or user namespace), said as such.
//! Exit 0 in a box, 1 not in one, 2 cannot tell.

use crate::args::unknown_opt;
use crate::boxes::{BoxPid, box_pid, list_names};
use crate::cat;
use crate::exit::{Ctx, Exit, Res, out, say};
use crate::json;
use crate::procfs;
use crate::select::select_box;
use crate::shell;
use serde_json::{Map, Value};

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    // --json (#273, audit J4): {pid, box (null: not in one), by: pidns or env}; the exit codes as
    // without.
    let mut as_json = false;
    let mut words = Vec::new();
    for a in args {
        if a == b"--json" {
            as_json = true;
        } else if a.starts_with(b"-") {
            return Err(unknown_opt(ctx, "which", a));
        } else {
            words.push(a.clone());
        }
    }
    let [pid] = words.as_slice() else {
        return Err(ctx.usage_die("which: one PID (omabox which [--json] PID)"));
    };
    if !shell::re_match("^[0-9]+$", pid) {
        return Err(ctx.usage_die("which: one PID (omabox which [--json] PID)"));
    }
    ctx.parsed();
    let pid = pid.clone();
    // The answer: a box's name or "not in a box".
    let answer = |b: &[u8], by: &str| -> Res<()> {
        if as_json {
            let mut m = Map::new();
            let p = serde_json::from_slice::<Value>(&pid).unwrap_or(Value::Null);
            m.insert("pid".into(), p);
            let opt = |s: String| {
                if s.is_empty() {
                    Value::Null
                } else {
                    Value::String(s)
                }
            };
            m.insert("box".into(), opt(String::from_utf8_lossy(b).into_owned()));
            m.insert("by".into(), opt(by.to_string()));
            out(cat!(json::compact(&Value::Object(m)), "\n"))
        } else if b.is_empty() {
            out("not in a box\n")
        } else {
            out(cat!(b, "\n"))
        }
    };
    let uid = procfs::status(&pid, "Uid", 2).filter(|u| !u.is_empty());
    let Some(uid) = uid else {
        say(cat!(
            "which: no process ",
            pid,
            " (gone, or not in this pid namespace)"
        ));
        return Err(Exit(2));
    };
    // A box runs as the user who started it (its user namespace maps that uid alone), so another
    // user's process is in none of theirs; its namespaces are not ours to read anyway.
    if uid != procfs::euid().to_string().as_bytes() {
        answer(b"", "")?;
        let u = String::from_utf8_lossy(&uid).into_owned();
        let who = u
            .parse::<u32>()
            .ok()
            .and_then(procfs::user_name)
            .unwrap_or_else(|| format!("uid {u}"));
        say(cat!(
            "which: process ",
            pid,
            " is ",
            who,
            "'s; boxes run as the user who started them"
        ));
        return Err(Exit(1));
    }
    let mut boxes: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for n in shell::words(&list_names(ctx, false).join(&b'\n')) {
        select_box(ctx, &n)?;
        let BoxPid::Pid(bpid) = box_pid(&ctx.d) else {
            continue;
        };
        if !procfs::running(&bpid) {
            continue;
        }
        if let Some(ns) = procfs::pidns(&bpid) {
            boxes.push((n, ns));
        }
    }
    let me = procfs::pidns(b"self").unwrap_or_default();
    let mut p = pid.clone();
    while shell::test_int(&p).is_some_and(|n| n > 0) {
        if let Some(ns) = procfs::pidns(&p) {
            if let Some((n, _)) = boxes.iter().find(|(_, b)| *b == ns) {
                answer(n, "pidns")?;
                return Ok(());
            }
            if ns == me {
                break;
            }
        }
        p = procfs::status(&p, "PPid", 2).unwrap_or_default();
    }
    // Only a name a box can have: the environment is the process's own to set.
    let environ = std::fs::read(shell::path(&cat!("/proc/", pid, "/environ"))).unwrap_or_default();
    let n = environ
        .split(|&c| c == 0 || c == b'\n')
        .find_map(|l| l.strip_prefix(b"OMABOX_BOX="))
        .unwrap_or_default();
    if shell::re_match("^[A-Za-z0-9][A-Za-z0-9_.-]*$", n) {
        answer(n, "env")?;
        say(
            "which: by its environment (OMABOX_BOX) only: no box this omabox lists has its pid namespace",
        );
        return Ok(());
    }
    if !shell::path(&cat!("/proc/", pid)).is_dir() {
        say(cat!("which: process ", pid, " is gone"));
        return Err(Exit(2));
    }
    answer(b"", "")?;
    Err(Exit(1))
}
