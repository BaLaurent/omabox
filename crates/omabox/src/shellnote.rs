//! The box's Omarchy shell mid-session (#147), as shot, wait, windows, tree and pixel say it first
//! (shell_note): gone, hung (#209, finding 289), crashed since the last command said so, or killed
//! and started again by its launcher (#303, finding 408). As bin/omabox's shell_note, shell_state,
//! shell_crashes, shell_relaunches and what they call.

use crate::boxes::{box_home_file, meta, run_read};
use crate::cat;
use crate::exec::{ErrTo, Out, on_box};
use crate::exit::{Ctx, Exit, Res, say};
use crate::jobs::job_how;
use crate::repo::readlink_e;
use crate::shell::{self, strip_nl};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

fn words(v: &[&[u8]]) -> Vec<Vec<u8>> {
    v.iter().map(|w| w.to_vec()).collect()
}

/// box_qs: the box's Quickshell (`up --quickshell`'s build, finding 358), else the system's.
pub fn box_qs(ctx: &Ctx) -> Vec<u8> {
    let q = meta(&ctx.d, "quickshell").0;
    if q.is_empty() {
        b"/usr/bin/quickshell".to_vec()
    } else {
        q
    }
}

/// shell_pid: the shell's pid in the box (share/shell.sh writes it), or None.
pub fn shell_pid(ctx: &Ctx) -> Option<Vec<u8>> {
    let p = strip_nl(&run_read(&ctx.d, "omabox-shell.pid")?);
    shell::re_match("^[0-9]+$", &p).then_some(p)
}

/// shell_running PID: a process in the box, and not one that exited and was not reaped yet.
pub fn shell_running(ctx: &Ctx, pid: &[u8]) -> bool {
    let r = on_box(
        ctx,
        &words(&[b"cat", &cat!("/proc/", pid, "/stat")]),
        false,
        Out::Capture(None),
        ErrTo::Null,
        false,
    );
    r.rc == 0 && !crate::exit::contains(&shell::subst(&r.out), b") Z ")
}

/// shell_hung PID: no answer over its IPC in 2 s, from a shell up for 10 s or more.
pub fn shell_hung(ctx: &Ctx, pid: &[u8]) -> bool {
    let qs = box_qs(ctx);
    let r = on_box(
        ctx,
        &words(&[b"timeout", b"2", &qs, b"ipc", b"--pid", pid, b"show"]),
        false,
        Out::Null,
        ErrTo::Null,
        true,
    );
    if r.rc != 124 {
        return false;
    }
    // Its age in s: /proc/uptime less its start (stat's field 22, in clock ticks).
    let age = on_box(
        ctx,
        &words(&[
            b"sh",
            b"-c",
            br#"s=$(cat "/proc/$1/stat") || exit 1; set -- ${s##*") "}; t=$(getconf CLK_TCK) || t=100
    read -r up _ < /proc/uptime; echo $(( ${up%.*} - ${20} / t ))"#,
            b"sh",
            pid,
        ]),
        false,
        Out::Capture(None),
        ErrTo::Null,
        false,
    );
    if age.rc != 0 {
        return false;
    }
    let a = shell::subst(&age.out);
    shell::re_match("^[0-9]+$", &a) && shell::test_int(&a).is_some_and(|n| n >= 10)
}

/// shell_state: none (--no-shell), running, hung, restarting (its launcher about to start it again)
/// or gone.
pub fn shell_state(ctx: &Ctx) -> &'static str {
    if meta(&ctx.d, "shell").0 != b"true" {
        return "none";
    }
    if let Some(p) = shell_pid(ctx).filter(|p| shell_running(ctx, p)) {
        return if shell_hung(ctx, &p) {
            "hung"
        } else {
            "running"
        };
    }
    let r = on_box(
        ctx,
        &words(&[b"pgrep", b"-cf", b"[o]marchy-launch-shell"]),
        false,
        Out::Capture(None),
        ErrTo::Null,
        false,
    );
    // (`[ "$(...)" -gt 0 ] 2>/dev/null`: what is no number is not more than 0)
    if shell::test_int(&shell::subst(&r.out)).is_some_and(|n| n > 0) {
        "restarting"
    } else {
        "gone"
    }
}

/// The awk of shell_crash_dirs: each report whose first Config Path is the box's shell's.
const CRASH_AWK: &str = r#"FNR == 1 { seen = 0 }
    !seen && /^Config Path: / { seen = 1; c = substr($0, 14); gsub(/\/+/, "/", c); sub(/\/[^\/]*$/, "", c)
      if (c == want) { n = FILENAME; sub(/\/report\.txt$/, "", n); sub(/.*\//, "", n); print n } }"#;

/// shell_crash_dirs: the box's Quickshell crash report folders (each a line) of this box's shell:
/// no link, under the box HOME, a name on one line.
pub fn shell_crash_dirs(ctx: &Ctx) -> Vec<u8> {
    let (Some(dir), Some(home)) = (
        readlink_e(&cat!(ctx.d, "/home/.cache/quickshell/crashes")),
        readlink_e(&cat!(ctx.d, "/home")),
    ) else {
        return Vec::new();
    };
    if !dir.starts_with(&cat!(home, "/")) {
        return Vec::new();
    }
    let mut files: Vec<Vec<u8>> = Vec::new();
    for rep in shell::glob(&shell::path(&dir), false) {
        let d = cat!(dir, "/", rep);
        let f = cat!(d, "/report.txt");
        if rep.contains(&b'\n')
            || shell::path(&d).is_symlink()
            || shell::path(&f).is_symlink()
            || !shell::path(&f).is_file()
        {
            continue;
        }
        files.push(f);
    }
    if files.is_empty() {
        return Vec::new();
    }
    let want = meta(&ctx.d, "omarchy").0;
    let want = if want.is_empty() {
        b"/usr/share/omarchy".to_vec()
    } else {
        want
    };
    // (`printf '%s' "$want/shell" | tr -s /`, then one trailing / off)
    let mut w: Vec<u8> = Vec::new();
    for &c in &cat!(want, "/shell") {
        if !(c == b'/' && w.last() == Some(&b'/')) {
            w.push(c);
        }
    }
    let w = strip_nl(&w);
    let w = w.strip_suffix(b"/").unwrap_or(&w).to_vec();
    let mut c = crate::sig::command("awk");
    c.arg("-v")
        .arg(OsStr::from_bytes(&cat!("want=", w)))
        .arg(CRASH_AWK)
        .args(files.iter().map(|f| OsStr::from_bytes(f)))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    c.output().map(|o| o.stdout).unwrap_or_default()
}

/// shell_crashes: how many crash reports since `up`.
pub fn shell_crashes(ctx: &Ctx) -> usize {
    shell_crash_dirs(ctx)
        .split(|&c| c == b'\n')
        .filter(|l| !l.is_empty())
        .count()
}

/// shell_relaunches: "MARK COUNT STATUS", omabox-shell.mark, how many "relaunching" lines the
/// launcher wrote to shell.log, and the last one's status (- for none).
pub fn shell_relaunches(ctx: &Ctx) -> Vec<u8> {
    let mark = run_read(&ctx.d, "omabox-shell.mark")
        .map(|m| strip_nl(&m))
        .filter(|m| shell::re_match("^[0-9]+$", m))
        .unwrap_or(b"0".to_vec());
    let (mut n, mut st) = (0usize, b"-".to_vec());
    if let Some(f) = box_home_file(&ctx.d, "shell.log") {
        let text = std::fs::read(&f).unwrap_or_default();
        (n, st) = relaunch_lines(&text);
    }
    cat!(mark, " ", n.to_string(), " ", st)
}

/// `grep -aoE 'omarchy-shell: Omarchy shell exited with status [0-9]{1,3}; relaunching' | awk ...`:
/// how many such matches, and the last one's status (- for none).
fn relaunch_lines(text: &[u8]) -> (usize, Vec<u8>) {
    const LEAD: &[u8] = b"omarchy-shell: Omarchy shell exited with status ";
    const END: &[u8] = b"; relaunching";
    let (mut n, mut st) = (0, b"-".to_vec());
    for line in text.split(|&c| c == b'\n') {
        let mut i = 0;
        while let Some(at) = line[i..].windows(LEAD.len()).position(|w| w == LEAD) {
            let s = i + at + LEAD.len();
            let d = line[s..]
                .iter()
                .take(4)
                .take_while(|c| c.is_ascii_digit())
                .count();
            if (1..=3).contains(&d) && line[s + d..].starts_with(END) {
                n += 1;
                st = line[s..s + d].to_vec();
                i = s + d + END.len();
            } else {
                i = i + at + 1;
            }
        }
    }
    (n, st)
}

/// shell_note: one line from shot, wait, windows, tree, pixel, keys and click: the shell gone or
/// hung, crashed since the last command said so, or killed and started again by its launcher; then
/// what the next one compares with.
pub fn shell_note(ctx: &Ctx) -> Res<()> {
    if meta(&ctx.d, "shell").0 != b"true" {
        return Ok(());
    }
    let n = shell_crashes(ctx);
    let seen = std::fs::read(ctx.at("shell-crashes-seen"))
        .map(|s| strip_nl(&s))
        .unwrap_or(b"0".to_vec());
    let seen = if shell::re_match("^[0-9]+$", &seen) {
        seen
    } else {
        b"0".to_vec()
    };
    let rl = shell_relaunches(ctx);
    let w = shell::read_fields(&rl, 3);
    let (m, c, st) = (w[0].clone(), w[1].clone(), w[2].clone());
    // (None yet, a box an older omabox started: this one is the first look.)
    let old = std::fs::read(ctx.at("shell-relaunches-seen"))
        .map(|o| strip_nl(&o[..o.len().min(100)]))
        .unwrap_or_default();
    let ow = shell::read_fields(&old, 3);
    let (om, oc) = (ow[0].clone(), ow[1].clone());
    let mut new = 0i64;
    if shell::re_match("^[0-9]+$", &om) && shell::re_match("^[0-9]+$", &oc) {
        new = if m == om {
            // (`$((c - oc))`: a number bash's arithmetic refuses, an 08 in the seen file, is its
            // own words, then exit 1)
            let (Some(ci), Some(oi)) = (shell::bash_int(&c), shell::bash_int(&oc)) else {
                return Err(Exit(1));
            };
            ci.wrapping_sub(oi)
        } else {
            shell::test_int(&c).unwrap_or(0)
        };
    }
    let state = shell_state(ctx);
    let name = &ctx.name;
    let seen_n = shell::test_int(&seen);
    let starting: &[u8] = if state == "running" {
        b""
    } else {
        b" (it is starting)"
    };
    if state == "gone" {
        let after = if n == 0 {
            String::new()
        } else {
            format!(" after {n} crash(es)")
        };
        say(cat!(
            "warning: box '",
            name,
            "' has no shell (gone",
            after,
            "; omabox log -b ",
            name,
            " shell; omabox restart-shell -b ",
            name,
            ")"
        ));
    } else if state == "hung" {
        say(cat!(
            "warning: the shell of box '",
            name,
            "' does not answer (hung: its bar is frozen; omabox log -b ",
            name,
            " shell; omabox restart-shell -b ",
            name,
            ")"
        ));
    } else if seen_n.is_some_and(|s| (n as i64) > s) {
        say(cat!(
            "warning: the shell crashed since the last command and was started again",
            starting,
            " (omabox log -b ",
            name,
            " shell)"
        ));
    } else if new > 0 {
        let how = if shell::re_match("^[0-9]+$", &st) {
            job_how(&st)
        } else {
            "exited".to_string()
        };
        say(cat!(
            "warning: the shell ",
            how,
            " since the last command and was started again",
            starting,
            " (omabox log -b ",
            name,
            " shell)"
        ));
    }
    let _ = std::fs::write(ctx.at("shell-crashes-seen"), format!("{n}\n"));
    let _ = std::fs::write(ctx.at("shell-relaunches-seen"), cat!(rl, "\n"));
    Ok(())
}

/// shell_seen: what the next shell_note compares with, as things are now (a command said it).
pub fn shell_seen(ctx: &Ctx) {
    let n = shell_crashes(ctx);
    let _ = std::fs::write(ctx.at("shell-crashes-seen"), format!("{n}\n"));
    let rl = shell_relaunches(ctx);
    let _ = std::fs::write(ctx.at("shell-relaunches-seen"), cat!(rl, "\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunches_counted_as_grep_and_awk() {
        let t = b"x omarchy-shell: Omarchy shell exited with status 9; relaunching.\n\
            omarchy-shell: Omarchy shell exited with status 1234; relaunching\n\
            a omarchy-shell: Omarchy shell exited with status 143; relaunching b omarchy-shell: Omarchy shell exited with status 2; relaunching\n";
        assert_eq!(relaunch_lines(t), (3, b"2".to_vec()));
        assert_eq!(relaunch_lines(b"nothing\n"), (0, b"-".to_vec()));
    }
}
