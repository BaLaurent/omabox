//! bash's EXIT trap for what a command made on its way (gpu's and cpu's temporary dir, a host rule
//! monitor add turned on): undone when the command ends, however it ends, and on INT, TERM or HUP,
//! the signal then raised again: bash runs its EXIT trap and dies of the signal (probed: at once for
//! TERM and HUP, its foreground child left running; for INT once that child is over, and not at all
//! when that child did not die of it: an INT to omabox alone). Nothing new starts meanwhile
//! (sig::foreground). A signal ignored when omabox started stays ignored (sig::catch).

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

type Undo = Box<dyn FnOnce() + Send>;

static TRAPS: Mutex<Vec<(u64, Undo)>> = Mutex::new(Vec::new());
/// Traps that wait for main's foreground child whatever the signal (on_exit_fg).
static FG_TRAPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static NEXT: AtomicU64 = AtomicU64::new(1);
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

/// on_exit, the undo once main's foreground child is over on TERM and HUP too: one whose step in
/// flight must land first (monitor add's rule-on call, finding 429).
pub fn on_exit_fg(f: impl FnOnce() + Send + 'static) -> Trap {
    FG_TRAPS.fetch_add(1, Ordering::SeqCst);
    let fg = FgCount;
    on_exit(move || {
        drop(fg);
        f();
    })
}

/// on_exit with bash's `trap 'exit 130' INT; trap 'exit 143' TERM; trap 'exit 129' HUP` beside it
/// (rec_traps, finding 320's): on any of them the undo runs once main's foreground child is over,
/// whatever it died of, then this process exits with that code (not of the signal).
pub fn on_exit_code(f: impl FnOnce() + Send + 'static) -> Trap {
    CODE_TRAPS.fetch_add(1, Ordering::SeqCst);
    FG_TRAPS.fetch_add(1, Ordering::SeqCst);
    let c = (CodeCount, FgCount);
    on_exit(move || {
        drop(c);
        f();
    })
}

/// Every undo still pending, the last made first (bash's EXIT trap on the way out).
pub fn run_undos() {
    let all: Vec<(u64, Undo)> = TRAPS
        .lock()
        .map(|mut t| std::mem::take(&mut *t))
        .unwrap_or_default();
    for (_, u) in all.into_iter().rev() {
        u();
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

/// FG_TRAPS' share of one on_exit_fg trap, given back when it is done with or dropped.
struct FgCount;

impl Drop for FgCount {
    fn drop(&mut self) {
        FG_TRAPS.fetch_sub(1, Ordering::SeqCst);
    }
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
    // Nothing new starts from here (a code trap inside a `$(...)` lets the substitution finish
    // first, sig::subst).
    if !(CODE_TRAPS.load(Ordering::SeqCst) > 0
        && crate::sig::SUBST_DEPTH.load(Ordering::SeqCst) > 0)
    {
        crate::sig::STOPPING.store(true, Ordering::SeqCst);
    }
    let fd = SIG_W.load(Ordering::Relaxed);
    if fd >= 0 {
        let b = [u8::try_from(sig).unwrap_or(0)];
        // SAFETY: write is async-signal-safe; b outlives the call.
        unsafe {
            libc::write(fd, b.as_ptr().cast(), 1);
        }
    }
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
            let mut held = None;
            // A code trap during a `$(...)`: when the child running then did not die of the signal
            // (one to omabox alone), bash runs the trap once the whole substitution is over (probed:
            // `trap 'exit 143' TERM` during `x=$(a; b)` exits after b): main finishes it, then
            // parks. A group signal kills bash's subshell and bash ends at once (probed); so does one
            // that came with no child running.
            if code && crate::sig::SUBST_DEPTH.load(Ordering::SeqCst) > 0 {
                let g = busy_at.then(lock_fg);
                if g.is_some() && crate::sig::FG_SIG.load(Ordering::SeqCst) != sig {
                    drop(g);
                    crate::sig::SUBST_STOP.store(true, Ordering::SeqCst);
                    while crate::sig::SUBST_DEPTH.load(Ordering::SeqCst) > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                } else {
                    crate::sig::STOPPING.store(true, Ordering::SeqCst);
                    held = g;
                }
            }
            crate::sig::STOPPING.store(true, Ordering::SeqCst);
            let wait_fg = sig == libc::SIGINT || FG_TRAPS.load(Ordering::SeqCst) > 0;
            let fg = match held {
                Some(g) => Some(g),
                None => wait_fg.then(lock_fg),
            };
            // An INT to omabox alone while a child ran that did not die of it: bash goes on (probed:
            // `kill -INT` of the script during a sleep, the rest of it ran, its EXIT trap at its end).
            if !code && sig == libc::SIGINT && busy && !crate::sig::FG_INT.load(Ordering::SeqCst) {
                if crate::sig::DEFER_INT.load(Ordering::SeqCst) {
                    crate::sig::INT_DEFERRED.store(true, Ordering::SeqCst);
                }
                crate::sig::STOPPING.store(false, Ordering::SeqCst);
                drop(fg);
                continue;
            }
            run_undos();
            if code {
                // (bash's `exit 130|143|129` in its trap, its EXIT trap run on the way)
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
    crate::sig::foreground(|| c.status().map(|s| crate::exec::status(s).0).unwrap_or(127))
}
