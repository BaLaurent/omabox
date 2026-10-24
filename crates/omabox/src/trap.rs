//! bash's EXIT trap for what a command made on its way (gpu's and cpu's temporary dir, the monitor
//! an interactive `monitor add` was making): undone when the command ends, however it ends, and on
//! INT, TERM or HUP, the signal then raised again, as bash dies of it after its EXIT trap. A code
//! trap's (on_exit_code) ends with the signal's 128+N instead. Before the undo, main's foreground
//! child is waited for, bounded (foreground_over: bash ran its EXIT trap at once on TERM and HUP,
//! the child left running, finding 453); an INT to omabox alone that this child outlives lets the
//! command go on, as bash does. The first signal decides: once its undo begins, another INT, TERM
//! or HUP changes nothing, and the undo finishes (bash's second signal cut its trap short, or ran
//! the lowest-numbered pending trap's code: finding 452). Nothing new starts meanwhile
//! (sig::run_child). A signal ignored when omabox started stays ignored (sig::catch).

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

type Undo = Box<dyn FnOnce() + Send>;

static TRAPS: Mutex<Vec<(u64, Undo)>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);
/// The signal thread holds sig::FOREGROUND while it undoes (an INT's, which waited for that child).
static FG_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SIG_W: AtomicI32 = AtomicI32::new(-1);

/// Traps that end with an exit code (`trap 'exit 143' TERM`), not the signal: on_exit_code's.
static CODE_TRAPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One thing to undo; undone when dropped (the command's end, a refusal on the way).
pub struct Trap(u64);

/// Undo F when the command ends, or on INT, TERM or HUP.
pub fn on_exit(f: impl FnOnce() + Send + 'static) -> Trap {
    arm();
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut t) = TRAPS.lock() {
        t.push((id, Box::new(f)));
    }
    Trap(id)
}

/// on_exit with bash's `trap 'exit 130' INT; trap 'exit 143' TERM; trap 'exit 129' HUP` beside it
/// (rec_traps, finding 320's): on any of them the undo runs once main's foreground child is over,
/// whatever it died of, then this process exits with that code (not of the signal).
pub fn on_exit_code(f: impl FnOnce() + Send + 'static) -> Trap {
    CODE_TRAPS.fetch_add(1, Ordering::SeqCst);
    let c = CodeCount;
    on_exit(move || {
        drop(c);
        f();
    })
}

/// Every undo still pending, the last made first (bash's EXIT trap on the way out).
pub fn run_undos() {
    // (one at a time: main's int_deferred_end waits for a signal's undos the thread runs, so the
    // process never ends of its INT halfway through them)
    static UNDOING: Mutex<()> = Mutex::new(());
    let _u = UNDOING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let all: Vec<(u64, Undo)> = TRAPS
        .lock()
        .map(|mut t| std::mem::take(&mut *t))
        .unwrap_or_default();
    // (main's undos run their children plainly, never parked: sig::UNDO_MAIN)
    let main = crate::sig::on_main();
    if main {
        crate::sig::UNDO_MAIN.store(true, Ordering::SeqCst);
    }
    for (_, u) in all.into_iter().rev() {
        u();
    }
    if main {
        crate::sig::UNDO_MAIN.store(false, Ordering::SeqCst);
    }
}

/// Whether an on_exit_code trap is on (a `trap 'exit 143' TERM` of bash's is set).
pub fn code_active() -> bool {
    CODE_TRAPS.load(Ordering::SeqCst) > 0
}

/// CODE_TRAPS' share of one on_exit_code trap.
struct CodeCount;

impl Drop for CodeCount {
    fn drop(&mut self) {
        CODE_TRAPS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// For an undo whose step in flight must land first (monitor add's remove after its create, finding
/// 429): wait until main's foreground child is over, unless it already is (an INT's undo, a code
/// trap's, a refusal's on main). Waiting is not holding: a second INT, TERM or HUP meanwhile ends
/// this process at once (with no code trap on: those hold the foreground and finish their undo).
pub fn wait_foreground() {
    if FG_HELD.load(Ordering::SeqCst) {
        return;
    }
    drop(
        crate::sig::FOREGROUND
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
}

impl Trap {
    /// Done with it: nothing left to undo.
    pub fn forget(self) {
        take(self.0);
        std::mem::forget(self);
    }
}

impl Drop for Trap {
    fn drop(&mut self) {
        if let Some(f) = take(self.0) {
            f();
        }
    }
}

fn take(id: u64) -> Option<Undo> {
    let mut t = TRAPS.lock().ok()?;
    let i = t.iter().position(|(k, _)| *k == id)?;
    Some(t.remove(i).1)
}

/// Whether a foreground child ran when the signal came, read in the handler: the thread wakes later,
/// when that step may be over and an in-process one begun (where bash, had it been there, would end
/// at once: the safe side).
static SIG_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_signal(sig: libc::c_int) {
    let _e = crate::sig::Errno::save();
    let _ = sig;
    SIG_BUSY.store(
        crate::sig::FG_ACTIVE.load(Ordering::SeqCst),
        Ordering::SeqCst,
    );
    // Nothing new starts from here (the thread lets main go on where bash would: an INT to omabox
    // alone; it clears this then).
    crate::sig::STOPPING.store(true, Ordering::SeqCst);
    let fd = SIG_W.load(Ordering::Relaxed);
    if fd >= 0 {
        let b = [u8::try_from(sig).unwrap_or(0)];
        // SAFETY: write is async-signal-safe; b outlives the call.
        unsafe {
            libc::write(fd, b.as_ptr().cast(), 1);
        }
    }
}

/// How long a signal's stop waits for main's foreground child, then once more after a TERM to it,
/// then after a KILL (each step's end shortened in the unit tests).
#[cfg(not(test))]
const FG_GRACE: [std::time::Duration; 3] = [
    std::time::Duration::from_secs(10),
    std::time::Duration::from_secs(5),
    std::time::Duration::from_secs(5),
];
#[cfg(test)]
const FG_GRACE: [std::time::Duration; 3] = [
    std::time::Duration::from_millis(1000),
    std::time::Duration::from_millis(500),
    std::time::Duration::from_millis(500),
];

/// Main's foreground child over (sig::FOREGROUND held), so the undo comes after what that step did
/// (check run's down after its box's up; gpu's and cpu's dir removed once nothing writes to it): at
/// most FG_GRACE[0], then the child is sent a TERM (not at once: a child that is itself omabox would
/// get the signal in its own cleanup), FG_GRACE[1] more, then a KILL. None when even then main has not
/// let go (what the child left running holds its pipes): the undo goes on regardless (finding 453).
fn foreground_over() -> Option<std::sync::MutexGuard<'static, ()>> {
    for (i, grace) in FG_GRACE.iter().enumerate() {
        let end = std::time::Instant::now() + *grace;
        loop {
            match crate::sig::FOREGROUND.try_lock() {
                Ok(g) => return Some(g),
                Err(std::sync::TryLockError::Poisoned(p)) => return Some(p.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => {}
            }
            if std::time::Instant::now() >= end {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let pid = crate::sig::FG_PID.load(Ordering::SeqCst);
        if let (true, Some(s)) = (pid > 0, [libc::SIGTERM, libc::SIGKILL].get(i)) {
            // SAFETY: a signal to main's foreground child (run_child clears FG_PID once it reaps it).
            unsafe {
                libc::kill(pid, *s);
            }
        }
    }
    None
}

/// The handlers and the thread that undoes on a signal, once.
fn arm() {
    if SIG_W.load(Ordering::SeqCst) >= 0 {
        return;
    }
    let Ok((r, w)) = crate::exec::pipe() else {
        return;
    };
    use std::os::fd::IntoRawFd;
    SIG_W.store(w.into_raw_fd(), Ordering::SeqCst);
    for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        crate::sig::catch(s, on_signal);
    }
    std::thread::spawn(move || {
        let mut f = std::fs::File::from(r);
        loop {
            let mut b = [0u8; 1];
            if std::io::Read::read_exact(&mut f, &mut b).is_err() {
                return;
            }
            let sig = libc::c_int::from(b[0]);
            // (judged by what ran when it came: a foreground child, or main in-process)
            let busy_at = SIG_BUSY.swap(false, Ordering::SeqCst);
            let busy = sig == libc::SIGINT && busy_at;
            let code = CODE_TRAPS.load(Ordering::SeqCst) > 0;
            let lock_fg = || {
                crate::sig::FOREGROUND
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            };
            crate::sig::STOPPING.store(true, Ordering::SeqCst);
            // (an INT that may yet let main go on waits for the child as long as it runs: its end
            // says; every other signal stops the command, the child waited for a bounded time)
            let fg = if sig == libc::SIGINT && busy && !code {
                Some(lock_fg())
            } else {
                foreground_over()
            };
            // An INT to omabox alone while a child ran that did not die of it: bash goes on (probed:
            // `kill -INT` of the script during a sleep, the rest of it ran, its EXIT trap at its end).
            // In a DEFER_INT step (bash's `$(...)` subshell) only the first: bash's main has ended of
            // it, so a second one can only have reached the subshell, which ends of it, its EXIT trap
            // run (an interactive monitor add's remove, finding 429). Judged when it comes, a group
            // INT whose child outlives it (timeout's hyprctl, a group of its own) looks like one to
            // omabox alone; the second one ends the step here, the safe side.
            let second = crate::sig::DEFER_INT.load(Ordering::SeqCst)
                && crate::sig::INT_DEFERRED.load(Ordering::SeqCst);
            // (never in an interactive monitor add: sig::INT_ENDS)
            if !code
                && !second
                && !crate::sig::INT_ENDS.load(Ordering::SeqCst)
                && sig == libc::SIGINT
                && busy
                && !crate::sig::FG_INT.load(Ordering::SeqCst)
            {
                if crate::sig::DEFER_INT.load(Ordering::SeqCst) {
                    crate::sig::INT_DEFERRED.store(true, Ordering::SeqCst);
                }
                crate::sig::STOPPING.store(false, Ordering::SeqCst);
                drop(fg);
                continue;
            }
            // From here a second INT, TERM or HUP changes nothing: the thread reads no more of
            // them, and the undo finishes (finding 452).
            FG_HELD.store(fg.is_some(), Ordering::SeqCst);
            run_undos();
            if code {
                // (bash's `exit 130|143|129`, its EXIT trap run on the way: the first signal's)
                let _ = std::io::Write::flush(&mut std::io::stdout());
                // SAFETY: _exit ends this process at once, nothing left to run.
                unsafe { libc::_exit(128 + sig) }
            }
            // SAFETY: the default disposition back, then the signal to this process: it ends of it,
            // as bash does after its EXIT trap.
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, sig);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
                libc::kill(libc::getpid(), sig);
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
            std::process::exit(128 + sig);
        }
    });
}

/// No trap, but bash's way with a signal: TERM and HUP end this process at once, an INT once the
/// foreground child is over, and not at all when that child did not die of it (an INT to omabox
/// alone while gdb or a scenario runs: bash waits and goes on).
pub fn int_as_bash() {
    arm();
    // (once more, over another module's handlers: `trap - INT TERM HUP` after record start)
    for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        crate::sig::catch(s, on_signal);
    }
}

/// `sleep SECS` as bash runs it: a child in the foreground (a signal's trap waits for it).
pub fn sleep(secs: &[u8]) -> i32 {
    use std::os::unix::ffi::OsStrExt;
    // (made before the foreground step: sig::command parks, which must not hold FOREGROUND)
    let mut c = crate::sig::command("sleep");
    c.arg(std::ffi::OsStr::from_bytes(secs));
    crate::sig::run_status(c).unwrap_or(127)
}
