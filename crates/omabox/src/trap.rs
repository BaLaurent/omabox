//! bash's EXIT trap for what a command made on its way (gpu's and cpu's temporary dir, the monitor
//! an interactive `monitor add` was making): undone when the command ends, however it ends, and on INT, TERM or HUP,
//! the signal then raised again: bash runs its EXIT trap and dies of the signal (probed: at once for
//! TERM and HUP, its foreground child left running; for INT once that child is over, and not at all
//! when that child did not die of it: an INT to omabox alone). A second INT, TERM or HUP while the
//! trap runs ends it there, as it ends bash's (probed). Nothing new starts meanwhile
//! (sig::foreground). A signal ignored when omabox started stays ignored (sig::catch).

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

type Undo = Box<dyn FnOnce() + Send>;

static TRAPS: Mutex<Vec<(u64, Undo)>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);
static SIG_W: AtomicI32 = AtomicI32::new(-1);

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

extern "C" fn on_signal(sig: libc::c_int) {
    let _e = crate::sig::Errno::save();
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
            let busy = crate::sig::FG_ACTIVE.load(Ordering::SeqCst);
            crate::sig::STOPPING.store(true, Ordering::SeqCst);
            let fg = (sig == libc::SIGINT).then(|| {
                crate::sig::FOREGROUND
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            });
            // An INT to omabox alone while a child ran that did not die of it: bash goes on (probed:
            // `kill -INT` of the script during a sleep, the rest of it ran, its EXIT trap at its end).
            if sig == libc::SIGINT && busy && !crate::sig::FG_INT.load(Ordering::SeqCst) {
                if crate::sig::DEFER_INT.load(Ordering::SeqCst) {
                    crate::sig::INT_DEFERRED.store(true, Ordering::SeqCst);
                }
                crate::sig::STOPPING.store(false, Ordering::SeqCst);
                drop(fg);
                continue;
            }
            // A second one while the undo runs ends this process at once, as it ends bash's trap.
            for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                if !crate::sig::ignored(s) {
                    // SAFETY: the default disposition for a signal this process caught.
                    unsafe {
                        libc::signal(s, libc::SIG_DFL);
                    }
                }
            }
            let all: Vec<(u64, Undo)> = TRAPS
                .lock()
                .map(|mut t| std::mem::take(&mut *t))
                .unwrap_or_default();
            for (_, u) in all.into_iter().rev() {
                u();
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

/// `sleep SECS` as bash runs it: a child in the foreground (a signal's trap waits for it).
pub fn sleep(secs: &[u8]) -> i32 {
    use std::os::unix::ffi::OsStrExt;
    crate::sig::foreground(|| {
        crate::sig::command("sleep")
            .arg(std::ffi::OsStr::from_bytes(secs))
            .status()
            .map(|s| crate::exec::status(s).0)
            .unwrap_or(127)
    })
}
