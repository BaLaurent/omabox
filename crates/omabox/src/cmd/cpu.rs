//! `omabox cpu [-b NAME] [DURATION] [--settle DURATION] [--pid PID | --app NAME] [--threads]
//! [--json]` (#205): CPU and memory of this box's processes, gpu's twin. CPU as % of one core over
//! the window, RSS and PSS as last seen; a box's processes are the ones in its pid namespace
//! (finding 242). utime+stime from /proc/PID/stat, sampled every second of the window (and at its
//! ends); --threads (#242): each thread of the --pid or --app processes too.

use crate::args::{optarg, unknown_opt};
use crate::cat;
use crate::exit::{Ctx, Res, out};
use crate::gpu::{cost_context, cost_lines};
use crate::jobs::{box_pids, box_pids_try};
use crate::jq::{self, Args};
use crate::num::{awk_num, fixed, s_duration};
use crate::select::select_box;
use crate::shell;

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    let (mut name, mut secs, mut settle) = (Vec::new(), b"5".to_vec(), b"0".to_vec());
    let (mut pid, mut app, mut json, mut threads) = (Vec::new(), Vec::new(), false, false);
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
            b"--settle" => {
                optarg(ctx, a, v)?;
                let Some(s) = s_duration(&args[i + 1], true) else {
                    return Err(ctx.die(cat!(
                        "cpu: --settle takes a duration, like 2s or 500ms (a bare number is seconds), got ",
                        args[i + 1]
                    )));
                };
                settle = s.into_bytes();
                i += 1;
            }
            b"--pid" => {
                optarg(ctx, a, v)?;
                if !shell::re_match("^[1-9][0-9]{0,6}$", &args[i + 1]) {
                    return Err(ctx.die(cat!(
                        "cpu: --pid takes a pid in the box (omabox run -- ps -e) or its host pid, got ",
                        args[i + 1]
                    )));
                }
                pid = args[i + 1].clone();
                i += 1;
            }
            b"--app" => {
                optarg(ctx, a, v)?;
                app = args[i + 1].clone();
                i += 1;
            }
            b"--threads" => threads = true,
            b"--json" => json = true,
            b"-h" | b"--help" => return crate::args::usage("cpu"),
            _ if a.starts_with(b"-") => return Err(unknown_opt(ctx, "cpu", a)),
            _ => {
                let Some(s) = s_duration(a, false) else {
                    return Err(ctx.die(cat!(
                        "cpu: how long to measure, like 5s or 500ms (a bare number is seconds), got ",
                        a
                    )));
                };
                secs = s.into_bytes();
            }
        }
        i += 1;
    }
    if !pid.is_empty() && !app.is_empty() {
        return Err(ctx.die("cpu: --pid or --app, not both"));
    }
    // Every process's threads would bury the one app's: Hyprland and the shell have dozens each.
    if threads && pid.is_empty() && app.is_empty() {
        return Err(ctx.die(
            "cpu: --threads is for one app's threads: name it with --pid PID or --app NAME (omabox run -- ps -e)",
        ));
    }
    select_box(ctx, &name)?;
    crate::boxes::need_box(ctx)?;
    if box_pids_try(ctx).is_none() {
        return Err(ctx.die("cpu: cannot read the box's pid namespace"));
    }
    // SAFETY: sysconf only reads a constant.
    let clk = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if clk < 1 {
        return Err(ctx.die("cpu: getconf CLK_TCK gave no tick rate"));
    }
    if settle != b"0" {
        crate::trap::sleep(&settle);
    }
    // (the EXIT trap's rm, also on INT, TERM or HUP)
    let (_, tmp) = shell::mktemp_args(&[b"-d"])?;
    let t = tmp.clone();
    let _trap = crate::trap::on_exit(move || {
        let _ = std::fs::remove_dir_all(shell::path(&t));
    });
    let s = cat!(tmp, "/s");
    // (its first 15 bytes, as the kernel cuts a comm: finding 428)
    let tapp = cpu_tame(&app[..app.len().min(15)]);
    let snap = |into: &mut Vec<u8>| {
        cpu_snap(ctx, threads, &pid, &tapp, into);
    };
    let mut samples = Vec::new();
    snap(&mut samples);
    let _ = std::fs::write(shell::path(&s), &samples);
    // --pid: there before the window, not after it (gone by then, it is a process that exited).
    if !pid.is_empty() {
        let r = jq::tool(
            "awk",
            &[
                b"-v".to_vec(),
                cat!("p=", pid),
                crate::jqprog::CPU_PID_AWK.as_bytes().to_vec(),
                s.clone(),
            ],
            None,
            jq::Err::Inherit,
        );
        if !r.ok() {
            return Err(ctx.die(cat!(
                "cpu: box '",
                ctx.name,
                "' has no process ",
                pid,
                " (omabox run -b ",
                ctx.name,
                " -- ps -e)"
            )));
        }
    }
    let t0: i128 = samples
        .split(|&c| c == b'\n')
        .next()
        .and_then(|l| l.strip_prefix(b"S "))
        .and_then(|v| std::str::from_utf8(v).ok()?.parse().ok())
        .unwrap_or(0);
    let ns: i128 = fixed(awk_num(&secs) * 1e9, 0).parse().unwrap_or(0);
    loop {
        let left = t0 + ns - crate::settle::mono_ns();
        if left <= 0 {
            break;
        }
        let l = (left as f64).min(1e9) / 1e9;
        crate::trap::sleep(fixed(l, 3).as_bytes());
        snap(&mut samples);
        let _ = std::fs::write(shell::path(&s), &samples);
    }
    let r = Args::new(&["-v"])
        .arg(format!("clk={clk}"))
        .arg("-v")
        .arg(cat!("want=", pid))
        .arg("-v")
        .arg(cat!("app=", tapp))
        .arg(crate::jqprog::CPU_REPORT_AWK)
        .into_words();
    let rep = jq::tool("awk", &r, Some(&samples), jq::Err::Inherit).out;
    let _ = std::fs::write(shell::path(&cat!(tmp, "/r")), &rep);
    if !rep.split(|&c| c == b'\n').any(|l| l.starts_with(b"p ")) && !app.is_empty() {
        return Err(ctx.die(cat!(
            "cpu: box '",
            ctx.name,
            "' ran no process ",
            app,
            " in the window (its name in omabox run -b ",
            ctx.name,
            " -- ps -e)"
        )));
    }
    let c = cost_context(ctx)?;
    // SAFETY: sysconf only reads a value.
    let cores = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    let cores = if cores < 0 {
        "null".to_string()
    } else {
        cores.to_string()
    };
    if json {
        let a = Args::new(&["-Rn"])
            .kv("--arg", "box", &ctx.name)
            .kv("--arg", "screen", &c.screen)
            .kv("--argjson", "secs", &secs)
            .kv("--argjson", "settle", &settle)
            .kv("--argjson", "cores", &cores)
            .kv("--argjson", "aq", &c.aq)
            .kv("--argjson", "render", &c.rj)
            .kv("--argjson", "cr", &c.cr)
            .kv("--argjson", "threads", if threads { "1" } else { "0" })
            .arg(crate::jqprog::CPU_JSON_JQ)
            .into_words();
        let r = jq::tool("jq", &a, Some(&rep), jq::Err::Inherit);
        out(&r.out)?;
        return crate::cmd::hyprctl::code(r.rc);
    }
    let (sn, sm) = crate::cmd::mode::screen_parts(&c.screen);
    out(cat!(
        "box '",
        ctx.name,
        "': ",
        sm,
        " on ",
        sn,
        ", ",
        secs,
        "s, CPU as % of one core (",
        cores,
        " cores), memory as last seen\n"
    ))?;
    cost_lines(ctx, &c)?;
    let t = jq::tool(
        "awk",
        &[crate::jqprog::CPU_TABLE_AWK],
        Some(&rep),
        jq::Err::Inherit,
    );
    out(&t.out)?;
    crate::cmd::hyprctl::code(t.rc)
}

/// cpu_tame: a process name as `cpu` writes it, in C's bytes: blanks to _, anything but
/// [A-Za-z0-9_.:+@-] to ?.
pub fn cpu_tame(v: &[u8]) -> Vec<u8> {
    v.iter()
        .map(|&c| {
            if matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
                b'_'
            } else if c.is_ascii_alphanumeric() || b"_.:+@-".contains(&c) {
                c
            } else {
                b'?'
            }
        })
        .collect()
}

/// cpu_snap: "S NANOSECONDS", then cpu_sample's lines; with --threads, after each process its
/// threads of the processes cpu_report keeps (by --pid, either numbering, or by name).
fn cpu_snap(ctx: &Ctx, threads: bool, pid: &[u8], tapp: &[u8], into: &mut Vec<u8>) {
    let pids = box_pids(ctx);
    into.extend_from_slice(format!("S {}\n", crate::settle::mono_ns()).as_bytes());
    let lines = cpu_sample(&pids);
    if !threads {
        for l in &lines {
            into.extend_from_slice(&cat!(l, "\n"));
        }
        return;
    }
    let mut tp: Vec<Vec<u8>> = Vec::new();
    for l in &lines {
        into.extend_from_slice(&cat!(l, "\n"));
        let f = shell::read_fields(l, 7);
        let (hp, bp, nm) = (&f[0], &f[5], &f[6]);
        if !pid.is_empty() {
            if bp == pid || hp == pid {
                tp.push(hp.clone());
            }
        } else if nm == tapp {
            tp.push(hp.clone());
        }
    }
    for l in cpu_threads(&tp) {
        into.extend_from_slice(&cat!(l, "\n"));
    }
}

/// A /proc/PID/stat line's name (between the first `(` and the last `) `) and its fields after it.
fn stat_parts(l: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let comm = match l.iter().position(|&c| c == b'(') {
        Some(i) => &l[i + 1..],
        None => l,
    };
    let comm = match find_last(comm, b") ") {
        Some(i) => &comm[..i],
        None => comm,
    };
    let rest = match find_last(l, b") ") {
        Some(i) => &l[i + 2..],
        None => l,
    };
    let st: Vec<Vec<u8>> = rest
        .split(|c| matches!(c, b' ' | b'\t' | b'\n'))
        .filter(|w| !w.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    (comm.to_vec(), st)
}

fn find_last(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).rposition(|w| w == n)
}

/// `{ read -r l < FILE; }`: the file's first line, None when it cannot be read (or has no newline).
fn first_line(p: &[u8]) -> Option<Vec<u8>> {
    let v = std::fs::read(shell::path(p)).ok()?;
    let i = v.iter().position(|&c| c == b'\n')?;
    Some(v[..i].to_vec())
}

/// `while read -ra f; do ...; done < FILE`: each whole line's words.
fn line_words(p: &[u8]) -> Vec<Vec<Vec<u8>>> {
    let v = std::fs::read(shell::path(p)).unwrap_or_default();
    let mut out = Vec::new();
    let mut rest: &[u8] = &v;
    while let Some(i) = rest.iter().position(|&c| c == b'\n') {
        let l = &rest[..i];
        out.push(
            l.split(|c| matches!(c, b' ' | b'\t'))
                .filter(|w| !w.is_empty())
                .map(<[u8]>::to_vec)
                .collect(),
        );
        rest = &rest[i + 1..];
    }
    out
}

/// `$((A + B))` of two stat fields.
fn ticks(st: &[Vec<u8>]) -> String {
    let a = shell::dec(&st[11]).unwrap_or(0);
    let b = shell::dec(&st[12]).unwrap_or(0);
    a.saturating_add(b).to_string()
}

/// cpu_sample PID...: a line each, "HOSTPID START TICKS RSS PSS BOXPID NAME" (PSS - when
/// smaps_rollup cannot be read); one gone meanwhile is skipped. Read without a fork per process.
fn cpu_sample(pids: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for p in pids {
        let Some(l) = first_line(&cat!("/proc/", p, "/stat")) else {
            continue;
        };
        let (comm, st) = stat_parts(&l);
        if st.len() < 20 {
            continue;
        }
        let (mut rss, mut pss, mut bp) = (b"0".to_vec(), b"-".to_vec(), p.clone());
        for f in line_words(&cat!("/proc/", p, "/status")) {
            match f.first().map(Vec::as_slice) {
                Some(b"VmRSS:") => rss = f.get(1).cloned().unwrap_or_default(),
                Some(b"NSpid:") => bp = f.last().cloned().unwrap_or_default(),
                _ => {}
            }
        }
        for f in line_words(&cat!("/proc/", p, "/smaps_rollup")) {
            if f.first().map(Vec::as_slice) == Some(b"Pss:") {
                pss = f.get(1).cloned().unwrap_or_default();
                break;
            }
        }
        let mut comm = cpu_tame(&comm);
        if comm.is_empty() {
            comm = b"?".to_vec();
        }
        out.push(cat!(
            p,
            " ",
            st[19],
            " ",
            ticks(&st),
            " ",
            rss,
            " ",
            pss,
            " ",
            bp,
            " ",
            comm
        ));
    }
    out
}

/// cpu_threads PID...: "t HOSTPID START TID TSTART TICKS BOXTID NAME" for each thread of each
/// process (#242), in C's order of the task dir.
fn cpu_threads(pids: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for p in pids {
        let Some(l) = first_line(&cat!("/proc/", p, "/stat")) else {
            continue;
        };
        let (_, st) = stat_parts(&l);
        if st.len() < 20 {
            continue;
        }
        let pst = st[19].clone();
        let dir = cat!("/proc/", p, "/task");
        let mut tids: Vec<Vec<u8>> = std::fs::read_dir(shell::path(&dir))
            .map(|rd| {
                rd.flatten()
                    .map(|e| {
                        use std::os::unix::ffi::OsStrExt;
                        e.file_name().as_bytes().to_vec()
                    })
                    .filter(|n| !n.starts_with(b"."))
                    .collect()
            })
            .unwrap_or_default();
        tids.sort();
        for tid in tids {
            let d = cat!(dir, "/", tid);
            let Some(l) = first_line(&cat!(d, "/stat")) else {
                continue;
            };
            let (comm, st) = stat_parts(&l);
            if st.len() < 20 {
                continue;
            }
            let mut bt = tid.clone();
            for f in line_words(&cat!(d, "/status")) {
                if f.first().map(Vec::as_slice) == Some(b"NSpid:") {
                    bt = f.last().cloned().unwrap_or_default();
                    break;
                }
            }
            let mut comm = cpu_tame(&comm);
            if comm.is_empty() {
                comm = b"?".to_vec();
            }
            out.push(cat!(
                "t ",
                p,
                " ",
                pst,
                " ",
                tid,
                " ",
                st[19],
                " ",
                ticks(&st),
                " ",
                bt,
                " ",
                comm
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_tamed_as_cpu_writes_them() {
        assert_eq!(cpu_tame(b"Web Content"), b"Web_Content");
        assert_eq!(cpu_tame("é\tx".as_bytes()), b"??_x");
        assert_eq!(cpu_tame(b"a/b:c+d@e-f.g"), b"a?b:c+d@e-f.g");
    }

    #[test]
    fn a_stat_line_parted() {
        let (c, st) = stat_parts(b"12 (a) b) c) S 1 2 3");
        assert_eq!(c, b"a) b) c");
        assert_eq!(
            st,
            vec![b"S".to_vec(), b"1".to_vec(), b"2".to_vec(), b"3".to_vec()]
        );
    }
}
