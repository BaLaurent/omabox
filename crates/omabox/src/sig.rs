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

/// A terminating signal's trap is on its way (shot --no-pointer's release); GATED: main holds the
/// pointer's gate (ptr.rs), where it must not stop.
pub static STOPPING: AtomicBool = AtomicBool::new(false);
pub static GATED: AtomicBool = AtomicBool::new(false);

/// F as a foreground command: on main's thread, with FOREGROUND held; once it is over, a trap on its
/// way runs before anything more (main waits there for it to end this process), as bash's does.
pub fn foreground<T>(f: impl FnOnce() -> T) -> T {
    if MAIN.get() != Some(&std::thread::current().id()) {
        return f();
    }
    // Nothing new starts once a trap is on its way, as bash starts nothing after its trap: a TERM
    // between two steps of monitor add must not see the host rule turned off and the window made.
    park_while_stopping();
    let r = {
        let _g = FOREGROUND
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        FG_INT.store(false, Ordering::SeqCst);
        FG_ACTIVE.store(true, Ordering::SeqCst);
        let r = f();
        FG_ACTIVE.store(false, Ordering::SeqCst);
        r
    };
    park_while_stopping();
    r
}

/// Main waits here while a trap is on its way (it ends this process, or lets main go on); another
/// thread (a trap's undo) never does.
fn park_while_stopping() {
    if MAIN.get() != Some(&std::thread::current().id()) {
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

/// DEFER_INT: main runs what bash runs in a `$(...)` subshell (monitor_add_window): an INT to omabox
/// alone ends bash's main at once (its read of the substitution is cut short, 130) while the subshell,
/// which got no signal, finishes the step. INT_DEFERRED: one came; the step goes on, then this ends
/// of it, having said nothing more.
pub static DEFER_INT: AtomicBool = AtomicBool::new(false);
pub static INT_DEFERRED: AtomicBool = AtomicBool::new(false);

/// An INT deferred (INT_DEFERRED) ends this process now, of SIGINT, as bash's main did.
pub fn int_deferred_end() {
    if INT_DEFERRED.load(Ordering::SeqCst) {
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
