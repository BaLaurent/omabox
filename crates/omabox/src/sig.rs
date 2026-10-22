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
    Command::new(crate::root().join("bin/omabox"))
}

/// The write end of the pipe a signal handler tells a watcher thread through.
static SIG_PIPE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn to_pipe(sig: libc::c_int) {
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
