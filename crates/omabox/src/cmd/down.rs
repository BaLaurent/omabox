//! `omabox down`: a box taken down, under its name's lock: its peek windows closed, its PID 1
//! SIGKILLed as box_pid finds it (the kernel then ends everything in its pid namespace; SIGTERM does
//! not work, finding 6) and its pasta with it (ready::kill_box), its reaper ended, its dir and HOME
//! removed. `--all`, `--orphans` (#220). The reaper takes a box down through `down_one`, telling it
//! which box it decided on (DownIf).

use crate::args::{optarg, unknown_opt};
use crate::boxes::{box_alive, box_state, list_names, lock_file, meta};
use crate::cat;
use crate::exit::{Ctx, Res, muted, say};
use crate::jail::{in_jail, jail_id};
use crate::jq::{self, Args};
use crate::select::{select_box, session_boxes};
use crate::shell;
use std::time::Duration;

/// down_orphans' word on what started an orphan.
const STARTER_JQ: &str = r#".starter | "\(.command), pid \(.pid)""#;
/// submap_release's Lua.
const SUBMAP_LUA: &str =
    r#"if hl.get_current_submap() == "omabox" then hl.dispatch(hl.dsp.submap("reset")) end"#;

/// The programs here as bin/omabox has them (`_rust-jq --inline`).
pub const JQ_INLINE: &[&str] = &[STARTER_JQ, SUBMAP_LUA];

/// A reaper's down (bash's DOWN_IF_CREATED, DOWN_IF_AGENT_GONE): only the box it decided on, created
/// then, and, when it decided on an agent that exited, only while no agent of its session took it
/// over (take_over). The reaper's down says nothing while it waits, and leaves no `.down-` marker.
#[derive(Default, Clone)]
pub struct DownIf {
    pub created: Option<Vec<u8>>,
    pub agent_gone: bool,
}

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    let (mut names, mut all, mut orphans) = (Vec::<Vec<u8>>::new(), false, false);
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_slice() {
            b"--all" => all = true,
            b"--orphans" => orphans = true,
            b"-b" | b"--box" => {
                optarg(ctx, a, args.get(i + 1))?;
                names.push(args[i + 1].clone());
                i += 1;
            }
            _ if a.starts_with(b"-") => return Err(unknown_opt(ctx, "down", a)),
            _ => names.push(a.clone()),
        }
        i += 1;
    }
    if orphans {
        if all || !names.is_empty() {
            return Err(ctx.die("down: --orphans takes no names, nor --all"));
        }
        ctx.parsed();
        return down_orphans(ctx);
    }
    if all && !names.is_empty() {
        return Err(ctx.die("down: --all or names, not both"));
    }
    ctx.parsed();
    if all {
        names = list_names(ctx, false);
    } else if names.is_empty() {
        // No name: select_box derives it, and outside any repo falls back to the session's box
        // (finding 200); passed the derived name, it would take it as given and not (finding 259).
        names.push(Vec::new());
    }
    let mut ok = true;
    for n in &names {
        ok &= down_one(ctx, n, &DownIf::default())?;
    }
    if ok {
        Ok(())
    } else {
        Err(crate::exit::Exit(1))
    }
}

/// down of one name (`""`: the default box): whether it went (or there was none to take down);
/// Err for a refusal that ends the command (a bad name, another jail's box).
pub fn down_one(ctx: &mut Ctx, given: &[u8], cond: &DownIf) -> Res<bool> {
    select_box(ctx, given)?;
    let _ = std::fs::remove_file(ctx.in_boxes(cat!(".expired-", ctx.name)));
    let lk = ctx.in_boxes(cat!(".lock-", ctx.name));
    let there = |ctx: &Ctx| shell::path(&ctx.d).is_dir() || lk.exists();
    // No dir and no lock: no box. A lock without a dir can be an `up` in its first ~40 ms (it makes
    // the dir after taking the lock, finding 147): waited for below, and the box it made taken down.
    // The time it found none, for an `up` of the name that started before but has no lock yet:
    // written before the lock is looked at again (finding 237).
    if !there(ctx) {
        down_mark(ctx, cond);
        if !there(ctx) {
            return Ok(down_none(ctx));
        }
    }
    let _ = std::fs::create_dir_all(shell::path(&ctx.boxes));
    // The reaper takes a box down through here while holding nothing; anyone else waits for an up.
    let lock = match lock_file(&lk, Duration::ZERO) {
        Some(l) => Some(l),
        None => {
            if cond.created.is_none() {
                say(cat!("box '", ctx.name, "': waiting for its up to finish"));
            }
            lock_file(&lk, Duration::from_secs(60))
        }
    };
    let Some(lock) = lock else {
        say(cat!(
            "box '",
            ctx.name,
            "' is busy (an up in progress?); not taken down"
        ));
        return Ok(false);
    };
    // A box gone takes its lock file with it, removed while held (#158). Its marker before the lock
    // goes (finding 237): an `up` waiting on it then reads it.
    if !shell::path(&ctx.d).is_dir() {
        down_mark(ctx, cond);
        let _ = std::fs::remove_file(&lk);
        drop(lock);
        return Ok(down_none(ctx));
    }
    // A jailed caller's box is checked again under the lock: select_box passed a name whose up (the
    // user's, or another jail's) had not written its box.json yet, and that up has finished since.
    if in_jail() && meta(&ctx.d, "jail").0 != jail_id() {
        drop(lock);
        return Err(ctx.die(cat!(
            "box '",
            ctx.name,
            "' is not this jail's: an agent inside ai-jail drives only the boxes it started"
        )));
    }
    // The reaper decided on the box it watched, then waited here: an `up` holding the lock may have
    // started a new box under the name (finding 74). Not that one.
    if let Some(c) = &cond.created
        && meta(&ctx.d, "created").0 != *c
    {
        return Ok(true);
    }
    // Nor, when it decided on an agent that exited, a box its session took over meanwhile.
    if cond.agent_gone && crate::boxes::agent_alive(&ctx.d) {
        return Ok(true);
    }
    // A box whose PID 1 this user namespace cannot read (a sandbox's call) is left as it is, said as
    // up says it: bash's kill_box took it for no box, removed its dir and said it down while its
    // PID 1 ran on (finding 473).
    if matches!(crate::boxes::box_pid(&ctx.d), crate::boxes::BoxPid::Unknown) {
        say(cat!(
            "cannot tell whether box '",
            ctx.name,
            "' is up from this user namespace (a sandbox's?): run omabox outside it"
        ));
        return Ok(false);
    }
    // Its peek windows, one per monitor shown (#164).
    for p in crate::marks::peek_pids(ctx) {
        signal(&p, libc::SIGTERM);
    }
    let was_interactive = meta(&ctx.d, "mode").0 == b"interactive";
    if !crate::ready::kill_box(ctx) {
        return Ok(false);
    }
    crate::ready::kill_reapers(ctx);
    crate::ready::rm_box(ctx);
    let _ = std::fs::remove_file(&lk);
    drop(lock);
    // (a host that does not answer must not fail the down)
    if was_interactive {
        muted(|| submap_release(ctx));
    }
    say(cat!("box '", ctx.name, "' down"));
    Ok(true)
}

/// `kill -SIG PID`, a pid only, its failure ignored.
fn signal(pid: &[u8], sig: libc::c_int) {
    if let Some(n) = shell::dec(pid)
        .and_then(|n| i32::try_from(n).ok())
        .filter(|&n| n > 0)
    {
        // SAFETY: kill with a positive pid and a signal number.
        unsafe {
            libc::kill(n, sig);
        }
    }
}

/// A down that finds no box leaves when it was started (proc_start: clock ticks since boot), for an
/// up started before it that has no lock yet: they compare starts, not when each got to its check
/// (finding 147). A jailed caller's names its jail on a second line: an up cancels for it only when
/// it is that jail's (a jail's down must not cancel the user's up of a name). Written before the
/// down lets go of the name's lock (finding 237). Not the reaper's.
///
/// After the start, on its line, what an up started in the same tick tells the order by (finding
/// 513): the pid its pid namespace made last as the marker is written, that namespace, and this
/// call's time (CALL_US), each `-` when it cannot be told. Bash's up reads a marker of one number
/// only, so it takes these for none (a bash up is a fallback's, with a bash down).
fn down_mark(ctx: &Ctx, cond: &DownIf) {
    if cond.created.is_none() {
        let me = std::process::id().to_string().into_bytes();
        let mark = ctx.in_boxes(cat!(".down-", ctx.name));
        // (a jail's never takes the place of the user's own, which cancels any up)
        let users = in_jail() && std::fs::read(&mark).is_ok_and(|m| user_marker(&m));
        if let (Some(start), false) = (crate::procfs::proc_start(&me), users) {
            let last = crate::procfs::last_pid().map(|n| n.to_string().into_bytes());
            let ns = crate::procfs::pidns(b"self").filter(|n| !n.is_empty() && !n.contains(&b' '));
            let mut v = cat!(
                start,
                " ",
                last.unwrap_or_else(|| b"-".to_vec()),
                " ",
                ns.unwrap_or_else(|| b"-".to_vec()),
                " ",
                crate::call_us(),
                "\n"
            );
            if in_jail() {
                v.extend(cat!(jail_id(), "\n"));
            }
            let _ = std::fs::create_dir_all(shell::path(&ctx.boxes))
                .and_then(|()| std::fs::write(&mark, v));
        }
    }
    prune_marks(ctx);
}

/// A `.down-` marker of the user's own: a time and no jail line.
fn user_marker(m: &[u8]) -> bool {
    let t = shell::strip_nl(m);
    !t.is_empty() && !t.contains(&b'\n')
}

/// The markers nobody reads any more: a down's after 10 minutes, and an idle box's note
/// (.expired-NAME) after a day (finding 237).
pub fn prune_marks(ctx: &Ctx) {
    let Ok(rd) = std::fs::read_dir(shell::path(&ctx.boxes)) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for e in rd.flatten() {
        let n = e.file_name().into_encoded_bytes();
        let max = if n.starts_with(b".down-") {
            600
        } else if n.starts_with(b".expired-") {
            86_400
        } else {
            continue;
        };
        let Ok(m) = e.metadata() else { continue };
        let old = m
            .modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|d| d.as_secs() >= max);
        if m.is_file() && old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// `down` found no box under the name: said. A name derived here (no -b) while this agent session
/// has boxes up under others, started from other directories (#262), names each and the down that
/// takes it, and fails: the agent's box is still up.
fn down_none(ctx: &Ctx) -> bool {
    let mine = if ctx.name_derived {
        session_boxes(ctx)
    } else {
        Vec::new()
    };
    if mine.is_empty() {
        say(cat!("no box '", ctx.name, "'"));
        return true;
    }
    let home = shell::env_or_empty("HOME");
    let mut has = Vec::new();
    for n in &mine {
        let mut dir = meta(&cat!(ctx.boxes, "/", n), "dir").0;
        if dir == home || dir.starts_with(&cat!(home, "/")) {
            dir = cat!("~", &dir[home.len()..]);
        }
        if !has.is_empty() {
            has.extend_from_slice(b"; ");
        }
        has.extend(cat!("'", n, "'"));
        if !dir.is_empty() {
            has.extend(cat!(" (started in ", dir, ")"));
        }
        has.extend(cat!(": omabox down -b ", n));
    }
    say(cat!("no box '", ctx.name, "'; this session has ", has));
    false
}

/// down --orphans (#220): every box `ls` calls an orphan, each named with what started it, then
/// taken down as that box (not a new one that took its name meanwhile).
fn down_orphans(ctx: &mut Ctx) -> Res<()> {
    let mut found: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for n in list_names(ctx, false) {
        select_box(ctx, &n)?;
        if box_state(&ctx.d) != "up" || !crate::ready::box_orphan(ctx) {
            continue;
        }
        found.push((ctx.name.clone(), meta(&ctx.d, "created").0));
    }
    if found.is_empty() {
        say("no orphan boxes");
        return Ok(());
    }
    let mut ok = true;
    for (n, created) in found {
        select_box(ctx, &n)?;
        let r = Args::new(&["-r", STARTER_JQ])
            .arg(cat!(ctx.d, "/box.json"))
            .run_file(jq::Err::Null);
        let by = if r.ok() { r.text() } else { b"?".to_vec() };
        say(cat!(
            "box '",
            ctx.name,
            "': an orphan (no idle limit; what started it, ",
            by,
            ", is gone): taking it down"
        ));
        let cond = DownIf {
            created: Some(created),
            agent_gone: false,
        };
        ok &= down_one(ctx, &n, &cond)?;
    }
    if ok {
        Ok(())
    } else {
        Err(crate::exit::Exit(1))
    }
}

/// After an interactive box went (finding 134): the host out of the "omabox" submap once no
/// interactive box is left (a host config reload keeps the submap and drops its binds, finding 118).
fn submap_release(ctx: &Ctx) {
    for n in list_names(ctx, false) {
        let mut c = ctx.clone();
        c.d = cat!(ctx.boxes, "/", n);
        c.name = n;
        if meta(&c.d, "mode").0 == b"interactive" && box_alive(&c).unwrap_or(false) {
            return;
        }
    }
    let _ = crate::host::host_hyprctl(ctx, &[b"eval", SUBMAP_LUA.as_bytes()], true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_of_the_user() {
        assert!(user_marker(b"123\n"));
        assert!(!user_marker(b"123\nj1\n"));
        assert!(!user_marker(b""));
    }

    #[test]
    fn programs_are_bash_s() {
        let script =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../bin/omabox")).unwrap();
        for p in JQ_INLINE {
            assert!(
                crate::exit::contains(&script, p.as_bytes()),
                "not in bin/omabox: {p}"
            );
        }
    }
}
