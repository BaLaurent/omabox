//! Entering a box, as bin/omabox does it: on_box (a tool against the box's compositor, in its user
//! and mount namespaces, finding 63) and box_exec (a command in all of its namespaces, with the
//! session's environment). Both through nsenter for now (#324: entering natively is a later step,
//! the pid namespace applying only to children made after setns). What nsenter itself says is held
//! apart from the command's own stderr and said after it (ns_held, ns_said: #166); a command in the
//! box's pid namespace gets no fd of the host's a box process could open again (finding 325: the
//! relays, a terminal of its own) and none but the ones it is given (finding 345: fds_only).

use crate::boxes::{self, BoxPid, box_env, box_pid, covers_held, run_read};
use crate::cat;
use crate::exit::{Ctx, Exit, Res};
use crate::jail;
use crate::jobs::job_how;
use crate::json;
use crate::procfs;
use crate::shell::{self, strip_nl};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// What ns_held keeps of nsenter's stderr, and how long it waits for the rest once the command is
/// over (a box process that keeps it open does not hold the call).
const NS_HELD_MAX: usize = 4096;
const NS_HELD_WAIT: Duration = Duration::from_secs(1);

// --- fds ------------------------------------------------------------------------------------------

/// A pipe, both ends close-on-exec.
pub fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: fds has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both were just opened and are ours alone.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// FD again, at 10 or above (as bash's {var} redirections), close-on-exec: a number a child is told
/// about, and that a dup2 to 3 never lands on.
pub fn dup_hi(fd: RawFd) -> std::io::Result<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC takes an int; it only reads the descriptor table.
    let n = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a new descriptor of ours.
    Ok(unsafe { OwnedFd::from_raw_fd(n) })
}

pub fn dev_null(write: bool) -> std::io::Result<OwnedFd> {
    let f = std::fs::OpenOptions::new()
        .read(!write)
        .write(write)
        .open("/dev/null")?;
    Ok(f.into())
}

fn fstat(fd: RawFd) -> Option<libc::stat> {
    // SAFETY: stat is plain data fstat fills in.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: st is ours; a closed fd is EBADF, nothing else.
    (unsafe { libc::fstat(fd, &mut st) } == 0).then_some(st)
}

fn is_fifo_or_socket(st: &libc::stat) -> bool {
    matches!(st.st_mode & libc::S_IFMT, libc::S_IFIFO | libc::S_IFSOCK)
}

/// fd_exposed: fd N is one a box process could open again through /proc/PID/fd of a command given it
/// (finding 325): a file, a directory, a terminal, any device but /dev/null. Not a pipe or a socket,
/// nor /dev/null, nor an fd that is not open.
pub fn fd_exposed(fd: RawFd) -> bool {
    let Some(st) = fstat(fd) else { return false };
    if is_fifo_or_socket(&st) {
        return false;
    }
    if st.st_mode & libc::S_IFMT == libc::S_IFCHR {
        let null = std::fs::metadata("/dev/null");
        return !null.is_ok_and(|m| m.dev() == st.st_dev && m.ino() == st.st_ino);
    }
    true
}

pub fn isatty(fd: RawFd) -> bool {
    // SAFETY: isatty only looks at the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

/// `[ /dev/fd/A -ef /dev/fd/B ]`.
fn same_file(a: RawFd, b: RawFd) -> bool {
    match (fstat(a), fstat(b)) {
        (Some(x), Some(y)) => x.st_dev == y.st_dev && x.st_ino == y.st_ino,
        _ => false,
    }
}

/// `[ -x PATH ]`.
pub fn executable(p: &[u8]) -> bool {
    std::ffi::CString::new(p.to_vec()).is_ok_and(|c| {
        // SAFETY: c is a valid C string for the call.
        unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
    })
}

/// The fds open in this process now, past stdio: what fds_only closes in a child.
fn open_fds() -> Vec<RawFd> {
    std::fs::read_dir("/proc/self/fd")
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().to_str()?.parse::<RawFd>().ok())
                .filter(|&n| n > 2)
                .collect()
        })
        .unwrap_or_default()
}

/// The hard core limit is 0: no cores to skip (nocore).
fn core_hard_zero() -> bool {
    // SAFETY: rlimit is plain data getrlimit fills in.
    let mut r: libc::rlimit = unsafe { std::mem::zeroed() };
    // SAFETY: r is ours.
    unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut r) == 0 && r.rlim_max == 0 }
}

/// `F >/dev/null` or `F 2>/dev/null` for a stretch of this process: fd FD is /dev/null until the guard
/// drops, as bash's redirection of a function's call is for every word said in it and every child it
/// starts. Only where no other thread writes to FD meanwhile.
pub struct Redir {
    fd: RawFd,
    saved: Option<OwnedFd>,
}

impl Redir {
    pub fn null(fd: RawFd) -> Redir {
        match dev_null(true) {
            Ok(n) => Redir::to(fd, n.as_raw_fd()),
            Err(_) => Redir { fd, saved: None },
        }
    }

    /// `F 2>"$file"`: fd FD the same as TO (a file of the caller's) until the guard drops.
    pub fn to(fd: RawFd, to: RawFd) -> Redir {
        let _ = std::io::stdout().flush();
        let saved = dup_hi(fd).ok();
        // SAFETY: both fds are open; dup2 only changes this process's descriptor table.
        unsafe {
            libc::dup2(to, fd);
        }
        Redir { fd, saved }
    }
}

impl Drop for Redir {
    fn drop(&mut self) {
        let _ = std::io::stdout().flush();
        if let Some(s) = &self.saved {
            // SAFETY: as in null: the fd saved there back in its place.
            unsafe {
                libc::dup2(s.as_raw_fd(), self.fd);
            }
        }
    }
}

// --- children -------------------------------------------------------------------------------------

/// How a child is started past its stdio: the fds it keeps by number (KEEP), one given it as fd 3
/// (run -d's), fds_only (every other fd closed), nocore (a core limit of 1 byte, finding 67:
/// `prlimit --core=1:`, done here before the exec).
#[derive(Default)]
pub struct Fds {
    pub keep: Vec<RawFd>,
    pub fd3: Option<RawFd>,
    pub only: bool,
    pub nocore: bool,
    /// Where held_run says the child's pid once started, and 0 once it is reaped (`$!`, for a
    /// caller that may have to kill it: settle_end's kill_tree).
    pub pid: Option<Arc<AtomicI32>>,
    /// Started as bash starts what it backgrounds (`&`, coproc, a process substitution): SIGINT and
    /// SIGQUIT ignored, so a Ctrl-C or Ctrl-\ at the terminal leaves it running; a tool holding a
    /// key or a button lets go once its stdin closes (finding 112).
    pub bg: bool,
}

/// What bash's `&` does to a child's signals (no job control): SIGINT and SIGQUIT ignored.
pub fn background(c: &mut Command) {
    // SAFETY: signal is async-signal-safe; between fork and exec it changes only the child.
    unsafe {
        c.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            libc::signal(libc::SIGQUIT, libc::SIG_IGN);
            Ok(())
        });
    }
}

pub fn command(argv: &[Vec<u8>], fds: Fds) -> Command {
    let mut c = crate::sig::command(OsStr::from_bytes(argv.first().map_or(&b""[..], |a| a)));
    c.args(argv.iter().skip(1).map(|a| OsStr::from_bytes(a)));
    let open = if fds.only { open_fds() } else { Vec::new() };
    let nocore = fds.nocore && !core_hard_zero();
    // SAFETY: the closure only makes async-signal-safe calls (dup2, close, fcntl, signal,
    // getrlimit, setrlimit) on memory it owns, between fork and exec.
    unsafe {
        c.pre_exec(move || {
            if let Some(src) = fds.fd3 {
                if libc::dup2(src, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            for &fd in &open {
                if !fds.keep.contains(&fd) && !(fds.fd3.is_some() && fd == 3) {
                    libc::close(fd);
                }
            }
            for &fd in &fds.keep {
                libc::fcntl(fd, libc::F_SETFD, 0);
            }
            if fds.bg {
                libc::signal(libc::SIGINT, libc::SIG_IGN);
                libc::signal(libc::SIGQUIT, libc::SIG_IGN);
            }
            if nocore {
                let mut r: libc::rlimit = std::mem::zeroed();
                if libc::getrlimit(libc::RLIMIT_CORE, &mut r) == 0 {
                    r.rlim_cur = 1;
                    libc::setrlimit(libc::RLIMIT_CORE, &r);
                }
            }
            Ok(())
        });
    }
    c
}

/// A child's end as bash's `$?`: its status, or 128+N for signal N; and the signal.
pub fn status(s: std::process::ExitStatus) -> (i32, Option<i32>) {
    match (s.code(), s.signal()) {
        (Some(c), _) => (c, None),
        (None, Some(sig)) => (128 + sig, Some(sig)),
        _ => (1, None),
    }
}

fn wait(child: &mut Child) -> (i32, Option<i32>) {
    match child.wait() {
        Ok(s) => status(s),
        Err(_) => (1, None),
    }
}

/// A pipe written from a thread of its own (a process substitution's `<(printf ...)`): the bytes,
/// then its end. A reader that goes first gets no SIGPIPE here: blocked in that thread, a write
/// fails instead.
pub fn feed(w: OwnedFd, data: Vec<u8>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // SAFETY: a signal set of our own, for this thread's mask only.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGPIPE);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
        let mut f = std::fs::File::from(w);
        let _ = f.write_all(&data);
    })
}

/// What a reader of R gets, up to LIMIT bytes (None: all) and its EOF; past the limit R is closed,
/// as `| head -c N` does (a writer then gets SIGPIPE).
pub fn read_all(r: OwnedFd, limit: Option<usize>) -> Vec<u8> {
    let mut f = std::fs::File::from(r);
    let mut out = Vec::new();
    match limit {
        Some(n) => {
            let _ = (&mut f).take(n as u64).read_to_end(&mut out);
        }
        None => {
            let _ = f.read_to_end(&mut out);
        }
    }
    out
}

// --- ns_held / ns_said -------------------------------------------------------------------------------

/// nsenter's own words, kept apart from the command's (#166): its stderr goes to a pipe read here
/// by a thread, the first NS_HELD_MAX bytes kept, the rest read into nothing, until its writers are
/// gone, or NS_HELD_WAIT after the command ended (then whatever came).
struct Held {
    buf: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
}

fn hold(r: OwnedFd) -> Held {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, done) = mpsc::channel();
    let (b, s) = (buf.clone(), stop.clone());
    std::thread::spawn(move || {
        let mut tmp = [0u8; 8192];
        let mut pfd = libc::pollfd {
            fd: r.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        while !s.load(Ordering::Relaxed) {
            // SAFETY: one pollfd of ours.
            let n = unsafe { libc::poll(&mut pfd, 1, 50) };
            if n <= 0 {
                continue;
            }
            // SAFETY: tmp is ours, its length given.
            let got = unsafe { libc::read(r.as_raw_fd(), tmp.as_mut_ptr().cast(), tmp.len()) };
            if got == 0
                || (got < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted)
            {
                break;
            }
            let got = usize::try_from(got).unwrap_or(0);
            if let Ok(mut v) = b.lock() {
                let room = NS_HELD_MAX.saturating_sub(v.len());
                v.extend_from_slice(&tmp[..got.min(room)]);
            }
        }
        let _ = tx.send(());
    });
    Held { buf, done, stop }
}

impl Held {
    /// NS_HELD: what came, up to its first NUL (`read -d ''`).
    fn take(self) -> Vec<u8> {
        let _ = self.done.recv_timeout(NS_HELD_WAIT);
        self.stop.store(true, Ordering::Relaxed);
        let v = self.buf.lock().map(|v| v.clone()).unwrap_or_default();
        match v.iter().position(|&c| c == 0) {
            Some(i) => v[..i].to_vec(),
            None => v,
        }
    }
}

/// Where a helper's own words go: the command's stderr as the caller set it (`2>&1` into a capture,
/// `2>/dev/null`, or this process's).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ErrTo {
    Inherit,
    Null,
    Out,
    /// An fd of the caller's (`2>&FD`: settle_begin's tool, its words to the pipe read with its
    /// answer).
    Fd(RawFd),
}

/// The words ns_said says, given what nsenter's stderr held, the command's end RC, and a line bash
/// itself put there for a command killed by a signal (KILLED: one of ns_own_line's, said in its
/// words under KILL_SAY: run's own command, #303). Its own lines whole (at most 3, printable ASCII,
/// 400 at most), then how many bytes of anything else were dropped.
pub fn said_text(held: &[u8], rc: i32, killed_line: bool, kill_say: bool) -> Vec<u8> {
    let mut lines: Vec<&[u8]> = Vec::new();
    if let Some(last) = held.iter().rposition(|&c| c == b'\n') {
        lines = held[..last].split(|&c| c == b'\n').collect();
    }
    let (mut out, mut n, mut kept, mut killed) = (Vec::new(), 0, 0usize, false);
    for l in lines {
        let l: Vec<u8> = l
            .iter()
            .copied()
            .filter(|c| (0x20..0x7f).contains(c))
            .collect();
        if n < 3 && l.len() <= 400 && own_line(&l) {
            out.extend_from_slice(&l);
            out.push(b'\n');
            n += 1;
            kept += l.len() + 1;
        }
    }
    // Bash's line for nsenter killed by a signal comes last ("SCRIPT: line N: PID Killed CMD").
    if killed_line && n < 3 && kill_say && rc > 128 {
        killed = true;
    }
    if killed {
        out.extend_from_slice(&cat!(
            "omabox: the command ",
            job_how(rc.to_string().as_bytes()),
            "\n"
        ));
    }
    let dropped = held.len().saturating_sub(kept);
    if dropped > 0 {
        out.extend_from_slice(
            format!("omabox: dropped {dropped} bytes of other output on nsenter's stderr\n")
                .as_bytes(),
        );
    }
    out
}

/// ns_own_line: nsenter's own, or prlimit's and env's on the way in.
fn own_line(l: &[u8]) -> bool {
    ["nsenter: ", "prlimit: ", "env: "]
        .iter()
        .any(|p| l.starts_with(p.as_bytes()) && l.len() > p.len())
}

/// The line bash prints for a command of a group or subshell killed by signal SIG, as ns_held holds
/// it: none for SIGINT and SIGPIPE; SIGTERM's is "Terminated" alone, padded, then CMD (not one of
/// ns_own_line's: dropped and counted); the rest start with the script's name and line, which
/// ns_kill_line takes as bash's own (KillLine::Own).
pub enum KillLine {
    None,
    Bytes(Vec<u8>),
    Own,
}

pub fn kill_line(sig: Option<i32>, cmd: &str) -> KillLine {
    match sig {
        None | Some(libc::SIGINT) | Some(libc::SIGPIPE) => KillLine::None,
        Some(libc::SIGTERM) => KillLine::Bytes(format!("{:<27}{cmd}\n", "Terminated").into_bytes()),
        Some(_) => KillLine::Own,
    }
}

/// What a sink gets: this process's stderr, nothing, or the capture (BUF).
fn say_to(err: ErrTo, buf: &mut Vec<u8>, m: &[u8]) {
    match err {
        ErrTo::Inherit => {
            let _ = std::io::stderr().write_all(m);
        }
        ErrTo::Null => {}
        ErrTo::Out => buf.extend_from_slice(m),
        ErrTo::Fd(fd) => {
            // SAFETY: the caller's fd, open while the call runs; m is valid for its length.
            let _ = unsafe { libc::write(fd, m.as_ptr().cast(), m.len()) };
        }
    }
}

/// ns_said RC [WORDS]: on a box that went down meanwhile (its PID 1 can outlive the command by up to
/// 2 s), WORDS instead (BOX_GONE_SAID), or nothing; else said_text. A box this namespace cannot read
/// (box_alive's refusal) ends it, said where the rest would go.
#[allow(clippy::too_many_arguments)]
fn ns_said(
    ctx: &Ctx,
    held: &[u8],
    killed: &KillLine,
    rc: i32,
    words: Option<&[u8]>,
    kill_say: bool,
    err: ErrTo,
    buf: &mut Vec<u8>,
) -> Result<bool, u8> {
    let mut held = held.to_vec();
    if let KillLine::Bytes(b) = killed {
        let room = NS_HELD_MAX.saturating_sub(held.len());
        held.extend_from_slice(&b[..b.len().min(room)]);
    }
    let own = matches!(killed, KillLine::Own) && held.len() < NS_HELD_MAX;
    if held.is_empty() && !own {
        return Ok(false);
    }
    if rc != 0 {
        for _ in 0..10 {
            match alive_quiet(ctx) {
                Ok(true) => {}
                Ok(false) => {
                    if let Some(w) = words {
                        say_to(err, buf, &cat!("omabox: ", w, "\n"));
                        return Ok(true);
                    }
                    return Ok(false);
                }
                Err((m, code)) => {
                    say_to(err, buf, &m);
                    return Err(code);
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    say_to(err, buf, &said_text(&held, rc, own, kill_say));
    Ok(false)
}

/// box_alive, its refusal's message returned rather than said (it goes where the caller's stderr is).
fn alive_quiet(ctx: &Ctx) -> Result<bool, (Vec<u8>, u8)> {
    match box_pid(&ctx.d) {
        BoxPid::Unknown => {
            let m = cat!(
                "omabox: cannot tell whether box '",
                ctx.name,
                "' is up from this user namespace (a sandbox's?): run omabox outside it\n"
            );
            Err((m, ctx.die_rc.unwrap_or(1)))
        }
        BoxPid::Pid(p) => Ok(procfs::running(&p)),
        BoxPid::None => Ok(false),
    }
}

// --- on_box ---------------------------------------------------------------------------------------

/// Where a command's stdout goes.
pub enum Out {
    Inherit,
    Null,
    /// To this fd (a file of the caller's, a pipe it reads).
    Fd(OwnedFd),
    /// Read here, as `$(...)`, up to so many bytes (`| head -c N`).
    Capture(Option<usize>),
}

/// How a call through on_box ended: its status (DIE_RC's for a refusal, said where its stderr goes),
/// what was captured, and whether ns_said said the box went down.
pub struct Ran {
    pub rc: i32,
    pub out: Vec<u8>,
    pub refused: bool,
    pub gone_said: bool,
}

impl Ran {
    /// Outside `$(...)`, a refusal ends the command (on_box's die).
    pub fn or_exit(self) -> Res<Ran> {
        if self.refused {
            return Err(Exit(u8::try_from(self.rc).unwrap_or(1)));
        }
        Ok(self)
    }
}

/// on_box ARGS: a tool against the box's compositor (hyprctl, cat, tail, stat), inside the box's
/// user and mount namespaces with a clean environment, where every path resolves as the box sees it
/// (finding 63). Not its pid namespace (finding 307). stdin as the caller's (or /dev/null with
/// STDIN_NULL); stdout and stderr where OUT and ERR say.
///
/// REPORT: the call is not inside `$(...)`, where bash says (into nsenter's held stderr) a command
/// killed by a signal; inside one it does not, and ns_said then has nothing to wait on.
pub fn on_box(
    ctx: &Ctx,
    args: &[Vec<u8>],
    stdin_null: bool,
    out: Out,
    err: ErrTo,
    report: bool,
) -> Ran {
    let stdin = if stdin_null { In::Null } else { In::Inherit };
    on_box_io(ctx, args, stdin, out, err, report, None)
}

/// on_box with its stdin any fd (settle_begin's pipe), and its nsenter's pid said in PID (`$!` of a
/// background on_box).
pub fn on_box_io(
    ctx: &Ctx,
    args: &[Vec<u8>],
    stdin: In,
    out: Out,
    err: ErrTo,
    report: bool,
    pid_to: Option<Arc<AtomicI32>>,
) -> Ran {
    on_box_with(ctx, args, stdin, out, err, report, pid_to, false)
}

/// on_box_io for what bash starts in the background (`&`, coproc, `<(...)`): SIGINT and SIGQUIT
/// ignored in it (Fds::bg).
pub fn on_box_bg(
    ctx: &Ctx,
    args: &[Vec<u8>],
    stdin: In,
    out: Out,
    err: ErrTo,
    report: bool,
    pid_to: Option<Arc<AtomicI32>>,
) -> Ran {
    on_box_with(ctx, args, stdin, out, err, report, pid_to, true)
}

#[allow(clippy::too_many_arguments)]
fn on_box_with(
    ctx: &Ctx,
    args: &[Vec<u8>],
    stdin: In,
    out: Out,
    err: ErrTo,
    report: bool,
    pid_to: Option<Arc<AtomicI32>>,
    bg: bool,
) -> Ran {
    let mut buf = Vec::new();
    let refuse = |buf: &mut Vec<u8>, m: Vec<u8>| {
        say_to(err, buf, &cat!("omabox: ", m, "\n"));
        Ran {
            rc: i32::from(ctx.die_rc.unwrap_or(1)),
            out: std::mem::take(buf),
            refused: true,
            gone_said: false,
        }
    };
    let pid = match box_pid(&ctx.d) {
        BoxPid::Pid(p) => p,
        // (box_pid's 3 leaves it empty: callers run it in an || list, where set -e does not hold.)
        BoxPid::None | BoxPid::Unknown => {
            return refuse(&mut buf, cat!("box '", ctx.name, "' is not up"));
        }
    };
    if covers_held(ctx).is_err() {
        // (covers_held's die, its words to wherever this call's stderr is)
        return refuse(
            &mut buf,
            cat!(
                "box '",
                ctx.name,
                "' is still starting: what its jail hides is not checked yet (omabox down ",
                ctx.name,
                " ends it)"
            ),
        );
    }
    let Some(wd) = box_env(&ctx.d, "WAYLAND_DISPLAY") else {
        return refuse(
            &mut buf,
            cat!("box '", ctx.name, "': no WAYLAND_DISPLAY in its omabox.env"),
        );
    };
    let Some(sig) = box_env(&ctx.d, "HYPRLAND_INSTANCE_SIGNATURE") else {
        return refuse(
            &mut buf,
            cat!(
                "box '",
                ctx.name,
                "': no HYPRLAND_INSTANCE_SIGNATURE in its omabox.env"
            ),
        );
    };
    let lang = shell::env("LANG")
        .filter(|l| !l.is_empty())
        .unwrap_or(b"C.UTF-8".to_vec());
    let mut argv: Vec<Vec<u8>> = vec![
        b"nsenter".to_vec(),
        b"-t".to_vec(),
        pid,
        b"-U".to_vec(),
        b"-m".to_vec(),
        b"--preserve-credentials".to_vec(),
        b"--".to_vec(),
        b"env".to_vec(),
        b"-i".to_vec(),
        b"PATH=/usr/bin".to_vec(),
        format!("XDG_RUNTIME_DIR=/run/user/{}", procfs::uid()).into_bytes(),
        cat!("WAYLAND_DISPLAY=", wd),
        cat!("HYPRLAND_INSTANCE_SIGNATURE=", sig),
        cat!("LANG=", lang),
        EFD_WORD.to_vec(),
        b"/usr/bin/sh".to_vec(),
        b"-c".to_vec(),
        ON_BOX_SH.to_vec(),
        b"sh".to_vec(),
    ];
    argv.extend(args.iter().cloned());
    let fds = Fds {
        pid: pid_to,
        bg,
        ..Fds::default()
    };
    let r = match held_run(&argv, stdin, out, err, fds) {
        Ok(r) => r,
        Err(e) => {
            say_to(
                err,
                &mut buf,
                format!("omabox: could not run nsenter: {e}\n").as_bytes(),
            );
            return Ran {
                rc: 126,
                out: buf,
                refused: false,
                gone_said: false,
            };
        }
    };
    let killed = if report {
        kill_line(r.sig, ON_BOX_KILL_CMD)
    } else {
        KillLine::None
    };
    buf = r.out;
    let words = cat!(
        "box '",
        ctx.name,
        "' went down (why: omabox log -b ",
        ctx.name,
        " all; clear it with: omabox down ",
        ctx.name,
        ")"
    );
    match ns_said(
        ctx,
        &r.held,
        &killed,
        r.rc,
        Some(&words),
        false,
        err,
        &mut buf,
    ) {
        Ok(gone_said) => Ran {
            rc: r.rc,
            out: buf,
            refused: false,
            gone_said,
        },
        Err(code) => Ran {
            rc: i32::from(code),
            out: buf,
            refused: true,
            gone_said: false,
        },
    }
}

/// The inner sh of on_box: the caller's stderr (OMABOX_EFD) made the command's first thing.
const ON_BOX_SH: &[u8] = b"case $OMABOX_EFD in *[!0-9]*) ;; *) eval \"exec 2>&$OMABOX_EFD $OMABOX_EFD>&-\" ;; esac\n      unset OMABOX_EFD; exec \"$@\"";
/// What bash's line names for on_box's nsenter killed by a signal (ns_held's group).
const ON_BOX_KILL_CMD: &str = "\"${args[@]}\" {w}>&- {dr}<&-";
/// The word ns_held puts the caller's stderr's number in (the first one only).
const EFD_WORD: &[u8] = b"OMABOX_EFD=@EFD";

/// A command's stdin.
pub enum In {
    Inherit,
    Null,
    Fd(OwnedFd),
}

/// What held_run gives back: the status, the signal that ended it, what was captured (and said
/// to ErrTo::Out), what nsenter's stderr held.
struct HeldRun {
    rc: i32,
    sig: Option<i32>,
    out: Vec<u8>,
    held: Vec<u8>,
}

/// ns_held ARGV: ARGV with its stderr held (hold), the caller's stderr (ERR) given it as the fd its
/// first OMABOX_EFD=@EFD word names.
fn held_run(
    argv: &[Vec<u8>],
    stdin: In,
    out: Out,
    err: ErrTo,
    mut fds: Fds,
) -> std::io::Result<HeldRun> {
    let (hr, hw) = pipe()?;
    let (cap, limit, stdout) = match out {
        Out::Inherit => (None, None, Stdio::inherit()),
        Out::Null => (None, None, Stdio::null()),
        Out::Fd(f) => (None, None, Stdio::from(f)),
        Out::Capture(n) => {
            let (r, w) = pipe()?;
            let efd_src = dup_hi(w.as_raw_fd())?;
            (Some((r, efd_src)), n, Stdio::from(w))
        }
    };
    let efd = match (err, &cap) {
        (ErrTo::Out, Some((_, w))) => dup_hi(w.as_raw_fd())?,
        (ErrTo::Null, _) => dup_hi(dev_null(true)?.as_raw_fd())?,
        (ErrTo::Fd(fd), _) => dup_hi(fd)?,
        _ => dup_hi(2)?,
    };
    let mut argv = argv.to_vec();
    if let Some(w) = argv.iter_mut().find(|w| w.as_slice() == EFD_WORD) {
        *w = format!("OMABOX_EFD={}", efd.as_raw_fd()).into_bytes();
    }
    fds.keep.push(efd.as_raw_fd());
    let pid_to = fds.pid.take();
    let mut c = command(&argv, fds);
    c.stdin(match stdin {
        In::Inherit => Stdio::inherit(),
        In::Null => Stdio::null(),
        In::Fd(f) => Stdio::from(f),
    });
    c.stdout(stdout);
    c.stderr(Stdio::from(hw));
    let spawned = c.spawn();
    drop(c);
    drop(efd);
    let mut child = spawned?;
    crate::sig::spawned(child.id());
    if let Some(p) = &pid_to {
        p.store(i32::try_from(child.id()).unwrap_or(0), Ordering::SeqCst);
    }
    let (held, out, (rc, sig)) = crate::sig::foreground(|| {
        let held = hold(hr);
        let out = match cap {
            Some((r, w)) => {
                drop(w);
                read_all(r, limit)
            }
            None => Vec::new(),
        };
        (held, out, wait(&mut child))
    });
    if let Some(p) = &pid_to {
        p.store(0, Ordering::SeqCst);
    }
    Ok(HeldRun {
        rc,
        sig,
        out,
        held: held.take(),
    })
}

// --- box_exec ---------------------------------------------------------------------------------------

/// The inner bash of box_exec: the caller's stderr (OMABOX_EFD) first, --pass's values from their fd,
/// the caller's dir, then the PATH the session has (findings 47, 57, 135, 358). Byte for byte bash's.
const BOX_EXEC_SH: &str = r#"
      case $OMABOX_EFD in *[!0-9]*) ;; *) eval "exec 2>&$OMABOX_EFD $OMABOX_EFD>&-" ;; esac; unset OMABOX_EFD
      if [ -n "${OMABOX_PASS_FD:-}" ]; then
        while IFS= read -r -d "" kv <&"$OMABOX_PASS_FD"; do export "$kv"; done
        eval "exec $OMABOX_PASS_FD<&-"; unset OMABOX_PASS_FD
      fi
      cd "$1" && shift
      if [ -n "${OMABOX_SYS:-}" ]; then unset OMABOX_SYS OMABOX_CALLER_PATH; PATH=/usr/bin exec -- "$@"; fi
      p=/opt/omabox/share/bin:/home/sbx/.local/bin
      # `up --quickshell`: the build as quickshell and qs, before the caller's /usr/bin (finding 358).
      [ ! -x "${XDG_RUNTIME_DIR:-/nonexistent}/omabox-quickshell/quickshell" ] || p=$p:$XDG_RUNTIME_DIR/omabox-quickshell
      # `up --omarchy`: its bin before the caller's dirs, as in the session (finding 135).
      [ "${OMARCHY_PATH:-/usr/share/omarchy}" = /usr/share/omarchy ] || p=$p:${OMARCHY_PATH%/}/bin
      set -f; IFS=:
      for d in $OMABOX_CALLER_PATH $PATH; do
        case $d in /*) ;; *) continue ;; esac
        case ":$p:" in *":$d:"*) ;; *) [ -d "$d" ] && p=$p:$d ;; esac
      done
      unset IFS OMABOX_CALLER_PATH; set +f; PATH=$p exec -- "$@""#;

/// What bash's line names for box_exec's nsenter killed by a signal (nocore's prlimit, in fds_only's
/// subshell).
fn box_exec_kill_cmd() -> &'static str {
    if core_hard_zero() {
        "\"$@\""
    } else {
        "prlimit --core=1: \"$@\""
    }
}

/// box_exec's options: its stdin /dev/null, its stdout the caller's stderr (run -d's
/// `</dev/null >&2`), an fd given it as 3 (BOX_KEEP=3, `3>&1`), and whether a command killed by a signal is said
/// in words (NS_KILL_SAY: run's own command, #303).
#[derive(Default)]
pub struct ExecOpts {
    pub stdin_null: bool,
    pub stdout_to_stderr: bool,
    pub fd3: Option<OwnedFd>,
    pub kill_say: bool,
    /// Its stdout an fd of the caller's (`$(box_sys ...)`: a pipe read there) rather than this
    /// process's.
    pub stdout: Option<RawFd>,
    /// box_sys (BOX_SYS): one of omabox's own helpers, by its full path, with only the box's
    /// display, Hyprland and session bus of omabox.env, in /home/sbx, PATH=/usr/bin, its pipes not
    /// relayed and no terminal of its own.
    pub sys: bool,
}

/// box_sys's environment: HOME, LANG, the runtime dir, then the first WAYLAND_DISPLAY,
/// HYPRLAND_INSTANCE_SIGNATURE and DBUS_SESSION_BUS_ADDRESS of omabox.env, each only when tame.
fn sys_env(d: &[u8]) -> Vec<Vec<u8>> {
    let lang = shell::env("LANG")
        .filter(|l| !l.is_empty())
        .unwrap_or(b"C.UTF-8".to_vec());
    let mut env = vec![
        b"HOME=/home/sbx".to_vec(),
        cat!("LANG=", lang),
        format!("XDG_RUNTIME_DIR=/run/user/{}", procfs::uid()).into_bytes(),
        b"OMABOX_SYS=1".to_vec(),
    ];
    let mut seen: Vec<Vec<u8>> = Vec::new();
    // (`while IFS= read -r -d '' v`: a last record without its NUL is not read)
    let text = run_read(d, "omabox.env").unwrap_or_default();
    let mut recs: Vec<&[u8]> = text.split(|&c| c == 0).collect();
    recs.pop();
    for v in recs {
        let i = v.iter().position(|&c| c == b'=').unwrap_or(v.len());
        let (k, val) = (&v[..i], v.get(i + 1..).unwrap_or_default());
        let tame = match k {
            b"WAYLAND_DISPLAY" | b"HYPRLAND_INSTANCE_SIGNATURE" => {
                shell::re_match("^[A-Za-z0-9_.-]+$", val)
            }
            b"DBUS_SESSION_BUS_ADDRESS" => shell::re_match("^unix:[A-Za-z0-9_.=/,-]{1,200}$", val),
            _ => continue,
        };
        if tame && !seen.iter().any(|s| s == k) {
            seen.push(k.to_vec());
            env.push(v.to_vec());
        }
    }
    env
}

/// box_exec ARGS: a command inside the box's namespaces (unprivileged: we own its user namespace)
/// with the session's environment, in box_cwd's dir, with box_exec's PATH. Its stdin, stdout and
/// stderr are never a file, a terminal or a device of the host's (finding 325), and it gets no other
/// fd but those it is given (#224).
pub fn box_exec(ctx: &mut Ctx, args: &[Vec<u8>], o: ExecOpts) -> Res<i32> {
    let pid = match box_pid(&ctx.d) {
        BoxPid::Pid(p) => p,
        BoxPid::None | BoxPid::Unknown => {
            return Err(ctx.die(cat!("box '", ctx.name, "' is not up")));
        }
    };
    covers_held(ctx)?;
    let mut wd = shell::pwd();
    let env: Vec<Vec<u8>> = if o.sys {
        if !args.first().is_some_and(|a| a.starts_with(b"/")) {
            let a = args.first().cloned().unwrap_or(b"nothing".to_vec());
            return Err(ctx.die(cat!(
                "box_exec: omabox's own helper goes by its full path, got ",
                a
            )));
        }
        wd.clear();
        sys_env(&ctx.d)
    } else {
        // Not the session's own marker: a command run here is not the box's session (finding 424).
        mapfile0(&run_read(&ctx.d, "omabox.env").unwrap_or_default())
            .into_iter()
            .filter(|v| !v.starts_with(b"OMABOX_OWN_SESSION="))
            .collect()
    };
    if jail::in_jail() && !o.sys {
        // A jailed caller's, when its jail and the host agree on it.
        wd = jail::get_raw("cwd");
    }
    let cwd = if wd.is_empty() {
        b"/home/sbx".to_vec()
    } else {
        box_cwd(&ctx.d, &pid, &wd)
    };
    // The box's own network namespace (every box has one since finding 89); one started before that
    // shares the host's, which is not ours to join.
    let mut ns: Vec<&[u8]> = vec![b"-U", b"-m", b"-p", b"-u", b"-i"];
    let net = |p: &[u8]| std::fs::read_link(shell::path(&cat!("/proc/", p, "/ns/net"))).ok();
    if net(&pid) != net(b"self") {
        ns.push(b"-n");
    }
    // run --pass (finding 66): the caller's values through a pipe, never on a command line;
    // --env-file's entries first, so a --pass of the same name wins.
    let mut keep: Vec<RawFd> = Vec::new();
    let mut pass_word = Vec::new();
    let mut pfd: Option<OwnedFd> = None;
    if !ctx.pass.is_empty() || !ctx.envkv.is_empty() {
        let mut data = Vec::new();
        for kv in &ctx.envkv {
            data.extend_from_slice(kv);
            data.push(0);
        }
        for v in &ctx.pass {
            data.extend_from_slice(&cat!(v, "=", caller_env(v).unwrap_or_default()));
            data.push(0);
        }
        let (r, w) = pipe().map_err(|_| Exit(1))?;
        let r = dup_hi(r.as_raw_fd()).map_err(|_| Exit(1))?;
        feed(w, data);
        keep.push(r.as_raw_fd());
        pass_word = format!("OMABOX_PASS_FD={}", r.as_raw_fd()).into_bytes();
        pfd = Some(r);
    }
    let mut nsline: Vec<Vec<u8>> = vec![b"nsenter".to_vec(), b"-t".to_vec(), pid];
    nsline.extend(ns.iter().map(|s| s.to_vec()));
    nsline.extend([
        b"--preserve-credentials".to_vec(),
        b"--".to_vec(),
        b"/usr/bin/env".to_vec(),
        b"-i".to_vec(),
        EFD_WORD.to_vec(),
    ]);
    nsline.extend(env);
    nsline.extend([
        b"OMABOX=1".to_vec(),
        cat!("OMABOX_NAME=", ctx.name),
        cat!(
            "OMABOX_CALLER_PATH=",
            if o.sys { Vec::new() } else { caller_path(true) }
        ),
    ]);
    if !pass_word.is_empty() {
        nsline.push(pass_word);
    }
    nsline.extend([
        b"/usr/bin/bash".to_vec(),
        b"-c".to_vec(),
        BOX_EXEC_SH.as_bytes().to_vec(),
        b"bash".to_vec(),
        cwd,
    ]);
    nsline.extend(args.iter().cloned());
    // The caller's stdio as this command gets them.
    let null_in = if o.stdin_null {
        dev_null(false).ok()
    } else {
        None
    };
    let src = [
        null_in.as_ref().map_or(0, AsRawFd::as_raw_fd),
        match (o.stdout, o.stdout_to_stderr) {
            (Some(fd), _) => fd,
            (None, true) => 2,
            (None, false) => 1,
        },
        2,
    ];
    if o.fd3.is_some() {
        keep.push(3);
    }
    let fd3 = o.fd3.as_ref().map(AsRawFd::as_raw_fd);
    let rc;
    let mut held = Vec::new();
    let mut killed = KillLine::None;
    if !o.sys && isatty(src[0]) && isatty(src[1]) && fg_tty() && executable(b"/usr/bin/script") {
        rc = box_pty(&nsline, keep, fd3).map_err(|_| Exit(1))?;
    } else {
        let relays = Relays::new(src, !o.sys).map_err(|_| Exit(1))?;
        let fds = Fds {
            keep,
            fd3,
            only: true,
            nocore: true,
            pid: None,
            bg: false,
        };
        let r = relays.run(&nsline, fds);
        match r {
            Ok(r) => {
                rc = r.rc;
                held = r.held;
                killed = kill_line(r.sig, box_exec_kill_cmd());
            }
            Err(_) => return Err(Exit(1)),
        }
    }
    drop(pfd);
    drop(o.fd3);
    let mut sink = Vec::new();
    match ns_said(
        ctx,
        &held,
        &killed,
        rc,
        None,
        o.kill_say,
        ErrTo::Inherit,
        &mut sink,
    ) {
        Ok(_) => Ok(rc),
        Err(code) => Err(Exit(code)),
    }
}

/// `mapfile -d ''`: each NUL-ended record, and a last one without its NUL.
fn mapfile0(v: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = v.split(|&c| c == 0).map(<[u8]>::to_vec).collect();
    if out.last().is_some_and(Vec::is_empty) {
        out.pop();
    }
    out
}

/// The caller's environment as it came to bin/omabox, from OMABOX_ENVIRON_FD (its /proc/PID/environ,
/// read before bash's globals overwrote the names they share with it); the fd closed after.
static CALLER_ENVIRON: std::sync::OnceLock<Option<Vec<u8>>> = std::sync::OnceLock::new();

pub fn caller_environ_take() {
    let fd = shell::env_or_empty("OMABOX_ENVIRON_FD");
    let fd = std::str::from_utf8(&fd)
        .ok()
        .and_then(|f| f.parse::<RawFd>().ok());
    let got = fd.filter(|&f| f > 2).map(|f| {
        // SAFETY: bash opened it for this process alone and names it here; read once, then closed.
        let file = unsafe { std::fs::File::from_raw_fd(f) };
        read_all(file.into(), None)
    });
    let _ = CALLER_ENVIRON.set(got);
}

/// The caller's environment as it came, when bash handed it over.
pub fn caller_environ() -> Option<&'static [u8]> {
    CALLER_ENVIRON.get().and_then(|e| e.as_deref())
}

/// A variable of the caller's as it was when omabox started (run --pass): from what came, not from
/// this process's environment (finding 74). A jailed caller's comes as OMABOX_RELAY_PASS_NAME
/// (relay_call). None when it is not there.
pub fn caller_env(name: &[u8]) -> Option<Vec<u8>> {
    let k = if jail::in_jail() {
        cat!("OMABOX_RELAY_PASS_", name)
    } else {
        name.to_vec()
    };
    if let Some(Some(env)) = CALLER_ENVIRON.get() {
        let lead = cat!(k, "=");
        return env
            .split(|&c| c == 0)
            .find_map(|e| e.strip_prefix(lead.as_slice()))
            .map(<[u8]>::to_vec);
    }
    std::env::var_os(OsStr::from_bytes(&k)).map(|v| v.as_bytes().to_vec())
}

/// caller_path [box]: the caller's PATH without the guard's xdg-open (finding 92); for a box (BOX),
/// nor an Omarchy checkout's bin (finding 149). A jailed caller's PATH is OMABOX_RELAY_PATH.
pub fn caller_path(for_box: bool) -> Vec<u8> {
    let p = if jail::in_jail() {
        shell::env_or_empty("OMABOX_RELAY_PATH")
    } else {
        shell::env_or_empty("PATH")
    };
    // (`IFS=: read -ra` of `<<<"$p"`: the first line, split at each :, a last empty field dropped.)
    let line = p.split(|&c| c == b'\n').next().unwrap_or_default();
    let mut dirs: Vec<&[u8]> = if line.is_empty() {
        Vec::new()
    } else {
        line.split(|&c| c == b':').collect()
    };
    if dirs.last().is_some_and(|d| d.is_empty()) {
        dirs.pop();
    }
    let mut out: Vec<&[u8]> = Vec::new();
    for d in dirs {
        if d.ends_with(b"/share/guard") || d.ends_with(b"/share/guard/") {
            continue;
        }
        let t = d.strip_suffix(b"/").unwrap_or(d);
        if for_box
            && t != b"/usr/share/omarchy/bin"
            && shell::path(&cat!(t, "/../default/hypr/envs.lua")).is_file()
        {
            continue;
        }
        out.push(d);
    }
    out.join(&b':')
}

/// box_cwd PID WD: WD when it is the same directory in the box (a mount of it: same device and
/// inode, or an overlay of it), else the box HOME (finding 63: /home/USER and /tmp exist in a box
/// too, empty or the box's own).
pub fn box_cwd(d: &[u8], pid: &[u8], wd: &[u8]) -> Vec<u8> {
    let here = std::fs::metadata(shell::path(wd))
        .map(|m| format!("{}:{}", m.dev(), m.ino()).into_bytes())
        .unwrap_or_default();
    let there = crate::sig::command("nsenter")
        .args([
            OsStr::new("-t"),
            OsStr::from_bytes(pid),
            OsStr::new("-U"),
            OsStr::new("-m"),
        ])
        .args(["--preserve-credentials", "stat", "-c", "%d:%i", "--"])
        .arg(OsStr::from_bytes(wd))
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| strip_nl(&o.stdout))
        .unwrap_or_default();
    if !here.is_empty() && here == there {
        return wd.to_vec();
    }
    if !there.is_empty() && under_overlay(d, wd) {
        return wd.to_vec();
    }
    b"/home/sbx".to_vec()
}

/// `jq -e --arg p WD/ 'any(.overlays[]?; . as $o | $p | startswith($o + "/"))' box.json`.
fn under_overlay(d: &[u8], wd: &[u8]) -> bool {
    let Ok(text) = std::fs::read(boxes::at(d, "box.json")) else {
        return false;
    };
    let (vals, _) = json::values(&text);
    let p = cat!(wd, "/");
    let mut last = false;
    for v in &vals {
        let Some(serde_json::Value::Array(a)) = v.get("overlays") else {
            last = false;
            continue;
        };
        last = false;
        for o in a {
            let serde_json::Value::String(o) = o else {
                return false;
            };
            if p.starts_with(&cat!(o, "/")) {
                last = true;
                break;
            }
        }
    }
    last
}

/// Whether this process is in its terminal's foreground process group: one in the background must
/// neither read the terminal (SIGTTIN) nor put it in raw mode (SIGTTOU).
fn fg_tty() -> bool {
    let Some(s) = procfs::read(b"self", "stat") else {
        return false;
    };
    let f = shell::words(procfs::after_comm(&s));
    f.len() > 5 && f[2] == f[5]
}

/// box_pty: the command on a pseudo-terminal of its own, for a caller at a terminal (finding 325):
/// util-linux `script` holds the caller's terminal and the pty's master; the box gets the slave. A
/// stderr that is not the terminal stays where it was (relayed if a box could reach it), given to
/// the inner bash as OMABOX_EFD.
fn box_pty(nsline: &[Vec<u8>], mut keep: Vec<RawFd>, fd3: Option<RawFd>) -> std::io::Result<i32> {
    let mut efd: Option<OwnedFd> = None;
    let mut relay: Option<Child> = None;
    if !isatty(2) {
        if relay_out(2, true) {
            let (r, w) = pipe()?;
            relay = Some(relay_copy_cmd(r, dup_hi(2)?).spawn()?);
            efd = Some(dup_hi(w.as_raw_fd())?);
        } else {
            efd = Some(dup_hi(2)?);
        }
    }
    if let Some(e) = &efd {
        keep.push(e.as_raw_fd());
    }
    let efd_word = efd.as_ref().map_or("OMABOX_EFD=-".to_string(), |e| {
        format!("OMABOX_EFD={}", e.as_raw_fd())
    });
    let mut words: Vec<Vec<u8>> = Vec::new();
    let mut set = false;
    for a in nsline {
        if !set && a.as_slice() == EFD_WORD {
            words.push(shell::quote_at(efd_word.as_bytes()));
            set = true;
        } else {
            words.push(shell::quote_at(a));
        }
    }
    let script = cat!("exec ", words.join(&b' '));
    let argv = vec![
        b"env".to_vec(),
        b"SHELL=/usr/bin/bash".to_vec(),
        b"/usr/bin/script".to_vec(),
        b"-q".to_vec(),
        b"-e".to_vec(),
        b"-c".to_vec(),
        script,
        b"/dev/null".to_vec(),
    ];
    let mut c = command(
        &argv,
        Fds {
            keep,
            fd3,
            only: true,
            nocore: true,
            pid: None,
            bg: false,
        },
    );
    let mut child = c.spawn()?;
    drop(c);
    let (rc, _) = wait(&mut child);
    drop(efd);
    if let Some(r) = relay {
        drain(vec![r]);
    }
    Ok(rc)
}

// --- the relays ---------------------------------------------------------------------------------------

/// relay_out N: fd N goes through a relay: a box could reach it (fd_exposed), or it is a pipe or a
/// socket a process the command leaves running would hold (finding 337; with omabox-relay only).
fn relay_out(fd: RawFd, pipes: bool) -> bool {
    if fd_exposed(fd) {
        return true;
    }
    pipes && executable(&relay_bin()) && fstat(fd).is_some_and(|st| is_fifo_or_socket(&st))
}

fn relay_bin() -> Vec<u8> {
    cat!(shell::bytes(&crate::root()), "/tools/relay/omabox-relay")
}

/// stdio_relay: each of the command's stdin, stdout and stderr a box could reach goes through a pipe
/// held here. A terminal on stdin is not read: /dev/null. One stdout and stderr that are the same file
/// share one relay, so their lines land in order.
struct Relays {
    fds: [OwnedFd; 3],
    input: Option<Child>,
    outs: Vec<Child>,
}

impl Relays {
    fn new(src: [RawFd; 3], pipes: bool) -> std::io::Result<Relays> {
        let mut input = None;
        let stdin = if isatty(src[0]) {
            dev_null(false)?
        } else if fd_exposed(src[0]) {
            let (r, w) = pipe()?;
            // (`<(cat)`: a process substitution, SIGINT and SIGQUIT ignored in it)
            let mut c = crate::sig::command("cat");
            c.stdin(Stdio::from(dup_hi(src[0])?)).stdout(Stdio::from(w));
            background(&mut c);
            input = Some(c.spawn()?);
            r
        } else {
            dup_hi(src[0])?
        };
        let mut outs = Vec::new();
        let mut out_w: Option<OwnedFd> = None;
        let stdout = if relay_out(src[1], pipes) {
            let (r, w) = pipe()?;
            let mut c = relay_copy_cmd(r, dup_hi(src[1])?);
            c.stderr(Stdio::from(dup_hi(src[2])?));
            outs.push(c.spawn()?);
            out_w = Some(dup_hi(w.as_raw_fd())?);
            w
        } else {
            dup_hi(src[1])?
        };
        let stderr = if relay_out(src[2], pipes) {
            match &out_w {
                Some(w) if same_file(src[1], src[2]) => dup_hi(w.as_raw_fd())?,
                _ => {
                    let (r, w) = pipe()?;
                    let mut c = relay_copy_cmd(r, dup_hi(src[2])?);
                    c.stderr(Stdio::from(dup_hi(src[2])?));
                    outs.push(c.spawn()?);
                    w
                }
            }
        } else {
            dup_hi(src[2])?
        };
        drop(out_w);
        Ok(Relays {
            fds: [stdin, stdout, stderr],
            input,
            outs,
        })
    }

    /// The command through ns_held with these as its stdio; then this side's ends closed, the input
    /// relay ended, the output drained (relay_drain).
    fn run(self, argv: &[Vec<u8>], fds: Fds) -> std::io::Result<HeldRun> {
        let [i, o, e] = self.fds;
        // (ns_held's EFD is the command's stderr: `{e}>&2` with stdio_relay's 2>&efd)
        let r = held_run_err(argv, In::Fd(i), Out::Fd(o), e, fds);
        if let Some(mut c) = self.input {
            let _ = c.kill();
            let _ = c.wait();
        }
        drain(self.outs);
        r
    }
}

/// relay_copy: R copied to OUT by omabox-relay copy (which relay_drain can end), or `cat` where this
/// omabox has none built.
fn relay_copy_cmd(r: OwnedFd, out: OwnedFd) -> Command {
    let bin = relay_bin();
    let mut c = if executable(&bin) {
        let mut c = crate::sig::command(OsStr::from_bytes(&bin));
        c.arg("copy");
        c
    } else {
        crate::sig::command("cat")
    };
    c.stdin(Stdio::from(r)).stdout(Stdio::from(out));
    // (`>(relay_copy)`: a process substitution, SIGINT and SIGQUIT ignored in it)
    background(&mut c);
    c
}

/// held_run with the command's stderr (EFD) a given fd.
fn held_run_err(
    argv: &[Vec<u8>],
    stdin: In,
    out: Out,
    err: OwnedFd,
    mut fds: Fds,
) -> std::io::Result<HeldRun> {
    let (hr, hw) = pipe()?;
    let efd = dup_hi(err.as_raw_fd())?;
    drop(err);
    let mut argv = argv.to_vec();
    if let Some(w) = argv.iter_mut().find(|w| w.as_slice() == EFD_WORD) {
        *w = format!("OMABOX_EFD={}", efd.as_raw_fd()).into_bytes();
    }
    fds.keep.push(efd.as_raw_fd());
    let mut c = command(&argv, fds);
    c.stdin(match stdin {
        In::Inherit => Stdio::inherit(),
        In::Null => Stdio::null(),
        In::Fd(f) => Stdio::from(f),
    });
    c.stdout(match out {
        Out::Fd(f) => Stdio::from(f),
        Out::Null => Stdio::null(),
        _ => Stdio::inherit(),
    });
    c.stderr(Stdio::from(hw));
    let spawned = c.spawn();
    drop(c);
    drop(efd);
    let mut child = spawned?;
    let (held, (rc, sig)) = crate::sig::foreground(|| {
        let held = hold(hr);
        (held, wait(&mut child))
    });
    Ok(HeldRun {
        rc,
        sig,
        out: Vec::new(),
        held: held.take(),
    })
}

/// relay_drain: each output relay, once the command is over, given a second to end. One still running
/// after 100 ms has a process the command left running holding its pipe (finding 337, #239): told the
/// command has ended (SIGURG, every 50 ms), it copies what the pipe holds and ends. One still copying
/// after the second, or a `cat`, is left running.
fn drain(outs: Vec<Child>) {
    for mut c in outs {
        for i in 1..=100 {
            if matches!(c.try_wait(), Ok(Some(_)) | Err(_)) {
                break;
            }
            if i >= 10 && i % 5 == 0 {
                // SAFETY: a child of ours, not yet reaped: its pid is still its own.
                unsafe {
                    libc::kill(c.id() as libc::pid_t, libc::SIGURG);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // As t_unit_box_gone checks bash's ns_said.
    #[test]
    fn said_as_bash() {
        let held =
            b"omabox: note: forged\n\x1b[31mnsenter: red\x1b[0m\nnsenter: real \x1b]0;t\x07\nhalf";
        let mut h = held.to_vec();
        h.resize(NS_HELD_MAX, b'A');
        assert_eq!(
            String::from_utf8_lossy(&said_text(&h, 1, false, false)),
            "nsenter: real ]0;t\nomabox: dropped 4077 bytes of other output on nsenter's stderr\n"
        );
        assert_eq!(
            String::from_utf8_lossy(&said_text(b"nsenter: x\n", 137, true, true)),
            "nsenter: x\nomabox: the command was killed by SIGKILL (137)\n"
        );
        assert_eq!(said_text(b"", 137, true, false), b"");
        assert_eq!(
            said_text(b"nsenter: a\nnsenter: b\nnsenter: c\n", 137, true, true),
            b"nsenter: a\nnsenter: b\nnsenter: c\n"
        );
        assert_eq!(
            said_text(b"env: x", 1, false, false),
            b"omabox: dropped 6 bytes of other output on nsenter's stderr\n"
        );
    }

    #[test]
    fn kill_lines() {
        assert!(matches!(kill_line(None, "x"), KillLine::None));
        assert!(matches!(kill_line(Some(libc::SIGINT), "x"), KillLine::None));
        assert!(matches!(
            kill_line(Some(libc::SIGPIPE), "x"),
            KillLine::None
        ));
        assert!(matches!(kill_line(Some(libc::SIGKILL), "x"), KillLine::Own));
        // bash 5.3's, 50 bytes for prlimit's line (what run -- sh -c 'kill -TERM $$' says it dropped)
        let KillLine::Bytes(b) = kill_line(Some(libc::SIGTERM), "prlimit --core=1: \"$@\"") else {
            panic!("no line")
        };
        assert_eq!(b.len(), 50);
    }

    #[test]
    fn env_records() {
        assert_eq!(
            mapfile0(b"A=1\0B=2\0"),
            vec![b"A=1".to_vec(), b"B=2".to_vec()]
        );
        assert_eq!(
            mapfile0(b"A=1\0\0B"),
            vec![b"A=1".to_vec(), Vec::new(), b"B".to_vec()]
        );
        assert!(mapfile0(b"").is_empty());
    }
}
