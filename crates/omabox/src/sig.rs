//! What bash's traps did for the commands ported here: a throwaway box taken down however `run` ends
//! (its EXIT trap: on the way out, and on INT, TERM or HUP), and a follower's processes ended with
//! it (follow_start's: exit 130, 143 or 129). Also how this half starts bin/omabox for what it has
//! not taken over (up, down).

use crate::cat;
use crate::exit::say;
use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

/// bin/omabox, for a command this half leaves to it, with this process's environment.
pub fn omabox_cmd() -> Command {
    command(crate::root().join("bin/omabox"))
}

/// SIGPIPE as this process came, before Rust's runtime set it ignored (in main's preamble): read by
/// a constructor, which runs before that.
static PIPE_IGNORED: AtomicBool = AtomicBool::new(false);

extern "C" fn pipe_as_it_came() {
    PIPE_IGNORED.store(ignored(libc::SIGPIPE), Ordering::Relaxed);
}

#[used]
#[unsafe(link_section = ".init_array")]
static PIPE_AS_IT_CAME: extern "C" fn() = pipe_as_it_came;

/// Whether SIGPIPE came ignored.
pub fn pipe_ignored() -> bool {
    PIPE_IGNORED.load(Ordering::Relaxed)
}

/// `Command::new` for every child of this half: std's spawn sets SIGPIPE back to the default in it,
/// where bash's children keep an ignored one (a write to a closed pipe is EPIPE there, not death).
pub fn command(prog: impl AsRef<OsStr>) -> Command {
    // (every child of main's is made here, just before it starts: none once a trap is on its way)
    park_while_stopping();
    let mut c = Command::new(prog);
    // Every sort omabox runs orders bytes (and reads numbers) as C does, the same in every locale
    // (finding 446: bash's ran it in the caller's, whose collation it followed).
    if c.get_program() == "sort" {
        c.env("LC_ALL", "C");
    }
    if pipe_ignored() {
        // SAFETY: signal is async-signal-safe; run between fork and exec, after std's own reset.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut c, || {
                libc::signal(libc::SIGPIPE, libc::SIG_IGN);
                Ok(())
            });
        }
    }
    c
}

/// The command's foreground child running (held_run, jq on the main thread): a signal's trap waits
/// for it, as bash runs a trap once the command in the foreground is over.
pub static FOREGROUND: Mutex<()> = Mutex::new(());

static MAIN: std::sync::OnceLock<std::thread::ThreadId> = std::sync::OnceLock::new();

/// This thread is main's (set at its start).
pub fn set_main() {
    let _ = MAIN.set(std::thread::current().id());
}

/// Whether this thread is main's, and not running a trap's undo (UNDO_MAIN): what parks and
/// takes FOREGROUND. An undo main runs (int_deferred_end's) runs its children plainly, as a trap's
/// thread does: the thread, on its way to the same undos, waits for that one (trap::run_undos' lock)
/// and must find main neither parked nor waiting for FOREGROUND, which the thread may hold.
fn main_step() -> bool {
    MAIN.get() == Some(&std::thread::current().id()) && !UNDO_MAIN.load(Ordering::SeqCst)
}

/// Main runs a trap's undo (trap::run_undos).
pub static UNDO_MAIN: AtomicBool = AtomicBool::new(false);

/// This thread is main's.
pub fn on_main() -> bool {
    MAIN.get() == Some(&std::thread::current().id())
}

/// A terminating signal's trap is on its way (shot --no-pointer's release); GATED: main holds the
/// pointer's gate (ptr.rs), where it must not stop.
pub static STOPPING: AtomicBool = AtomicBool::new(false);
pub static GATED: AtomicBool = AtomicBool::new(false);

/// C run as bash runs a foreground command (every child of main's waited for goes through here):
/// C made before (sig::command parks, never under FOREGROUND), then with FOREGROUND held: spawned
/// (nothing once a trap is on its way), FG_ACTIVE raised only once it exists (unless it was
/// started with SIGINT ignored, INT_IGNORED: a group Ctrl-C reaches no child then, and is no INT
/// to omabox alone, finding 112's `<(...)`), WITH run on it (feeding or reading its pipes), its end
/// read and recorded (exec::status: FG_INT) before the lock drops. C is dropped right after
/// the spawn, so the pipe ends it held close. Its status and signal, or the spawn's error.
pub fn run_child<T>(
    c: Command,
    int_ignored: bool,
    with: impl FnOnce(&mut std::process::Child) -> T,
) -> std::io::Result<(T, (i32, Option<i32>))> {
    let mut c = c;
    let end = |ch: &mut std::process::Child| match ch.wait() {
        Ok(s) => crate::exec::status(s),
        Err(_) => (1, None),
    };
    if !main_step() {
        let spawned = c.spawn();
        drop(c);
        let mut ch = spawned?;
        let t = with(&mut ch);
        let st = end(&mut ch);
        return Ok((t, st));
    }
    // Nothing new starts once a trap is on its way, as bash starts nothing after its trap: a TERM
    // between two steps of monitor add must not see the box's output made after the trap's remove
    // (a monitor no one listed).
    let r = loop {
        park_while_stopping();
        let g = FOREGROUND
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // (a signal between the park and the lock: its trap first, nothing started)
        if STOPPING.load(Ordering::SeqCst) && !GATED.load(Ordering::SeqCst) {
            drop(g);
            continue;
        }
        FG_INT.store(false, Ordering::SeqCst);
        let spawned = c.spawn();
        drop(c);
        let mut ch = match spawned {
            Ok(ch) => ch,
            Err(e) => {
                drop(g);
                break Err(e);
            }
        };
        FG_ACTIVE.store(!int_ignored, Ordering::SeqCst);
        FG_PID.store(i32::try_from(ch.id()).unwrap_or(0), Ordering::SeqCst);
        let t = with(&mut ch);
        let st = end(&mut ch);
        FG_PID.store(0, Ordering::SeqCst);
        FG_ACTIVE.store(false, Ordering::SeqCst);
        drop(g);
        break Ok((t, st));
    };
    park_while_stopping();
    r
}

/// run_child for a status: bash's `$?` (128+N for signal N).
pub fn run_status(c: Command) -> std::io::Result<i32> {
    run_child(c, false, |_| ()).map(|(_, (rc, _))| rc)
}

/// run_child for `$(...)`-like output: its stdout read whole (a pipe of its own), and its status.
pub fn run_output(mut c: Command) -> std::io::Result<(i32, Vec<u8>)> {
    c.stdout(std::process::Stdio::piped());
    run_child(c, false, |ch| {
        let mut out = Vec::new();
        if let Some(mut o) = ch.stdout.take() {
            let _ = std::io::Read::read_to_end(&mut o, &mut out);
        }
        out
    })
    .map(|(out, (rc, _))| (rc, out))
}

/// run_output for a `$(...)` of bash's: a substitution's step (subst), so an INT to omabox alone
/// during it ends this process of it once the step is over (bash: 130 at once; the subshell's child
/// runs on), where a foreground child's lets main go on.
pub fn run_subst(c: Command) -> std::io::Result<(i32, Vec<u8>)> {
    subst(|| run_output(c))
}

/// Main waits here while a trap is on its way (it ends this process, or lets main go on); another
/// thread (a trap's undo) never does.
fn park_while_stopping() {
    if !main_step() {
        return;
    }
    while STOPPING.load(Ordering::SeqCst) && !GATED.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// A foreground child is running (FG_ACTIVE), and one of the children it waited for died of SIGINT
/// (FG_INT, exec::status): bash's test for an INT to itself alone, which it lets pass when the child
/// it waited for did not die of it.
pub static FG_ACTIVE: AtomicBool = AtomicBool::new(false);
pub static FG_INT: AtomicBool = AtomicBool::new(false);
/// The pid of main's foreground child while it runs (0: none), for a signal's stop that waited
/// for it long enough (trap.rs's foreground_over).
pub static FG_PID: AtomicI32 = AtomicI32::new(0);

/// DEFER_INT: main runs what bash runs in a `$(...)` subshell (monitor_add_window): an INT to omabox
/// alone ends bash's main at once (its read of the substitution is cut short, 130) while the subshell,
/// which got no signal, finishes the step. INT_DEFERRED: one came; the step goes on, then this ends
/// of it, having said nothing more.
pub static DEFER_INT: AtomicBool = AtomicBool::new(false);
pub static INT_DEFERRED: AtomicBool = AtomicBool::new(false);
/// An interactive monitor add (cmd/monitor.rs, finding 437): any INT ends it, its trap removing the
/// output, where bash's main ends of one to itself alone while its subshell finishes the add. A
/// group Ctrl-C during the create looks like one to omabox alone (timeout's hyprctl, a group of its
/// own, outlives it), and was taken for that: the add finished, its monitor kept, exit 130.
pub static INT_ENDS: AtomicBool = AtomicBool::new(false);

/// F as a step bash runs in a `$(...)`: its main waits for the substitution, so an INT to omabox
/// alone meanwhile lets the step finish, then ends this process of it (int_deferred_end).
pub fn subst<T>(f: impl FnOnce() -> T) -> T {
    // (a trap's undo, on its thread or main's, runs it plainly: an undo's `$(...)` must neither
    // judge main's INT nor end the process from inside the undos, whose lock it holds)
    if !main_step() {
        return f();
    }
    let outer = DEFER_INT.swap(true, Ordering::SeqCst);
    let r = f();
    if !outer {
        DEFER_INT.store(false, Ordering::SeqCst);
        int_deferred_end();
    }
    r
}

/// An INT deferred (INT_DEFERRED) ends this process now, of SIGINT, as bash's main did.
pub fn int_deferred_end() {
    if INT_DEFERRED.load(Ordering::SeqCst) {
        // (a trap on its way ends this process itself; else bash's EXIT trap runs on the way, its
        // undos: a temp file of record stop's, finding 432)
        park_while_stopping();
        crate::trap::run_undos();
        // SAFETY: the default disposition back, then the signal to this process: it ends of it.
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::kill(libc::getpid(), libc::SIGINT);
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
        std::process::exit(130);
    }
}

/// A handler's errno given back as it found it.
pub struct Errno(libc::c_int);

impl Errno {
    pub fn save() -> Errno {
        // SAFETY: errno is this thread's; reading it is async-signal-safe.
        Errno(unsafe { *libc::__errno_location() })
    }
}

impl Drop for Errno {
    fn drop(&mut self) {
        // SAFETY: as in save: this thread's errno set back.
        unsafe { *libc::__errno_location() = self.0 }
    }
}

/// The write end of the pipe a signal handler tells a watcher thread through.
static SIG_PIPE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn to_pipe(sig: libc::c_int) {
    let _e = Errno::save();
    let fd = SIG_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        let b = [u8::try_from(sig).unwrap_or(0)];
        // SAFETY: write is async-signal-safe; b outlives the call.
        unsafe {
            libc::write(fd, b.as_ptr().cast(), 1);
        }
    }
}

const TERMINATING: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// Whether SIG is ignored now (as it came: this process ignores none itself).
pub fn ignored(sig: libc::c_int) -> bool {
    // SAFETY: a query of the disposition into a zeroed sigaction of ours.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(sig, std::ptr::null(), &mut old) == 0 && old.sa_sigaction == libc::SIG_IGN
    }
}

/// `trap ... SIG` as a non-interactive bash sets it: HANDLER, unless SIG was ignored when this
/// omabox started (nohup, a `&` of a script's), which bash cannot trap: then it stays ignored.
pub fn catch(sig: libc::c_int, handler: extern "C" fn(libc::c_int)) {
    if ignored(sig) {
        return;
    }
    // SAFETY: the handlers given here only make async-signal-safe calls.
    unsafe {
        libc::signal(sig, handler as *const () as libc::sighandler_t);
    }
}

/// The box `run` started for its command, taken down once (`out=$(cmd_down NAME 2>&1) || say "$out"`).
pub struct Teardown {
    name: Vec<u8>,
    done: Arc<Mutex<bool>>,
}

impl Teardown {
    /// From now until finish: a terminating signal takes the box down, then ends this process as the
    /// signal would have.
    pub fn new(name: Vec<u8>) -> Teardown {
        let done = Arc::new(Mutex::new(false));
        if let Ok((r, w)) = crate::exec::pipe() {
            SIG_PIPE.store(w.as_raw_fd(), Ordering::Relaxed);
            std::mem::forget(w);
            for s in TERMINATING {
                catch(s, to_pipe);
            }
            let (n, d) = (name.clone(), done.clone());
            std::thread::spawn(move || {
                let mut f = std::fs::File::from(r);
                let mut b = [0u8; 1];
                if f.read_exact(&mut b).is_ok() {
                    down(&n, &d);
                    let sig = libc::c_int::from(b[0]);
                    // SAFETY: back to the default action, then the signal again: this process ends
                    // as it would have.
                    unsafe {
                        libc::signal(sig, libc::SIG_DFL);
                        libc::raise(sig);
                    }
                }
            });
        }
        Teardown { name, done }
    }

    pub fn finish(self) {
        down(&self.name, &self.done);
    }
}

fn down(name: &[u8], done: &Mutex<bool>) {
    let Ok(mut d) = done.lock() else { return };
    if *d {
        return;
    }
    *d = true;
    let Ok((r, w)) = crate::exec::pipe() else {
        return;
    };
    let mut c = omabox_cmd();
    c.arg("down").arg(OsStr::from_bytes(name));
    let Ok(w2) = w.try_clone() else { return };
    c.stdin(Stdio::null())
        .stdout(Stdio::from(w))
        .stderr(Stdio::from(w2));
    let child = c.spawn();
    drop(c);
    let out = crate::exec::read_all(r, None);
    let ok = child
        .and_then(|mut ch| ch.wait())
        .is_ok_and(|s| s.success());
    if !ok {
        say(cat!(crate::shell::subst(&out)));
    }
}

/// A follower's processes (follow_start), ended with this one: SIGTERM to each, then exit 130 (INT),
/// 143 (TERM) or 129 (HUP).
static FOLLOWERS: [AtomicI32; 8] = [const { AtomicI32::new(0) }; 8];
static FOLLOWING: AtomicBool = AtomicBool::new(false);

extern "C" fn end_followers(sig: libc::c_int) {
    for p in &FOLLOWERS {
        let pid = p.load(Ordering::Relaxed);
        if pid > 0 {
            // SAFETY: kill is async-signal-safe.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
    let code = match sig {
        libc::SIGINT => 130,
        libc::SIGTERM => 143,
        _ => 129,
    };
    // SAFETY: _exit is async-signal-safe; nothing is left to flush (each line was).
    unsafe { libc::_exit(code) }
}

/// From now, a terminating signal ends the follower's processes, then this one (follow_start's traps).
pub fn follow_on() {
    if !FOLLOWING.swap(true, Ordering::Relaxed) {
        for s in TERMINATING {
            catch(s, end_followers);
        }
    }
}

/// A child just started (held_run's nsenter): one of the follower's while it follows.
pub fn spawned(pid: u32) {
    if FOLLOWING.load(Ordering::Relaxed) {
        follower(pid);
    }
}

/// Registers PID as one of the follower's processes.
pub fn follower(pid: u32) {
    let pid = i32::try_from(pid).unwrap_or(0);
    for p in &FOLLOWERS {
        if p.compare_exchange(0, pid, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            break;
        }
    }
}

/// Signal cases run in a process of their own (they end it): the test binary again, this one test
/// alone, OMABOX_SIG_CASE naming it; its status and stderr, or None when it hung (KILLed at 15 s).
#[cfg(test)]
mod signal_cases {
    use std::os::unix::process::ExitStatusExt;

    const CASE: &str = "OMABOX_SIG_CASE";

    /// Some((code or 128+signal, stderr)) from the parent; None in the child alone, which runs the
    /// case (a child that cannot be started fails the test, never runs the case in the parent).
    fn in_child(name: &str) -> Option<Option<(i32, String)>> {
        if std::env::var(CASE).as_deref() == Ok(name) {
            return None;
        }
        let exe = std::env::current_exe().expect("the test binary's path");
        let mut ch = std::process::Command::new(exe)
            .args([
                &format!("sig::signal_cases::{name}"),
                "--exact",
                "--nocapture",
            ])
            .args(["--test-threads=1"])
            .env(CASE, name)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the case's child");
        let err = ch.stderr.take().expect("the case's stderr");
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut { err }, &mut s);
            s
        });
        let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let st = loop {
            if let Ok(Some(st)) = ch.try_wait() {
                break Some(st);
            }
            if std::time::Instant::now() > end {
                let _ = ch.kill();
                let _ = ch.wait();
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let err = reader.join().unwrap_or_default();
        Some(st.map(|s| {
            (
                s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
                err,
            )
        }))
    }

    fn sleep_child(s: &str) -> std::io::Result<i32> {
        let mut c = super::command("sleep");
        c.arg(s);
        super::run_status(c)
    }

    /// SIG to this process alone at each AT (ms from now).
    fn kill_at(at: &[(u64, i32)]) {
        let pid = i32::try_from(std::process::id()).unwrap_or(0);
        let v = at.to_vec();
        std::thread::spawn(move || {
            let mut t = 0;
            for (at, sig) in v {
                std::thread::sleep(std::time::Duration::from_millis(at - t));
                t = at;
                // SAFETY: a signal to this test's own process.
                unsafe { libc::kill(pid, sig) };
            }
        });
    }

    /// A code trap (record stop's) and a TERM to omabox alone during a foreground child, an INT
    /// 200 ms on, still in it: the child's end, the undo, then the first signal's 143, nothing more
    /// of the command (bash ran the lowest-numbered pending trap's code, 130: finding 452).
    #[test]
    fn code_trap_first_signal_wins() {
        let Some(r) = in_child("code_trap_first_signal_wins") else {
            super::set_main();
            let _t = crate::trap::on_exit_code(|| eprintln!("CASE undo"));
            kill_at(&[(200, libc::SIGTERM), (400, libc::SIGINT)]);
            let _ = sleep_child("0.6");
            eprintln!("CASE after the child");
            return;
        };
        let (rc, err) = r.expect("it hung");
        assert_eq!(rc, 143, "{err}");
        assert_eq!(err.matches("CASE undo").count(), 1, "{err}");
        assert!(!err.contains("CASE after"), "{err}");
    }

    /// A second signal while the undo runs (its own child, 0.6 s): the undo finishes, then this
    /// process ends of the first (bash's trap died of the second, its undo cut short: finding 452).
    /// For a trap with an exit code and one without.
    fn second_during_undo(name: &str, code: bool) -> Option<(i32, String)> {
        let Some(r) = in_child(name) else {
            super::set_main();
            let undo = || {
                eprintln!("CASE undo begun");
                let _ = sleep_child("0.6");
                eprintln!("CASE undo over");
            };
            let _t = if code {
                crate::trap::on_exit_code(undo)
            } else {
                crate::trap::on_exit(undo)
            };
            kill_at(&[
                (200, libc::SIGTERM),
                (500, libc::SIGINT),
                (600, libc::SIGHUP),
            ]);
            std::thread::sleep(std::time::Duration::from_secs(3));
            eprintln!("CASE after the wait");
            return None;
        };
        Some(r.expect("it hung"))
    }

    #[test]
    fn second_signal_during_the_undo() {
        let Some((rc, err)) = second_during_undo("second_signal_during_the_undo", false) else {
            return;
        };
        assert_eq!(rc, 143, "{err}");
        assert!(err.contains("CASE undo over"), "{err}");
        assert!(!err.contains("CASE after"), "{err}");
    }

    #[test]
    fn second_signal_during_a_code_undo() {
        let Some((rc, err)) = second_during_undo("second_signal_during_a_code_undo", true) else {
            return;
        };
        assert_eq!(rc, 143, "{err}");
        assert!(err.contains("CASE undo over"), "{err}");
        assert!(!err.contains("CASE after"), "{err}");
    }

    /// A TERM to omabox alone during a foreground child (a check run's `up`): the trap waits for it,
    /// so the undo sees what it did (bash ran its EXIT trap at once, the child left running: finding
    /// 453); then 143.
    #[test]
    fn term_waits_for_the_foreground_child() {
        let f = std::env::temp_dir().join(format!("omabox-sig-fg.{}", std::process::id()));
        let Some(r) = in_child("term_waits_for_the_foreground_child") else {
            super::set_main();
            let g = f.clone();
            let _t = crate::trap::on_exit(move || {
                let seen = if g.exists() {
                    "the child's file"
                } else {
                    "nothing"
                };
                eprintln!("CASE undo saw {seen}");
            });
            kill_at(&[(150, libc::SIGTERM)]);
            let mut c = super::command("sh");
            c.args(["-c", "sleep 0.5; : > \"$0\"", &f.to_string_lossy()]);
            let _ = super::run_status(c);
            eprintln!("CASE after the child");
            return;
        };
        let (rc, err) = r.expect("it hung");
        let _ = std::fs::remove_file(&f);
        assert_eq!(rc, 143, "{err}");
        assert!(err.contains("CASE undo saw the child's file"), "{err}");
        assert!(!err.contains("CASE after"), "{err}");
    }

    /// A foreground child that does not end: waited for FG_GRACE[0], sent a TERM it ignores, then a
    /// KILL; the undo runs and this ends of its signal, never hanging on it (finding 453).
    #[test]
    fn a_child_that_will_not_end_is_stopped() {
        let Some(r) = in_child("a_child_that_will_not_end_is_stopped") else {
            super::set_main();
            let _t = crate::trap::on_exit_code(|| eprintln!("CASE undo"));
            kill_at(&[(150, libc::SIGHUP)]);
            let mut c = super::command("sh");
            c.args(["-c", "trap '' TERM; exec sleep 30"]);
            let rc = super::run_status(c);
            eprintln!("CASE after the child {rc:?}");
            return;
        };
        let t0 = std::time::Instant::now();
        let (rc, err) = r.expect("it hung");
        assert_eq!(rc, 129, "{err}");
        assert!(err.contains("CASE undo"), "{err}");
        assert!(!err.contains("CASE after"), "{err}");
        assert!(t0.elapsed() < std::time::Duration::from_secs(8), "{err}");
    }

    /// monitor add's shape: a DEFER_INT step, an INT to omabox alone in it, int_deferred_end's undo
    /// on main spawning, a TERM during that undo's first child: it ends (it deadlocked: main parked
    /// in its undo, the thread waiting for that undo).
    #[test]
    fn undo_on_main_and_a_term() {
        let Some(r) = in_child("undo_on_main_and_a_term") else {
            super::set_main();
            let _t = crate::trap::on_exit(|| {
                let _ = sleep_child("0.6");
                let _ = sleep_child("0.1");
                eprintln!("CASE undo over");
            });
            kill_at(&[(200, libc::SIGINT), (800, libc::SIGTERM)]);
            super::DEFER_INT.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = sleep_child("0.5");
            super::DEFER_INT.store(false, std::sync::atomic::Ordering::SeqCst);
            super::int_deferred_end();
            eprintln!("CASE past int_deferred_end");
            return;
        };
        let (rc, err) = r.expect("it hung");
        assert!(rc == 130 || rc == 143, "{rc} {err}");
        assert!(err.contains("CASE undo over"), "{err}");
        assert!(!err.contains("CASE past"), "{err}");
    }

    /// int_deferred_end's undo on main with a `$(...)` step in it (host_session's, a rule-off's):
    /// it runs plainly, the undo ends, then 130 (subst's own int_deferred_end there would have
    /// taken the undos' lock main already held).
    #[test]
    fn undo_on_main_with_a_subst() {
        let Some(r) = in_child("undo_on_main_with_a_subst") else {
            super::set_main();
            let _t = crate::trap::on_exit(|| {
                let mut c = super::command("sleep");
                c.arg("0.1");
                let _ = super::run_subst(c);
                eprintln!("CASE undo over");
            });
            kill_at(&[(200, libc::SIGINT)]);
            super::DEFER_INT.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = sleep_child("0.5");
            super::DEFER_INT.store(false, std::sync::atomic::Ordering::SeqCst);
            super::int_deferred_end();
            eprintln!("CASE past int_deferred_end");
            return;
        };
        let (rc, err) = r.expect("it hung");
        assert_eq!(rc, 130, "{err}");
        assert!(err.contains("CASE undo over"), "{err}");
        assert!(!err.contains("CASE past"), "{err}");
    }

    /// A `$(...)` step (run_subst): an INT to omabox alone during it ends this process of it once
    /// the step is over (130); run_child's foreground child lets main go on.
    #[test]
    fn subst_step_and_int_alone() {
        let Some(r) = in_child("subst_step_and_int_alone") else {
            super::set_main();
            crate::trap::int_as_bash();
            kill_at(&[(200, libc::SIGINT)]);
            let mut c = super::command("sleep");
            c.arg("0.5");
            let _ = super::run_subst(c);
            eprintln!("CASE past the substitution");
            return;
        };
        let (rc, err) = r.expect("it hung");
        assert_eq!(rc, 130, "{err}");
        assert!(!err.contains("CASE past"), "{err}");
        let Some(r) = in_child("foreground_step_and_int_alone") else {
            return;
        };
        let (rc, err) = r.expect("it hung");
        assert_eq!(rc, 0, "{err}");
        assert!(err.contains("CASE past the step"), "{err}");
    }

    #[test]
    fn foreground_step_and_int_alone() {
        if std::env::var(CASE).as_deref() != Ok("foreground_step_and_int_alone") {
            return;
        }
        super::set_main();
        crate::trap::int_as_bash();
        kill_at(&[(200, libc::SIGINT)]);
        let _ = sleep_child("0.5");
        eprintln!("CASE past the step");
    }
}
