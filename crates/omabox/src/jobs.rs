//! `run -d` jobs, as far as need_box says their early ends (job_note).

use crate::boxes::{BoxPid, at, box_pid, run_read};
use crate::cat;
use crate::exit::{Ctx, say};
use crate::json;
use crate::procfs;
use crate::shell::{self, Re, re_group};
use serde_json::Value;
use std::io::Write;

/// A `run -d` job that ended within its first JOB_EARLY_MS, said by the box's next command, once
/// (#286, finding 399): run -d's own watch (0.3 s) misses a server that dies a second later. Each
/// job's waiter (share/run-job.sh) writes its status as it ends to omabox.run-NS.exit in the box's
/// runtime dir ($D/run here), NS its start (its log is run-NS.log). $D/jobs-noted (host-only) names
/// those dealt with: said here, said by run -d itself, stopped by --replace, ended with status 0 (a
/// one-shot finished; no death), or later than that. A box an older omabox started has no such
/// file: what is there by then is taken as dealt with. Said whatever -q.
const JOB_EARLY_MS: i64 = 30000;
/// A job killed while an `omabox run` ran in the box, or this long after its end, was stopped by
/// the agent itself (#300, finding 406).
const JOB_RUN_GRACE_MS: i64 = 2000;

fn job_noted(ctx: &Ctx, names: &[Vec<u8>]) {
    if names.is_empty() || !shell::path(&ctx.d).is_dir() {
        return;
    }
    let lines: Vec<u8> = names.iter().flat_map(|n| cat!(n, "\n")).collect();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(ctx.at("jobs-noted"))
    {
        let _ = f.write_all(&lines);
    }
}

/// The lines `while IFS= read -r line` reads: a last one without its newline is not one.
fn read_lines(v: &[u8]) -> Vec<&[u8]> {
    let mut l: Vec<&[u8]> = v.split(|&c| c == b'\n').collect();
    l.pop();
    l
}

fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// `stat -c %.3Y`, as ms: the mtime, truncated to the ms.
fn mtime_ms(p: &std::path::Path) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).ok()?;
    (m.mtime() >= 0).then(|| m.mtime() * 1000 + m.mtime_nsec() / 1_000_000)
}

pub fn job_note(ctx: &Ctx) {
    let run = ctx.at("run");
    let ends: Vec<Vec<u8>> = shell::glob(&run, false)
        .into_iter()
        .filter(|n| n.starts_with(b"omabox.run-") && n.ends_with(b".exit"))
        .collect();
    if ends.is_empty() {
        return;
    }
    let noted = std::fs::read(ctx.at("jobs-noted"));
    let old = !ctx.at("jobs-noted").is_file();
    let seen: Vec<&[u8]> = match (&noted, old) {
        (Ok(v), false) => read_lines(v),
        _ => Vec::new(),
    };
    let fresh: Vec<Vec<u8>> = ends
        .into_iter()
        .filter(|n| !seen.contains(&n.as_slice()))
        .collect();
    if fresh.is_empty() {
        return;
    }
    if old {
        job_noted(ctx, &fresh);
        return;
    }
    let now = now_us();
    let newest = shell::glob(&ctx.at("home"), false)
        .into_iter()
        .rev()
        .find(|l| shell::re_match(r"^run-[0-9]+\.log$", l));
    let re_exit = Re::new(r"^omabox\.run-([0-9]{1,19})\.exit$");
    for n in fresh {
        let Some(ns) = re_exit.as_ref().and_then(|r| r.group(&n, 1)) else {
            job_noted(ctx, &[n]);
            continue;
        };
        // Its end: the status file's time (a link or anything but a file is not the waiter's).
        let f = at(&ctx.d, &format!("run/{}", String::from_utf8_lossy(&n)));
        let end = if f.is_symlink() || !f.is_file() {
            None
        } else {
            mtime_ms(&f)
        };
        let Some(end) = end else {
            job_noted(ctx, &[n]);
            continue;
        };
        let ms = end.wrapping_sub(shell::arith(&ns).unwrap_or(0) / 1_000_000);
        if !(0..=JOB_EARLY_MS).contains(&ms) {
            job_noted(ctx, &[n]);
            continue;
        }
        let name = String::from_utf8_lossy(&n).into_owned();
        let mut st = run_read(&ctx.d, &name).unwrap_or_default();
        st.truncate(64);
        let st = shell::strip_nl(&st);
        let st = if shell::re_match("^[0-9]+$", &st) {
            st
        } else {
            Vec::new()
        };
        // Made, not yet written (the waiter's `echo $? >`): the next command reads it; one still
        // empty 2 s on was not kept.
        if st.is_empty() && now / 1000 - end < 2000 {
            continue;
        }
        job_noted(ctx, std::slice::from_ref(&n));
        if st == b"0" {
            continue;
        }
        // Stopped by the agent itself (#300, finding 406): killed while an `omabox run` ran in the
        // box (`omabox run -- pkill APP`), or just after. An exit or a crash is still said.
        if matches!(st.as_slice(), b"130" | b"137" | b"143") && job_run_stopped(ctx, end) {
            continue;
        }
        // What it was: its record ($D/jobs, the host's), kept until now (finding 405). Found by its
        // log's name, each record a line read on its own. A leader that exited leaving its session
        // running (a launcher) is no end.
        let mut words = Vec::new();
        let mut pid = Vec::new();
        if let Some(rec) = job_record(ctx, &cat!("/run-", ns, ".log")) {
            let (file, rest) = split_tab(&rec);
            let (p, w) = split_tab(rest);
            if !job_live(ctx, &[shell::path(file)]).is_empty() {
                continue;
            }
            pid = p.to_vec();
            words = w.to_vec();
            let text = String::from_utf8_lossy(&words).into_owned();
            if text.chars().count() > 60 {
                words = format!("{}…", text.chars().take(59).collect::<String>()).into_bytes();
            }
        }
        let log = cat!("run-", ns, ".log");
        let wher = if newest.as_deref() == Some(log.as_slice()) {
            cat!("omabox log -b ", ctx.name, " run")
        } else {
            cat!("log: ", ctx.d, "/home/", log)
        };
        let words = if words.is_empty() {
            b"(its words were not kept)".to_vec()
        } else {
            words
        };
        let pid = if pid.is_empty() {
            Vec::new()
        } else {
            cat!(" (pid ", pid, ")")
        };
        say(cat!(
            "warning: run -d job ",
            words,
            pid,
            " ",
            job_how(&st),
            format!(" after {}.{}s", ms / 1000, ms % 1000 / 100),
            ", since the last command (",
            wher,
            ")"
        ));
    }
}

/// `${rec%%$'\t'*}` and `${rec#*$'\t'}`.
fn split_tab(v: &[u8]) -> (&[u8], &[u8]) {
    match v.iter().position(|&c| c == b'\t') {
        Some(i) => (&v[..i], &v[i + 1..]),
        None => (v, v),
    }
}

/// The first record in $D/jobs (a JSON object a line) whose log ends with LOG, as
/// "FILE<TAB>PID<TAB>ARGV" (jq's `\(.pid)`, `.argv | join(" ")`), up to its first newline.
fn job_record(ctx: &Ctx, log: &[u8]) -> Option<Vec<u8>> {
    let dir = ctx.at("jobs");
    let files = shell::glob(&dir, false);
    if files.is_empty() {
        return None;
    }
    for name in files {
        let file = cat!(ctx.d, "/jobs/", name);
        let Ok(text) = std::fs::read(shell::path(&file)) else {
            continue;
        };
        let mut lines: Vec<&[u8]> = text.split(|&c| c == b'\n').collect();
        if lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        for line in lines {
            let (vals, ok) = json::values(line);
            let [v] = vals.as_slice() else { continue };
            if !ok {
                continue;
            }
            let Value::Object(m) = v else { continue };
            let Some(Value::String(l)) = m.get("log") else {
                continue;
            };
            if !l.as_bytes().ends_with(log) {
                continue;
            }
            let pid = match m.get("pid") {
                Some(Value::String(s)) => s.clone(),
                Some(x) => json::compact(x),
                None => "null".to_string(),
            };
            let Some(argv) = join_argv(m.get("argv")) else {
                continue;
            };
            let rec = cat!(file, "\t", pid, "\t", argv);
            let first = rec.split(|&c| c == b'\n').next().unwrap_or_default();
            return Some(first.to_vec());
        }
    }
    None
}

/// `.argv | join(" ")`, None where jq fails.
fn join_argv(v: Option<&Value>) -> Option<String> {
    let Value::Array(a) = v? else { return None };
    let parts: Option<Vec<String>> = a
        .iter()
        .map(|x| match x {
            Value::String(s) => Some(s.clone()),
            Value::Null => Some(String::new()),
            Value::Number(_) | Value::Bool(_) => Some(json::compact(x)),
            _ => None,
        })
        .collect();
    Some(parts?.join(" "))
}

/// How a job ended, in words, from its exit status.
fn job_how(st: &[u8]) -> String {
    let s = String::from_utf8_lossy(st);
    if st.is_empty() {
        return "exited (its exit status was not kept)".to_string();
    }
    if let Some(n) = shell::test_int(st).filter(|&n| n > 128) {
        if let Some(sig) = procfs::signame((n - 128).to_string().as_bytes()) {
            return format!("was killed by SIG{sig} ({s})");
        }
    }
    format!("exited with status {s}")
}

/// Whether a job that ended at END (ms) did so while an `omabox run` ran in the box, or within
/// JOB_RUN_GRACE_MS of one's end. $D/runs (host-only) holds o.PID.START while one runs and
/// c.START.END once it ended.
fn job_run_stopped(ctx: &Ctx, end: i64) -> bool {
    let num = |v: &[u8]| shell::test_int(v);
    for f in shell::glob(&ctx.at("runs"), false) {
        if let (Some(a), Some(b)) = (
            re_group(r"^c\.([0-9]+)\.([0-9]+)$", &f, 1),
            re_group(r"^c\.([0-9]+)\.([0-9]+)$", &f, 2),
        ) {
            if num(&a).is_some_and(|a| a <= end)
                && num(&b).is_some_and(|b| end <= b + JOB_RUN_GRACE_MS)
            {
                return true;
            }
        } else if let (Some(p), Some(s)) = (
            re_group(r"^o\.([0-9]+)\.([0-9]+)$", &f, 1),
            re_group(r"^o\.([0-9]+)\.([0-9]+)$", &f, 2),
        ) {
            if num(&s).is_some_and(|s| s <= end) && shell::path(&cat!("/proc/", p)).is_dir() {
                return true;
            }
        }
    }
    false
}

/// The box's processes, as host pids: everything in its pid namespace.
fn box_pids(ctx: &Ctx) -> Vec<Vec<u8>> {
    let pid = match box_pid(&ctx.d) {
        BoxPid::Pid(p) => p,
        _ => Vec::new(),
    };
    let Some(ns) = procfs::pidns(&pid) else {
        return Vec::new();
    };
    shell::glob(std::path::Path::new("/proc"), false)
        .into_iter()
        .filter(|p| p.iter().all(u8::is_ascii_digit))
        .filter(|p| procfs::pidns(p).is_some_and(|n| n == ns))
        .collect()
}

/// Each live process of the box: "HOST_PID BOX_PID BOX_SID START".
fn box_procs(ctx: &Ctx) -> Vec<[Vec<u8>; 4]> {
    let mut out = Vec::new();
    for p in box_pids(ctx) {
        let Ok(s) = std::fs::read(shell::path(&cat!("/proc/", p, "/status"))) else {
            continue;
        };
        let state = s
            .split(|&c| c == b'\n')
            .filter(|l| l.starts_with(b"State:"))
            .filter_map(|l| shell::words(l).into_iter().nth(1))
            .next_back()
            .unwrap_or_default();
        let ids = cat!(
            state,
            " ",
            procfs::status_last(&s, "NSpid"),
            " ",
            procfs::status_last(&s, "NSsid")
        );
        let re = r"^[A-Za-z] ([0-9]+) ([0-9]+)$";
        let (Some(bp), Some(sid)) = (re_group(re, &ids, 1), re_group(re, &ids, 2)) else {
            continue;
        };
        if state == b"Z" {
            continue;
        }
        out.push([
            p.clone(),
            bp,
            sid,
            procfs::proc_start(&p).unwrap_or_default(),
        ]);
    }
    out
}

/// awk's `==`: as numbers when both look like numbers, else as strings.
fn awk_eq(a: &[u8], b: &[u8]) -> bool {
    let num = |v: &[u8]| {
        std::str::from_utf8(v)
            .ok()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<f64>().ok())
    };
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Of box_procs', job PID's, as (HOST_PID, BOX_PID): its session's, and its leader by its pid before
/// it has made that session (finding 405). START is its leader's start time, - when the leader had
/// exited before the job was recorded: a leader that started at another time (or at all, for -) has
/// the pid after the job ended, and the job has none.
fn job_procs(procs: &[[Vec<u8>; 4]], pid: &[u8], start: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for [host, bp, sid, st] in procs {
        if awk_eq(sid, pid) || awk_eq(bp, pid) {
            if awk_eq(bp, pid) && !awk_eq(st, start) {
                return Vec::new();
            }
            out.push((host.clone(), bp.clone()));
        }
    }
    out
}

/// The processes of the jobs recorded in FILES. A record whose job has ended is removed once its end
/// has been dealt with (job_said): until then job_note needs it to name what ended (finding 405).
fn job_live(ctx: &Ctx, files: &[std::path::PathBuf]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let procs = box_procs(ctx);
    let mut out = Vec::new();
    for f in files {
        let fields = std::fs::read(f).ok().and_then(|t| record_fields(&t));
        let Some([pid, start, log]) = fields else {
            let _ = std::fs::remove_file(f);
            continue;
        };
        let p = job_procs(&procs, &pid, &start);
        if !p.is_empty() {
            out.extend(p);
        } else if job_said(ctx, &log) {
            let _ = std::fs::remove_file(f);
        }
    }
    out
}

/// `{ read -r pid; read -r start; read -r log; } < <(jq -r '.pid, .start, .log' FILE)`.
fn record_fields(text: &[u8]) -> Option<[Vec<u8>; 3]> {
    let (vals, _) = json::values(text);
    let mut lines = Vec::new();
    for v in &vals {
        let Value::Object(m) = v else {
            if v.is_null() {
                lines.extend(["null"; 3].map(String::from));
                continue;
            }
            break;
        };
        for k in ["pid", "start", "log"] {
            lines.push(json::raw(m.get(k).unwrap_or(&Value::Null)));
        }
    }
    let all: Vec<u8> = lines.iter().flat_map(|l| cat!(l, "\n")).collect();
    let mut it = all.split(|&c| c == b'\n');
    let mut take = || it.next().map(ifs_trim);
    let (a, b, c) = (take()?, take()?, take()?);
    // The third read needs its line's newline: jq always ends with one; one missing is no line.
    (all.iter().filter(|&&c| c == b'\n').count() >= 3).then_some([a, b, c])
}

/// What `read` keeps of a line: IFS's blanks at either end gone.
fn ifs_trim(v: &[u8]) -> Vec<u8> {
    let blank = |c: &u8| matches!(c, b' ' | b'\t' | b'\n');
    let a = v.iter().position(|c| !blank(c)).unwrap_or(v.len());
    let b = v.iter().rposition(|c| !blank(c)).map_or(a, |i| i + 1);
    v[a..b].to_vec()
}

/// Whether the end of the job whose log is LOG has been dealt with ($D/jobs-noted): said, or not to
/// be said. A box an older omabox started (no jobs-noted) and a log not named as run -d names them:
/// yes.
fn job_said(ctx: &Ctx, log: &[u8]) -> bool {
    let noted = ctx.at("jobs-noted");
    let base = log.rsplit(|&c| c == b'/').next().unwrap_or(log);
    let Some(ns) = re_group(r"^run-([0-9]{1,19})\.log$", base, 1) else {
        return true;
    };
    if !noted.is_file() {
        return true;
    }
    let want = cat!("omabox.run-", ns, ".exit");
    std::fs::read(noted).is_ok_and(|t| t.split(|&c| c == b'\n').any(|l| l == want.as_slice()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(a: &str) -> [Vec<u8>; 4] {
        let w: Vec<Vec<u8>> = a.split(' ').map(|s| s.as_bytes().to_vec()).collect();
        [w[0].clone(), w[1].clone(), w[2].clone(), w[3].clone()]
    }

    // As t_unit_cli's job_procs checks.
    #[test]
    fn job_procs_as_bash() {
        let ps = vec![
            line("100 7 7 500"),
            line("101 8 7 510"),
            line("102 9 9 520"),
        ];
        let hp = |v: Vec<(Vec<u8>, Vec<u8>)>| {
            v.into_iter()
                .map(|(a, b)| {
                    format!(
                        "{} {}",
                        String::from_utf8_lossy(&a),
                        String::from_utf8_lossy(&b)
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(hp(job_procs(&ps, b"7", b"500")), vec!["100 7", "101 8"]);
        assert!(job_procs(&ps, b"7", b"499").is_empty());
        assert_eq!(
            hp(job_procs(&[line("101 8 7 510")], b"7", b"-")),
            vec!["101 8"]
        );
        assert!(job_procs(&ps, b"7", b"-").is_empty());
        assert_eq!(
            hp(job_procs(&[line("103 7 6 500")], b"7", b"500")),
            vec!["103 7"]
        );
        assert!(job_procs(&[line("103 7 6 499")], b"7", b"500").is_empty());
    }

    #[test]
    fn how_it_ended() {
        assert_eq!(job_how(b""), "exited (its exit status was not kept)");
        assert_eq!(job_how(b"4"), "exited with status 4");
        assert_eq!(job_how(b"143"), "was killed by SIGTERM (143)");
        assert_eq!(job_how(b"160"), "was killed by SIG (160)");
        assert_eq!(job_how(b"300"), "was killed by SIGRTMIN+10 (300)");
    }

    #[test]
    fn records() {
        assert_eq!(
            record_fields(br#"{"pid":"12","start":"5","log":"/home/sbx/run-1.log"}"#),
            Some([
                b"12".to_vec(),
                b"5".to_vec(),
                b"/home/sbx/run-1.log".to_vec()
            ])
        );
        assert_eq!(record_fields(b"x"), None);
        assert_eq!(
            join_argv(Some(&serde_json::json!(["a", 1, null, true]))),
            Some("a 1  true".into())
        );
        assert_eq!(join_argv(Some(&serde_json::json!(["a", []]))), None);
    }
}
