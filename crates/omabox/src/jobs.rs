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

pub fn job_noted(ctx: &Ctx, names: &[Vec<u8>]) {
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
pub fn read_lines(v: &[u8]) -> Vec<&[u8]> {
    let mut l: Vec<&[u8]> = v.split(|&c| c == b'\n').collect();
    l.pop();
    l
}

pub fn now_us() -> i64 {
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
        let ms = end.saturating_sub(shell::dec(&ns).unwrap_or(0) / 1_000_000);
        if !(0..=JOB_EARLY_MS).contains(&ms) {
            job_noted(ctx, &[n]);
            continue;
        }
        let name = String::from_utf8_lossy(&n).into_owned();
        let mut st = run_read(&ctx.d, &name).unwrap_or_default();
        st.truncate(64);
        let st = shell::strip_nl(&st);
        let st = if shell::is_num(&st) { st } else { Vec::new() };
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
        if let Some(rec) = record_by_log(ctx, &cat!("/run-", ns, ".log")) {
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
pub fn split_tab(v: &[u8]) -> (&[u8], &[u8]) {
    match v.iter().position(|&c| c == b'\t') {
        Some(i) => (&v[..i], &v[i + 1..]),
        None => (v, v),
    }
}

/// The first record in $D/jobs (a JSON object a line) whose log ends with LOG, as
/// "FILE<TAB>PID<TAB>ARGV" (jq's `\(.pid)`, `.argv | join(" ")`), up to its first newline.
fn record_by_log(ctx: &Ctx, log: &[u8]) -> Option<Vec<u8>> {
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
pub fn job_how(st: &[u8]) -> String {
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
pub fn box_pids(ctx: &Ctx) -> Vec<Vec<u8>> {
    box_pids_try(ctx).unwrap_or_default()
}

/// box_pids, None where bash's fails (the box's pid namespace cannot be read).
pub fn box_pids_try(ctx: &Ctx) -> Option<Vec<Vec<u8>>> {
    let pid = match box_pid(&ctx.d) {
        BoxPid::Pid(p) => p,
        _ => Vec::new(),
    };
    let ns = procfs::pidns(&pid)?;
    Some(
        shell::glob(std::path::Path::new("/proc"), false)
            .into_iter()
            .filter(|p| p.iter().all(u8::is_ascii_digit))
            .filter(|p| procfs::pidns(p).is_some_and(|n| n == ns))
            .collect(),
    )
}

/// Each live process of the box: "HOST_PID BOX_PID BOX_SID START".
pub fn box_procs(ctx: &Ctx) -> Vec<[Vec<u8>; 4]> {
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
pub fn job_procs(procs: &[[Vec<u8>; 4]], pid: &[u8], start: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
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

/// job_failed PID (#297, finding 404): run -d --wait's job has died, killed or with a status other
/// than 0 (its waiter's status file, read here in $D/run), nothing of its session left. A 0 is no
/// death: a one-shot changes the screen and ends with 0.
pub fn job_failed(ctx: &Ctx, pid: &[u8]) -> bool {
    if !shell::is_num(pid) {
        return false;
    }
    let f = ctx.at("jobs").join(shell::path(pid));
    if !f.is_file() {
        return false;
    }
    let (log, ok) = crate::json::field(&f, "log");
    if !ok {
        return false;
    }
    let base = log.rsplit(|&c| c == b'/').next().unwrap_or(&log).to_vec();
    let Some(ns) = shell::re_group(r"^run-([0-9]{1,19})\.log$", &base, 1) else {
        return false;
    };
    let st = run_read(
        &ctx.d,
        &format!("omabox.run-{}.exit", String::from_utf8_lossy(&ns)),
    )
    .map(|v| shell::subst(&v[..v.len().min(64)]))
    .unwrap_or_default();
    if !shell::is_num(&st) || st == b"0" {
        return false;
    }
    job_live(ctx, std::slice::from_ref(&f)).is_empty()
}

/// The processes of the jobs recorded in FILES. A record whose job has ended is removed once its end
/// has been dealt with (job_said): until then job_note needs it to name what ended (finding 405).
pub fn job_live(ctx: &Ctx, files: &[std::path::PathBuf]) -> Vec<(Vec<u8>, Vec<u8>)> {
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
pub fn job_said(ctx: &Ctx, log: &[u8]) -> bool {
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

// --- run -d's own: recording a job, --replace, its first moments, run's marks --------------------------

/// job_exe ARGV0: a job's program, for --replace to name a job of it run with other arguments (#255):
/// a path resolved lexically against this directory; a bare name as it is.
pub fn job_exe(a0: &[u8]) -> Vec<u8> {
    if a0.contains(&b'/') {
        crate::repo::realpath_ms(a0).unwrap_or_else(|| a0.to_vec())
    } else {
        a0.to_vec()
    }
}

/// A word as `jq --arg` takes it: a byte that starts no UTF-8 character is U+FFFD.
fn jstr(v: &[u8]) -> Value {
    Value::String(String::from_utf8_lossy(v).into_owned())
}

/// job_record PID LOG ARGV...: after a launch, the record of job PID (in the box), with the start
/// time of the process with that pid (- when it is gone already: finding 405).
pub fn job_record(ctx: &Ctx, pid: &[u8], log: &[u8], argv: &[Vec<u8>]) {
    let mut start = Vec::new();
    for [_, bp, _, st] in box_procs(ctx) {
        if awk_eq(&bp, pid) {
            start = st;
        }
    }
    let start = if start.is_empty() {
        b"-".to_vec()
    } else {
        start
    };
    let dir = ctx.at("jobs");
    let _ = std::fs::create_dir_all(&dir);
    let mut m = serde_json::Map::new();
    m.insert("pid".into(), jstr(pid));
    m.insert("start".into(), jstr(&start));
    m.insert("log".into(), jstr(log));
    m.insert(
        "exe".into(),
        jstr(&job_exe(argv.first().map_or(&b""[..], |a| a))),
    );
    m.insert(
        "argv".into(),
        Value::Array(argv.iter().map(|a| jstr(a)).collect()),
    );
    let text = cat!(json::compact(&Value::Object(m)), "\n");
    let _ = std::fs::write(
        dir.join(std::ffi::OsStr::new(
            &String::from_utf8_lossy(pid).into_owned(),
        )),
        text,
    );
}

/// A record file's one JSON value, as jq reads it (None for anything but one value).
fn record(f: &std::path::Path) -> Option<Value> {
    let text = std::fs::read(f).ok()?;
    let (vals, ok) = json::values(&text);
    match (vals.as_slice(), ok) {
        ([v], true) => Some(v.clone()),
        _ => None,
    }
}

/// `"\(.x)"`: a string as it is, anything else as jq writes it.
fn interp(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(x) => json::compact(x),
        None => "null".into(),
    }
}

/// The record files in $D/jobs, in glob order (`[ -f ]`: regular files, links followed).
fn job_files(ctx: &Ctx) -> Vec<std::path::PathBuf> {
    let dir = ctx.at("jobs");
    shell::glob(&dir, false)
        .into_iter()
        .map(|n| dir.join(shell::path(&n)))
        .filter(|f| f.is_file())
        .collect()
}

/// job_others ARGV...: the live `run -d` jobs of ARGV's program, as "ARGV0 (pid PID, ARGS)" by pid.
fn job_others(ctx: &Ctx, argv: &[Vec<u8>]) -> Vec<String> {
    let a0 = argv.first().cloned().unwrap_or_default();
    let exe = String::from_utf8_lossy(&job_exe(&a0)).into_owned();
    let a0 = String::from_utf8_lossy(&a0).into_owned();
    let mut out: Vec<(Vec<u8>, String)> = Vec::new();
    for f in job_files(ctx) {
        let Some(v) = record(&f) else { continue };
        let first = v
            .get("argv")
            .and_then(|a| a.get(0))
            .cloned()
            .unwrap_or(Value::Null);
        let mine = match v.get("exe") {
            Some(e) if !matches!(e, Value::Null | Value::Bool(false)) => {
                *e == Value::String(exe.clone())
            }
            _ => first == Value::String(a0.clone()) || first == Value::String(exe.clone()),
        };
        if !mine || job_live(ctx, std::slice::from_ref(&f)).is_empty() {
            continue;
        }
        let args = match v.get("argv") {
            Some(Value::Array(a)) if a.len() > 1 => {
                let parts: Option<Vec<String>> = a[1..]
                    .iter()
                    .map(|x| match x {
                        Value::String(s) => Some(s.clone()),
                        Value::Null => Some(String::new()),
                        Value::Number(_) | Value::Bool(_) => Some(json::compact(x)),
                        _ => None,
                    })
                    .collect();
                let Some(p) = parts else { continue };
                p.join(" ")
            }
            Some(Value::Array(_)) => "no arguments".into(),
            _ => continue,
        };
        let line = format!(
            "{} (pid {}, {args})",
            interp(Some(&first)),
            interp(v.get("pid"))
        );
        let pid = json::raw(v.get("pid").unwrap_or(&Value::Null)).into_bytes();
        out.push((cat!(pid, "\t", line), line));
    }
    // `sort -n | cut -f2-`: by the pid's number, then the whole line.
    let num = |l: &[u8]| -> u128 {
        let d: Vec<u8> = l.iter().copied().take_while(u8::is_ascii_digit).collect();
        String::from_utf8_lossy(&d).parse().unwrap_or(0)
    };
    out.sort_by(|a, b| num(&a.0).cmp(&num(&b.0)).then_with(|| a.0.cmp(&b.0)));
    out.into_iter().map(|(_, l)| l).collect()
}

fn now_ms() -> i64 {
    now_us() / 1000
}

/// run -d --replace (issue #29): stop the jobs `run -d` started in this box with these same words,
/// and wait until they and their windows are gone: SIGTERM to each job's processes, SIGKILL to what is
/// left after 5 s. Refuses, before anything is started, when one cannot be stopped or a window of it
/// stays.
pub fn job_replace(ctx: &Ctx, quiet: bool, argv: &[Vec<u8>]) -> crate::exit::Res<()> {
    let want = Value::Array(argv.iter().map(|a| jstr(a)).collect());
    let recs: Vec<std::path::PathBuf> = job_files(ctx)
        .into_iter()
        .filter(|f| record(f).is_some_and(|v| v.get("argv") == Some(&want)))
        .collect();
    let mut procs = if recs.is_empty() {
        Vec::new()
    } else {
        job_live(ctx, &recs)
    };
    // Stopped on purpose: not an early end for the next command to say (job_note).
    if !procs.is_empty() {
        let mut stop = Vec::new();
        for f in &recs {
            let Some(log) =
                record(f).and_then(|v| v.get("log").and_then(Value::as_str).map(String::from))
            else {
                continue;
            };
            if let Some(ns) = re_group(r"/run-([0-9]+)\.log$", log.as_bytes(), 1) {
                stop.push(cat!("omabox.run-", ns, ".exit"));
            }
        }
        job_noted(ctx, &stop);
    }
    if procs.is_empty() {
        // A job of the same program with other arguments still runs (#255): named, said under -q too.
        let others = job_others(ctx, argv);
        if !others.is_empty() {
            let verb = if others.len() == 1 {
                "still runs: stop it, or --replace with its arguments"
            } else {
                "still run: stop them, or --replace with the arguments of one"
            };
            say(format!(
                "--replace: no earlier job with these arguments; {} {verb}",
                others.join(", ")
            ));
        } else if !quiet {
            say(cat!(
                "--replace: no earlier job of this command in box '",
                ctx.name,
                "'"
            ));
        }
        return Ok(());
    }
    // Its windows, to see them go (Hyprland's pids are the box's).
    let bpids: Vec<Value> = procs
        .iter()
        .filter_map(|(_, b)| serde_json::from_slice::<Value>(b).ok())
        .collect();
    let windows = |cj: &[u8]| -> Vec<Vec<u8>> {
        let (vals, _) = json::values(cj);
        let mut out = Vec::new();
        for v in vals {
            let Value::Array(a) = v else { continue };
            for c in a {
                if c.get("pid").is_some_and(|p| bpids.contains(p)) {
                    out.push(json::raw(c.get("address").unwrap_or(&Value::Null)).into_bytes());
                }
            }
        }
        out
    };
    let cj = crate::hypr::hypr_json(ctx, &["clients"])?;
    let wins = windows(&cj);
    let t0 = now_ms();
    let (mut end, mut killed) = (t0 + 5000, false);
    let signal = |procs: &[(Vec<u8>, Vec<u8>)], sig: i32| {
        for (h, _) in procs {
            if let Ok(p) = String::from_utf8_lossy(h).parse::<libc::pid_t>() {
                // SAFETY: a pid of one of the box's processes; kill only sends a signal.
                unsafe {
                    libc::kill(p, sig);
                }
            }
        }
    };
    signal(&procs, libc::SIGTERM);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        procs = job_live(ctx, &recs);
        if procs.is_empty() {
            break;
        }
        if now_ms() < end {
            continue;
        }
        if killed {
            let ids: Vec<Vec<u8>> = procs.iter().map(|(_, b)| b.clone()).collect();
            return Err(ctx.die(cat!(
                "--replace: the earlier job still runs after SIGKILL (pids in the box: ",
                ids.join(&b' '),
                "); nothing started"
            )));
        }
        killed = true;
        end = now_ms() + 3000;
        signal(&procs, libc::SIGKILL);
    }
    // A client's windows go when it does; a window still there after 3 s is not what --replace is for.
    let mut n = 0;
    if !wins.is_empty() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let cj = crate::hypr::hypr_json(ctx, &["clients"])?;
            let (vals, _) = json::values(&cj);
            let mut still: Vec<Vec<u8>> = Vec::new();
            for v in vals {
                let Value::Array(a) = v else { continue };
                for c in a {
                    let addr = json::raw(c.get("address").unwrap_or(&Value::Null)).into_bytes();
                    if wins.contains(&addr) {
                        still.extend(shell::words(&addr));
                    }
                }
            }
            if still.is_empty() {
                break;
            }
            if std::time::Instant::now() >= until {
                return Err(ctx.die(cat!(
                    "--replace: the earlier job ended, but its window ",
                    still.join(&b' '),
                    " is still there; nothing started"
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        n = wins.iter().filter(|w| !w.is_empty()).count();
    }
    if !quiet {
        let s = if n == 1 { "" } else { "s" };
        let k = if killed {
            "; it ignored SIGTERM for 5 s: killed"
        } else {
            ""
        };
        say(format!(
            "--replace: stopped the earlier job ({n} window{s}) in {:.2}s{k}",
            (now_ms() - t0) as f64 / 1000.0
        ));
    }
    Ok(())
}

/// job_ended WHEN QUIET PID LOG EXIT_FILE ARGV... (#215, #286): whether job PID had ended, nothing of
/// its session left running; if so it is said, with its exit status (run-job.sh's EXIT_FILE, read in
/// the box) and its log's last lines, and its JOB_RC returned: the status (`start`, a plain run -d's
/// first moments: 0 too, a 0 under QUIET not said; WAIT, run -d --wait's answer that did not settle:
/// a 0 is 124, said, as its screen is not the command's), 1 when it was not kept.
#[allow(clippy::too_many_arguments)]
pub fn job_ended(
    ctx: &Ctx,
    wait: bool,
    quiet: bool,
    jpid: &[u8],
    log: &[u8],
    ex: &[u8],
    argv: &[Vec<u8>],
) -> crate::exit::Res<Option<i32>> {
    use crate::exec::{ErrTo, Out, on_box};
    if !shell::is_num(jpid) {
        return Ok(None);
    }
    let rec = ctx.at("jobs").join(shell::path(jpid));
    if rec.is_file() && !job_live(ctx, std::slice::from_ref(&rec)).is_empty() {
        return Ok(None);
    }
    // The waiter writes the status once the job's process is reaped: a moment after it is gone.
    let mut st = Vec::new();
    for i in 1..=10 {
        let argv = vec![
            b"timeout".to_vec(),
            b"2".to_vec(),
            b"cat".to_vec(),
            b"--".to_vec(),
            ex.to_vec(),
        ];
        let r = on_box(ctx, &argv, false, Out::Capture(Some(64)), ErrTo::Null);
        st = shell::subst(&r.out);
        if shell::is_num(&st) {
            break;
        }
        st.clear();
        if i != 10 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    let base = ex.rsplit(|&c| c == b'/').next().unwrap_or(ex).to_vec();
    job_noted(ctx, &[base]);
    let _ = std::fs::remove_file(&rec);
    let what = job_how(&st);
    let rc = if st.is_empty() {
        1
    } else if st == b"0" && wait {
        124
    } else {
        shell::test_int(&st).map_or(1, |n| (n & 0xff) as i32)
    };
    if !wait && st == b"0" && quiet {
        return Ok(Some(rc));
    }
    let mut miss = Vec::new();
    for a in argv {
        if a.starts_with(b"/") && shell::path(a).exists() {
            let t = vec![b"test".to_vec(), b"-e".to_vec(), a.clone()];
            if on_box(ctx, &t, false, Out::Inherit, ErrTo::Inherit)
                .or_exit()?
                .rc
                != 0
            {
                miss.push(a.clone());
            }
        }
    }
    let t = vec![
        b"timeout".to_vec(),
        b"2".to_vec(),
        b"tail".to_vec(),
        b"-n".to_vec(),
        b"5".to_vec(),
        b"--".to_vec(),
        cat!("/home/sbx/", log),
    ];
    let r = on_box(ctx, &t, false, Out::Capture(Some(65536)), ErrTo::Null);
    let mut tail = Vec::new();
    if !r.out.is_empty() {
        let body = r.out.strip_suffix(b"\n").unwrap_or(&r.out);
        for l in body.split(|&c| c == b'\n') {
            tail.extend_from_slice(&cat!("  ", shell::cut_line(l, 300), "\n"));
        }
    }
    let tail = shell::subst(&tail);
    if wait {
        let zero: &[u8] = if st == b"0" {
            b": what the screen shows is not its (124 is about the screen)"
        } else {
            b""
        };
        say(cat!(
            "the command ",
            what,
            " before the screen settled, and left nothing running",
            zero
        ));
    } else {
        say(cat!(
            "the command ",
            what,
            " at once in box '",
            ctx.name,
            "', and left nothing running (log: ",
            ctx.d,
            "/home/",
            log,
            ")"
        ));
    }
    if !miss.is_empty() {
        say(cat!(
            "its arguments name host paths box '",
            ctx.name,
            "' does not have: ",
            miss.join(&b' '),
            " (a box sees the repo and what --ro-bind mounts, not /tmp or the rest of HOME)"
        ));
    }
    if tail.is_empty() {
        say("its log is empty");
    } else {
        say(cat!("its log ends (omabox log -b ", ctx.name, " run):"));
        let _ = std::io::stderr().write_all(&cat!(tail, "\n"));
    }
    Ok(Some(rc))
}

/// While an `omabox run` runs in the box, $D/runs/o.PID.START; once it ended, c.START.END (#300).
pub fn run_mark_begin(ctx: &Ctx) -> Option<i64> {
    let mark = now_ms();
    let dir = ctx.at("runs");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(dir.join(format!("o.{}.{mark}", std::process::id())), b"").ok()?;
    Some(mark)
}

/// Its end; dropped: a run that ended 10 s before this one began, and one whose omabox is gone.
pub fn run_mark_end(ctx: &Ctx, mark: Option<i64>) {
    let Some(mark) = mark else { return };
    let dir = ctx.at("runs");
    let now = now_ms();
    let _ = std::fs::rename(
        dir.join(format!("o.{}.{mark}", std::process::id())),
        dir.join(format!("c.{mark}.{now}")),
    );
    for f in shell::glob(&dir, false) {
        let gone = if let Some(e) = re_group(r"^c\.[0-9]+\.([0-9]+)$", &f, 1) {
            shell::test_int(&e).is_some_and(|e| e < mark - 10000)
        } else if let Some(p) = re_group(r"^o\.([0-9]+)\.[0-9]+$", &f, 1) {
            !shell::path(&cat!("/proc/", p)).is_dir()
        } else {
            false
        };
        if gone {
            let _ = std::fs::remove_file(dir.join(shell::path(&f)));
        }
    }
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
