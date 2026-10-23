//! `omabox gdb [-b NAME] [--shell | --pid PID] [--watch] [-- GDB ARGS]`: the host's gdb attached to a
//! process in the box (Hyprland by default), in every namespace of the box, its network's too
//! (finding 229): gdb parses the box's ELF and DWARF holding CAP_SYS_PTRACE there only, so it
//! reaches nothing of the host's. Without --watch every thread's backtrace (or the given gdb
//! commands) and gdb's status; --watch leaves it attached in a session of its own in the box, a
//! fatal signal's backtraces into the box's gdb log.

use crate::args::{optarg, unexpected, unknown_opt_hint};
use crate::boxes::{BoxPid, box_pid, covers_held, need_box};
use crate::cat;
use crate::exec::{Relayed, stdio_relay};
use crate::exit::{Ctx, Exit, Res, say};
use crate::select::select_box;
use crate::shell;
use crate::shellnote::{shell_pid, shell_running, shell_state};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Hyprland,
    Shell,
    Pid,
}

impl Target {
    fn word(self) -> &'static [u8] {
        match self {
            Target::Hyprland => b"hyprland",
            Target::Shell => b"shell",
            Target::Pid => b"pid",
        }
    }
}

fn w(v: &[&str]) -> Vec<Vec<u8>> {
    v.iter().map(|s| s.as_bytes().to_vec()).collect()
}

/// `stdio_relay fds_only "" ARGV...` as a foreground step of bash's: its failure to start one is
/// bash's 1 (a fork that failed).
fn relay(argv: &[Vec<u8>], capture: bool, err_null: bool) -> Relayed {
    stdio_relay(argv, capture, err_null).unwrap_or(Relayed {
        rc: 1,
        out: Vec::new(),
    })
}

/// `${what//[^A-Za-z0-9 ._:+@-]/?}`: a process's name as the box set it (prctl: newlines and
/// escapes allowed), each other character a ?; a name with a byte that starts no character is
/// matched byte by byte, as bash falls back to then.
fn tame_comm(v: &[u8]) -> Vec<u8> {
    let ok = |c: u8| c.is_ascii_alphanumeric() || b" ._:+@-".contains(&c);
    shell::chars_map(v, |ch| match ch {
        [c] if ok(*c) => vec![*c],
        _ => b"?".to_vec(),
    })
}

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    let (mut name, mut target, mut pid, mut watch) =
        (Vec::new(), Target::Hyprland, Vec::new(), false);
    let mut extra: Vec<Vec<u8>> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_slice() {
            b"-b" | b"--box" => {
                optarg(ctx, a, args.get(i + 1))?;
                name = args[i + 1].clone();
                i += 1;
            }
            b"--shell" => target = Target::Shell,
            b"--pid" => {
                optarg(ctx, a, args.get(i + 1))?;
                let p = &args[i + 1];
                if !shell::re_match("^[1-9][0-9]{0,6}$", p) {
                    return Err(ctx.die(cat!(
                        "gdb: --pid takes a pid in the box (omabox run -- ps -e), got ",
                        p
                    )));
                }
                target = Target::Pid;
                pid = p.clone();
                i += 1;
            }
            b"--watch" => watch = true,
            b"-h" | b"--help" => return crate::args::usage("gdb"),
            b"--" => {
                extra = args[i + 1..].to_vec();
                break;
            }
            x if x.starts_with(b"-") => {
                return Err(unknown_opt_hint(ctx, "gdb", a, "(gdb's own go after --)"));
            }
            x => {
                return Err(unexpected(
                    ctx,
                    "gdb",
                    x,
                    "(gdb's own arguments go after --)",
                ));
            }
        }
        i += 1;
    }
    if watch && !extra.is_empty() {
        return Err(ctx.die("gdb: --watch runs its own commands; -- ARGS go with a backtrace"));
    }
    // An INT to omabox alone while gdb runs: bash waits for it and goes on (trap.rs).
    crate::trap::int_as_bash();
    select_box(ctx, &name)?;
    need_box(ctx)?;
    if !crate::exec::executable(b"/usr/bin/gdb") {
        return Err(ctx.die("gdb: no /usr/bin/gdb (package gdb); a box runs the host's"));
    }
    // (`bp=$(box_pid)` under set -e: its status 3, a box this user namespace cannot read, ends
    // the command there, unsaid)
    let bp = match box_pid(&ctx.d) {
        BoxPid::Pid(p) => p,
        BoxPid::Unknown => return Err(Exit(3)),
        BoxPid::None => return Err(ctx.die(cat!("box '", ctx.name, "' is not up"))),
    };
    covers_held(ctx)?;
    // Every namespace of the box, its network too (finding 229). A box started before finding 89
    // shares the host's, which is not ours to join.
    let mut ns = w(&["nsenter", "-t"]);
    ns.push(bp.clone());
    ns.extend(w(&["-U", "-m", "-p", "-u", "-i"]));
    let net = |p: &[u8]| std::fs::read_link(shell::path(&cat!("/proc/", p, "/ns/net"))).ok();
    if net(&bp).unwrap_or_default() != net(b"self").unwrap_or_default() {
        ns.push(b"-n".to_vec());
    }
    ns.push(b"--preserve-credentials".to_vec());
    let with = |tail: &[&[u8]]| {
        let mut v = ns.clone();
        v.extend(tail.iter().map(|t| t.to_vec()));
        v
    };
    match target {
        Target::Hyprland => {
            let r = relay(&with(&[b"pgrep", b"-xo", b"Hyprland"]), true, false);
            if r.rc != 0 {
                return Err(ctx.die(cat!("gdb: box '", ctx.name, "' has no Hyprland running")));
            }
            pid = shell::strip_nl(&r.out);
        }
        Target::Shell => {
            let p = shell_pid(ctx).filter(|p| shell_running(ctx, p));
            match p {
                Some(p) => pid = p,
                None => {
                    return Err(ctx.die(cat!(
                        "gdb: box '",
                        ctx.name,
                        "' has no shell running (",
                        shell_state(ctx),
                        ")"
                    )));
                }
            }
        }
        Target::Pid => {
            let d = cat!("/proc/", pid);
            if relay(&with(&[b"test", b"-d", &d]), false, false).rc != 0 {
                return Err(ctx.die(cat!(
                    "gdb: box '",
                    ctx.name,
                    "' has no process ",
                    pid,
                    " (omabox run -b ",
                    ctx.name,
                    " -- ps -e)"
                )));
            }
        }
    }
    let comm = relay(&with(&[b"cat", &cat!("/proc/", pid, "/comm")]), true, true);
    let mut what = if comm.rc == 0 {
        tame_comm(&shell::strip_nl(&comm.out))
    } else {
        target.word().to_vec()
    };
    if what.is_empty() {
        what = target.word().to_vec();
    }
    // A traced process's tracer, 0 for none (`$(tracer_of PID) || tracer=0`).
    let status = cat!("/proc/", pid, "/status");
    let tracer_of = || {
        let r = relay(
            &with(&[b"sed", b"-n", b"s/^TracerPid:[[:space:]]*//p", &status]),
            true,
            true,
        );
        if r.rc == 0 {
            shell::strip_nl(&r.out)
        } else {
            b"0".to_vec()
        }
    };
    let traced = |t: &[u8]| !t.is_empty() && t != b"0";
    let tracer = tracer_of();
    if traced(&tracer) {
        return Err(ctx.die(cat!(
            "gdb: ",
            what,
            " (pid ",
            pid,
            " in box '",
            ctx.name,
            "') is traced already, by pid ",
            tracer,
            " (a gdb --watch?): omabox run -b ",
            ctx.name,
            " -- kill ",
            tracer,
            " ends that"
        )));
    }
    let mut caps = ns.clone();
    caps.extend(w(&[
        "--keep-caps",
        "--",
        "setpriv",
        "--inh-caps=-all,+sys_ptrace",
        "--ambient-caps=-all,+sys_ptrace",
        "--bounding-set=-all,+sys_ptrace",
        "--",
    ]));
    let lang = shell::env("LANG")
        .filter(|l| !l.is_empty())
        .unwrap_or(b"C.UTF-8".to_vec());
    let mut gdb = w(&["env", "-i", "PATH=/usr/bin", "HOME=/home/sbx"]);
    gdb.push(cat!("LANG=", lang));
    gdb.extend(w(&[
        "gdb",
        "-nx",
        "-batch",
        "-iex",
        "set debuginfod enabled off",
        "-iex",
        "set pagination off",
        "-iex",
        "set width 0",
        "-iex",
        "set print thread-events off",
    ]));
    if !watch {
        if extra.is_empty() {
            extra = w(&["-ex", "thread apply all bt"]);
        }
        let mut argv = w(&["timeout", "120"]);
        argv.extend(caps);
        argv.extend(gdb);
        argv.push(b"-p".to_vec());
        argv.push(pid.clone());
        argv.extend(extra);
        let rc = relay(&argv, false, false).rc;
        if rc == 124 {
            return Err(ctx.die(cat!(
                "gdb: no answer in 120 s: the process may be hung in a way gdb cannot stop (omabox log -b ",
                ctx.name,
                " hyprland; another process: --pid PID; omabox down ",
                ctx.name,
                " ends it)"
            )));
        }
        return crate::cmd::hyprctl::code(rc);
    }
    // Watched: in a session of its own in the box (it ends with the box), into the gdb log, which is
    // written from inside the box, its header too (finding 229): a link there resolves to the box's
    // own files; a link or FIFO is replaced (log reads only a regular file, finding 107; a FIFO would
    // hang this write, 10 s at most).
    let log = b"/home/sbx/gdb.log".to_vec();
    let date = crate::sig::command("date")
        .arg("-Is")
        .stdin(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .output()
        .map(|o| shell::strip_nl(&o.stdout))
        .unwrap_or_default();
    let head = cat!(
        "\n== omabox gdb --watch ",
        what,
        " (pid ",
        pid,
        " in the box), ",
        date
    );
    let mut argv = w(&["timeout", "10"]);
    argv.extend(ns.clone());
    argv.extend(w(&["sh", "-c", GDB_LOG_SH, "sh"]));
    argv.push(log.clone());
    argv.push(head);
    if relay(&argv, false, false).rc != 0 {
        return Err(ctx.die("gdb: cannot write the box's gdb log"));
    }
    let mut argv = caps;
    argv.extend(w(&["setsid", "-f", "sh", "-c", GDB_RUN_SH, "sh"]));
    argv.push(log);
    argv.extend(gdb);
    argv.push(b"-p".to_vec());
    argv.push(pid.clone());
    argv.extend(w(&[
        "-ex",
        "handle all nostop noprint pass",
        "-ex",
        "handle SIGSEGV SIGABRT SIGBUS SIGFPE SIGILL stop print pass",
        "-ex",
        "continue",
        "-ex",
        "echo \\n== omabox: a fatal signal; the thread it hit:\\n",
        "-ex",
        "thread",
        "-ex",
        "bt",
        "-ex",
        "echo \\n== omabox: every thread:\\n",
        "-ex",
        "thread apply all bt",
        "-ex",
        "echo == omabox: end\\n",
    ]));
    // (set -e: a failure here ends the command with its status, unsaid)
    let rc = relay(&argv, false, false).rc;
    if rc != 0 {
        return Err(Exit(u8::try_from(rc & 0xff).unwrap_or(1)));
    }
    let end = crate::settle::secs() + 10;
    loop {
        if traced(&tracer_of()) {
            break;
        }
        if crate::settle::secs() >= end {
            return Err(ctx.die(cat!(
                "gdb: did not attach to ",
                what,
                " in 10 s (omabox log -b ",
                ctx.name,
                " gdb)"
            )));
        }
        crate::trap::sleep(b"0.1");
    }
    say(cat!(
        "gdb watches ",
        what,
        " (pid ",
        pid,
        " in box '",
        ctx.name,
        "'): on a crash, every thread's backtrace goes to omabox log -b ",
        ctx.name,
        " gdb (--crash: the thread it hit)"
    ));
    Ok(())
}

/// The gdb log's header, written in the box (bin/omabox's, byte for byte).
pub const GDB_LOG_SH: &str =
    r#"{ [ ! -L "$1" ] && [ ! -p "$1" ]; } || rm -f -- "$1"; printf "%s\n" "$2" >> "$1""#;
/// gdb --watch's start in its session (bin/omabox's, byte for byte).
pub const GDB_RUN_SH: &str = r#"log=$1; shift; exec "$@" >> "$log" 2>&1 </dev/null"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_tamed() {
        assert_eq!(tame_comm(b"Hyprland"), b"Hyprland");
        assert_eq!(tame_comm(b"a b.c_d:e+f@g-h"), b"a b.c_d:e+f@g-h");
        assert_eq!(tame_comm(b"x\ny\x1b"), b"x?y?");
        assert_eq!(tame_comm(b""), b"");
    }
}
