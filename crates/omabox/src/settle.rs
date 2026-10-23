//! The screen's watch (finding 82): omabox-still in the box, for `wait still|change` and an action's
//! --wait (keys, click, scroll, drag, run -d). Its options where a command parses its own
//! (settle_opt, settle_opts_check, settle_in_window, mask_arg), the tool started and its `ready` read
//! (settle_begin), its answer in words (settle_end, settle_sent), and, for an action, what the
//! desktop did meanwhile (#274: desk_snap before the input, desk_facts and desk_target after). As
//! bin/omabox's SETTLE_* globals and functions, one struct here.

use crate::boxes::{box_alive, meta, touch};
use crate::cat;
use crate::desk::{Target, desk_facts, desk_snap, desk_target};
use crate::exec::{ErrTo, In, Out, on_box, pipe};
use crate::exit::{Ctx, Exit, Res, out, say};
use crate::hypr::{hung_text, hypr_answers, hypr_json};
use crate::jobs::job_failed;
use crate::jq::{self, Args};
use crate::num::{ms_duration, ms_s, xywh_less, xywh_words};
use crate::procfs;
use crate::shell::{self, bash_int};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const BOX_STILL: &str = "/opt/omabox/bin/omabox-still";
/// --timeout and the like: 10 min at most.
pub const WAIT_MAX_MS: i64 = 600_000;

/// `date +%s%3N`.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(0))
        .unwrap_or(0)
}

/// bash's SECONDS, as its deadlines read it: whole seconds of the wall clock (`end=$((SECONDS + N))`
/// then `[ $SECONDS -lt $end ]` is `time() < start + N`), so a deadline here ends where bash's does,
/// 0 to 1 s short of N.
pub fn secs() -> i64 {
    // SAFETY: time with a null pointer only returns the time.
    unsafe { libc::time(std::ptr::null_mut()) }
}

/// geom_parse: a -g value as its X Y W H, from grim's "X,Y WxH" or X,Y,W,H and X,Y W,H (#27).
pub fn geom_parse(v: &[u8]) -> Option<[Vec<u8>; 4]> {
    let re = shell::Re::new(r"^(-?[0-9]+),(-?[0-9]+)[\ ,]([1-9][0-9]*)[x,]([1-9][0-9]*)$")?;
    let m = re.captures(v, false)?;
    let g = |i: usize| {
        m.get(i)
            .copied()
            .flatten()
            .map(|(a, b)| v[a..b].to_vec())
            .unwrap_or_default()
    };
    Some([g(1), g(2), g(3), g(4)])
}

/// `${g// /,}` of geom_parse's answer: omabox-still's X,Y,W,H.
pub fn geom_csv(g: &[Vec<u8>; 4]) -> Vec<u8> {
    g.join(&b',')
}

/// `IFS=, read -r A B C D <<<"$V"`: four fields, the last taking the rest.
fn csv4(v: &[u8]) -> [Vec<u8>; 4] {
    let line = v.split(|&c| c == b'\n').next().unwrap_or_default();
    let mut it = line.splitn(4, |&c| c == b',');
    let mut f = || it.next().unwrap_or_default().to_vec();
    [f(), f(), f(), f()]
}

/// rects_meet A B: whether two X,Y,W,H rectangles overlap; None where bash's arithmetic refuses a
/// number (an X of 08: its own error line, the check skipped).
pub fn rects_meet(a: &[u8], b: &[u8]) -> Option<bool> {
    let n = |v: &[Vec<u8>; 4]| -> Option<[i64; 4]> {
        Some([
            bash_int(&v[0])?,
            bash_int(&v[1])?,
            bash_int(&v[2])?,
            bash_int(&v[3])?,
        ])
    };
    let [ax, ay, aw, ah] = n(&csv4(a))?;
    let [bx, by, bw, bh] = n(&csv4(b))?;
    Some(
        ax < bx.wrapping_add(bw)
            && bx < ax.wrapping_add(aw)
            && ay < by.wrapping_add(bh)
            && by < ay.wrapping_add(ah),
    )
}

/// cursor_rect "X Y": where the software cursor is drawn (findings 82, 305), as omabox-still's
/// rectangle around the point.
pub fn cursor_rect(p: &[u8]) -> Option<Vec<u8>> {
    let sp = p.iter().rposition(|&c| c == b' ').unwrap_or(0);
    let fp = p.iter().position(|&c| c == b' ').map_or(p.len(), |i| i + 1);
    let x = bash_int(&p[..sp])?;
    let y = bash_int(&p[fp.min(p.len())..])?;
    Some(format!("{},{},64,64", x.wrapping_sub(16), y.wrapping_sub(16)).into_bytes())
}

/// cursor_now: where the pointer is, "X Y" (nothing when it is not two numbers); a box not answering
/// ends the command (hypr_json), as does jq failing (`p=$(cursor_now) || exit $?`).
pub fn cursor_now(ctx: &Ctx) -> Res<Vec<u8>> {
    let j = shell::strip_nl(&hypr_json(ctx, &["cursorpos"])?);
    let r = jq::jq(
        &["-r", crate::jqprog::CURSOR_JQ],
        Some(&j),
        jq::Err::Inherit,
    );
    if !r.ok() {
        return Err(Exit(u8::try_from(r.rc).unwrap_or(1)));
    }
    Ok(r.text())
}

/// ws_text WS: a workspace in words.
pub fn ws_text(ws: &[u8]) -> Vec<u8> {
    if ws == b"special:scratchpad" {
        return b"the scratchpad (SUPER+S shows it)".to_vec();
    }
    if let Some(s) = ws.strip_prefix(b"special:") {
        return cat!(
            "special workspace ",
            s,
            ", which the bar widget's Show opens and a workspace switch hides"
        );
    }
    cat!("workspace ", ws)
}

/// kill_tree PID: PID and everything under it, the children first.
pub fn kill_tree(pid: &[u8]) {
    for c in procfs::children(pid) {
        kill_tree(&c);
    }
    if let Some(p) = std::str::from_utf8(pid)
        .ok()
        .and_then(|p| p.parse::<libc::pid_t>().ok())
        .filter(|&p| p > 0)
    {
        // SAFETY: a signal to a pid; nothing of ours is touched.
        unsafe {
            libc::kill(p, libc::SIGKILL);
        }
    }
}

/// One line of the tool's, as `IFS= read -r -t 1` gives it: a whole line, what came of one before
/// the second was over, or what came of one before the end.
pub enum Got {
    Line(Vec<u8>),
    Timeout(Vec<u8>),
    Eof(Vec<u8>),
}

/// The tool's stdout and stderr (SETTLE_OUT), read a line at a time (and with_mods' keyboard tool's,
/// drag --shot's pointer tool's).
pub struct Lines {
    fd: OwnedFd,
    buf: Vec<u8>,
    eof: bool,
}

impl Lines {
    pub fn new(fd: OwnedFd) -> Lines {
        Lines {
            fd,
            buf: Vec::new(),
            eof: false,
        }
    }

    pub fn read(&mut self, t: Duration) -> Got {
        let end = Instant::now() + t;
        loop {
            if let Some(i) = self.buf.iter().position(|&c| c == b'\n') {
                let mut l: Vec<u8> = self.buf.drain(..=i).collect();
                l.pop();
                return Got::Line(l);
            }
            if self.eof {
                return Got::Eof(std::mem::take(&mut self.buf));
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Got::Timeout(std::mem::take(&mut self.buf));
            }
            let mut pfd = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = libc::c_int::try_from(left.as_millis().max(1)).unwrap_or(1000);
            // SAFETY: one pollfd of ours.
            if unsafe { libc::poll(&mut pfd, 1, ms) } <= 0 {
                continue;
            }
            let mut tmp = [0u8; 4096];
            // SAFETY: tmp is ours, its length given.
            let n = unsafe { libc::read(self.fd.as_raw_fd(), tmp.as_mut_ptr().cast(), tmp.len()) };
            match n {
                0 => self.eof = true,
                n if n < 0 => {
                    if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                        self.eof = true;
                    }
                }
                // (read drops NUL bytes)
                n => self.buf.extend(
                    tmp[..usize::try_from(n).unwrap_or(0)]
                        .iter()
                        .filter(|&&c| c != 0),
                ),
            }
        }
    }
}

/// The tool running: its stdin held here (SETTLE_IN: its end, --tied), its output read here
/// (SETTLE_OUT), the background on_box that runs it, and that one's nsenter's pid (SETTLE_PID's
/// tree).
struct Tool {
    input: Option<OwnedFd>,
    lines: Lines,
    thread: JoinHandle<()>,
    pid: Arc<AtomicI32>,
}

/// SETTLE_*: what one command's watch holds.
#[derive(Default)]
pub struct Settle {
    /// SETTLE_FACTS: the action (keys, click, scroll, drag, run), empty for `wait`.
    pub facts: &'static str,
    /// SETTLE_FXY: the point ("X Y") of click, scroll, drag's start; SETTLE_FPID: run -d's job.
    pub fxy: Vec<u8>,
    pub fpid: Vec<u8>,
    /// SETTLE_ARGS, SETTLE_JSON, SETTLE_WAIT, SETTLE_TOOK, SETTLE_REGION, SETTLE_WINDOW,
    /// SETTLE_ORIGIN ("X Y").
    pub args: Vec<Vec<u8>>,
    pub json: bool,
    pub wait: bool,
    pub took: usize,
    pub region: bool,
    pub window: bool,
    pub origin: Option<(Vec<u8>, Vec<u8>)>,
    /// MASKS: --ignore regions so far.
    pub masks: usize,
    /// SETTLE_LOOKED: a 124 said where the input went and that nothing moved.
    pub looked: bool,
    snap: Option<JoinHandle<Vec<u8>>>,
    before: Vec<u8>,
    line: Vec<u8>,
    err: Vec<u8>,
    t0: i64,
    tool: Option<Tool>,
}

impl Settle {
    /// mask_arg GEOM: an --ignore region (#131) as omabox-still's X,Y,W,H; at most 16.
    pub fn mask_arg(&mut self, ctx: &Ctx, g: &[u8]) -> Res<Vec<u8>> {
        let Some(p) = geom_parse(g) else {
            return Err(ctx.die(cat!("--ignore is \"X,Y WxH\" (or X,Y,W,H), got ", g)));
        };
        self.masks += 1;
        if self.masks > 16 {
            return Err(ctx.die("--ignore: at most 16 regions"));
        }
        Ok(geom_csv(&p))
    }

    /// settle_opt OPT [VALUE]: --wait's options, taken where the command parses its own; `took`, how
    /// many words (0: not one of these).
    pub fn opt(&mut self, ctx: &Ctx, args: &[Vec<u8>]) -> Res<()> {
        self.took = 1;
        let o = args.first().map(Vec::as_slice).unwrap_or_default();
        let v = args.get(1);
        match o {
            b"--ignore" => {
                let Some(v) = v else {
                    return Err(ctx.die("--ignore takes a region: \"X,Y WxH\""));
                };
                let m = self.mask_arg(ctx, v)?;
                self.args.extend([b"--mask".to_vec(), m]);
                self.took = 2;
            }
            b"-g" => {
                let Some(g) = v.and_then(|v| geom_parse(v)) else {
                    return Err(ctx.die(
                        "-g is \"X,Y WxH\" (or X,Y,W,H): the part of the screen --wait watches",
                    ));
                };
                if self.region {
                    return Err(ctx.die("-g once: one region is watched"));
                }
                self.args.extend([b"--region".to_vec(), geom_csv(&g)]);
                self.region = true;
                self.took = 2;
            }
            b"--wait" => self.wait = true,
            b"--json" => self.json = true,
            b"--start" | b"--quiet" | b"--timeout" => {
                let Some(ms) = v.and_then(|v| ms_duration(v)) else {
                    return Err(ctx.die(cat!(o, " takes a duration: 300ms, 2s, 1m")));
                };
                if ms > WAIT_MAX_MS {
                    return Err(ctx.die(cat!(o, " is at most 10m")));
                }
                self.args.extend([o.to_vec(), ms.to_string().into_bytes()]);
                self.took = 2;
            }
            b"--strict" => self.args.push(b"--strict".to_vec()),
            _ => self.took = 0,
        }
        Ok(())
    }

    /// settle_opts_check [WINDOWS]: the options go with --wait; with one --window selector, -g and
    /// --ignore are the window's coordinates; an --ignore that misses -g's region is said (#162).
    pub fn opts_check(&mut self, ctx: &Ctx, windows: usize) -> Res<()> {
        if !self.wait && !(self.args.is_empty() && !self.json) {
            return Err(ctx.die(
                "--start, --quiet, --timeout, --strict, --ignore, -g and --json go with --wait",
            ));
        }
        if windows != 0 {
            self.window = true;
        }
        let n = self.args.len();
        let mut region: Vec<u8> = Vec::new();
        for i in 0..n.saturating_sub(1) {
            if self.args[i] == b"--region" {
                region = self.args[i + 1].clone();
            }
        }
        if region.is_empty() {
            return Ok(());
        }
        for i in 0..n.saturating_sub(1) {
            if self.args[i] != b"--mask" {
                continue;
            }
            match rects_meet(&self.args[i + 1], &region) {
                Some(true) => continue,
                Some(false) => {}
                // (bash's arithmetic refused a number: its own words, then the command ends, 1)
                None => return Err(Exit(1)),
            }
            say(cat!(
                "--ignore \"",
                xywh_words(&self.args[i + 1]),
                "\" is outside -g's region: it leaves nothing out there"
            ));
        }
        Ok(())
    }

    /// settle_in_window X Y (F8, #287): the caller's regions, given in the window's coordinates,
    /// moved by the window's place. None where bash's arithmetic refuses a number of theirs.
    pub fn in_window(&mut self, x: &[u8], y: &[u8]) -> Option<()> {
        self.origin = Some((x.to_vec(), y.to_vec()));
        let (ox, oy) = (bash_int(x)?, bash_int(y)?);
        for i in 0..self.args.len().saturating_sub(1) {
            if !matches!(self.args[i].as_slice(), b"--region" | b"--mask") {
                continue;
            }
            let [a, b, w, h] = csv4(&self.args[i + 1]);
            let (a, b) = (bash_int(&a)?, bash_int(&b)?);
            self.args[i + 1] = cat!(
                a.wrapping_add(ox).to_string(),
                ",",
                b.wrapping_add(oy).to_string(),
                ",",
                w,
                ",",
                h
            );
        }
        Some(())
    }

    /// The before snapshot (an action's), read in the background while the tool starts.
    fn snap_start(&mut self, ctx: &Ctx) -> Res<()> {
        if self.facts.is_empty() {
            return Ok(());
        }
        // (bash's snapshot went to a `mktemp` file, removed once read: one that cannot be made
        // ends the command, mktemp's words said; made and removed here all the same)
        let (_, f) = shell::mktemp()?;
        let _ = std::fs::remove_file(shell::path(&f));
        self.before.clear();
        let c = ctx.clone();
        self.snap = Some(std::thread::spawn(move || desk_snap(&c, true)));
        Ok(())
    }

    /// desk_snap_take: the before snapshot, once its read is over.
    fn snap_take(&mut self) {
        if let Some(t) = self.snap.take() {
            self.before = t.join().unwrap_or_default();
        }
    }

    /// settle_begin MODE TOOL_ARGS...: omabox-still in the box, waiting on its first frame (its
    /// `ready`, or its answer), 12 s at most: a hung compositor is the tool's own "unknown hung" at
    /// 10 s (finding 181).
    pub fn begin(&mut self, ctx: &Ctx, tool_args: &[Vec<u8>]) -> Res<()> {
        // OMABOX_STILL_DUMP=DIR (a path in the box; the suite's, for a flaky check: #222), refused
        // before anything is made (finding 426).
        let mut argv = vec![BOX_STILL.as_bytes().to_vec()];
        argv.extend(tool_args.iter().cloned());
        if let Some(d) = shell::env("OMABOX_STILL_DUMP").filter(|d| !d.is_empty()) {
            if !shell::re_match("^/[A-Za-z0-9._/-]+$", &d) {
                return Err(ctx.die(cat!(
                    "OMABOX_STILL_DUMP: a plain absolute path in the box, got ",
                    d
                )));
            }
            argv.extend([b"--dump".to_vec(), d]);
        }
        // (Before any input is sent: a box started by an omabox without the tool cannot watch.)
        let t = on_box(
            ctx,
            &[
                b"test".to_vec(),
                b"-x".to_vec(),
                BOX_STILL.as_bytes().to_vec(),
            ],
            false,
            Out::Inherit,
            ErrTo::Inherit,
        )
        .or_exit()?;
        if t.rc != 0 {
            return Err(ctx.die(cat!(
                "box '",
                ctx.name,
                "' has no ",
                BOX_STILL,
                " (started by an older omabox): omabox down ",
                ctx.name,
                ", then up again"
            )));
        }
        self.snap_start(ctx)?;
        // (the tool's fifos were in a `mktemp -d` dir: the same rule)
        let (_, dir) = shell::mktemp_args(&[b"-d"])?;
        let _ = std::fs::remove_dir(shell::path(&dir));
        argv.push(b"--tied".to_vec());
        let ((in_r, in_w), (out_r, out_w)) = match (pipe(), pipe()) {
            (Ok(a), Ok(b)) => (a, b),
            _ => return Err(ctx.die("could not make the pipes for the screen's watch")),
        };
        let pid = Arc::new(AtomicI32::new(0));
        let (c, p) = (ctx.clone(), pid.clone());
        let thread = std::thread::spawn(move || {
            block_sigpipe();
            let err: RawFd = out_w.as_raw_fd();
            let o = out_w.try_clone().ok();
            if let Some(o) = o {
                // (bash's `&`: SIGINT and SIGQUIT ignored in it)
                let _ = crate::exec::on_box_bg(
                    &c,
                    &argv,
                    In::Fd(in_r),
                    Out::Fd(o),
                    ErrTo::Fd(err),
                    Some(p),
                );
            }
            drop(out_w);
        });
        self.tool = Some(Tool {
            input: Some(in_w),
            lines: Lines {
                fd: out_r,
                buf: Vec::new(),
                eof: false,
            },
            thread,
            pid,
        });
        self.err.clear();
        let end = secs() + 12;
        let (mut line, mut part) = (Vec::new(), Vec::new());
        let answer = shell::Re::new(r"^(satisfied|unsatisfied|unknown)\ ");
        let is_answer = |l: &[u8]| answer.as_ref().is_some_and(|r| r.is_match(l));
        if let Some(tool) = self.tool.as_mut() {
            loop {
                // (IFS=: a line split at a space keeps it, #310.)
                match tool.lines.read(Duration::from_secs(1)) {
                    Got::Line(l) => {
                        line = cat!(part, l);
                        part.clear();
                        if line.starts_with(b"ready") || is_answer(&line) {
                            break;
                        }
                        let sep: &[u8] = if self.err.is_empty() { b"" } else { b"; " };
                        self.err = cat!(self.err, sep, line);
                    }
                    // A read that timed out keeps what it had of a line: the rest comes with the
                    // next one (finding 217).
                    Got::Timeout(p) => {
                        part.extend(p);
                        line.clear();
                        if secs() >= end {
                            break;
                        }
                    }
                    Got::Eof(p) => {
                        part.extend(p);
                        line.clear();
                        break;
                    }
                }
            }
        }
        self.line = line;
        self.t0 = now_ms();
        Ok(())
    }

    /// settle_ready JSON: before keys, click or run -d act; no `ready` means nothing is sent, and
    /// the answer is unknown (1), never the action done unwatched.
    pub fn ready(&mut self, ctx: &Ctx, json: bool) -> Res<()> {
        self.snap_take();
        if self.line.starts_with(b"ready") {
            return Ok(());
        }
        let _ = self.end(ctx, "settle", json, b"")?;
        say("nothing was sent: the screen could not be watched");
        Err(Exit(1))
    }

    /// The tool's ends closed, then the tool given 2 s to be gone, killed with what runs it after
    /// (#124).
    fn finish_tool(&mut self) {
        let Some(mut t) = self.tool.take() else {
            return;
        };
        t.input.take();
        drop(t.lines);
        let mut gone = false;
        for _ in 0..40 {
            if t.thread.is_finished() {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !gone {
            let p = t.pid.load(Ordering::SeqCst);
            if p > 0 {
                kill_tree(p.to_string().as_bytes());
            }
            // (Killed with what runs it, its on_box says nothing more: not waited for.)
            return;
        }
        let _ = t.thread.join();
    }

    /// settle_end COND JSON SENT: the tool's answer as a line (or JSON) on stdout; 0, 124 or 1. COND
    /// names it in messages; SENT is what an action already did, said when the answer is not yes.
    pub fn end(&mut self, ctx: &Ctx, cond: &str, json: bool, sent: &[u8]) -> Res<i32> {
        let mut line = self.line.clone();
        let scr = line.strip_prefix(b"ready ").unwrap_or(&line).to_vec();
        let answer = shell::Re::new(r"^(satisfied|unsatisfied|unknown)\ ");
        let is_answer = |l: &[u8]| answer.as_ref().is_some_and(|r| r.is_match(l));
        if line.starts_with(b"ready") {
            line.clear();
            let mut part = Vec::new();
            let mut used = secs();
            if let Some(tool) = self.tool.as_mut() {
                loop {
                    match tool.lines.read(Duration::from_secs(1)) {
                        Got::Line(l) => {
                            line = cat!(part, l);
                            part.clear();
                            if is_answer(&line) {
                                break;
                            }
                            let sep: &[u8] = if self.err.is_empty() { b"" } else { b"; " };
                            self.err = cat!(self.err, sep, line);
                            line.clear();
                            continue;
                        }
                        Got::Timeout(p) => {
                            part.extend(p);
                            line.clear();
                        }
                        // EOF: the tool is gone without an answer.
                        Got::Eof(_) => {
                            line.clear();
                            break;
                        }
                    }
                    // Waiting is using the box (idle expiry, finding 59).
                    if secs() - used >= 30 {
                        touch(&ctx.at("used"));
                        used = secs();
                    }
                }
            }
        }
        // Every wait in the tool has a deadline (finding 181), but this one does not depend on that
        // (#124): one still running 2 s after it answered is killed, with what runs it.
        self.finish_tool();
        let w = shell::read_fields(&line, 3);
        let (mut res, mut reason) = (w[0].clone(), w[1].clone());
        let mut f: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for kv in shell::words(&w[2]) {
            let i = kv.iter().position(|&c| c == b'=');
            let (k, v) = match i {
                Some(i) => (kv[..i].to_vec(), kv[i + 1..].to_vec()),
                None => (kv.clone(), kv.clone()),
            };
            if let Some(e) = f.iter_mut().find(|e| e.0 == k) {
                e.1 = v;
            } else {
                f.push((k, v));
            }
        }
        let get = |k: &str| -> Vec<u8> {
            f.iter()
                .find(|e| e.0 == k.as_bytes())
                .map(|e| e.1.clone())
                .unwrap_or_default()
        };
        // `${f[K]:--}`
        let dash = |k: &str| -> Vec<u8> {
            let v = get(k);
            if v.is_empty() { b"-".to_vec() } else { v }
        };
        if res.is_empty() {
            res = b"unknown".to_vec();
            reason = b"lost".to_vec();
        }
        let n = &ctx.name;
        let err_part = if self.err.is_empty() {
            Vec::new()
        } else {
            cat!(" (", self.err, ")")
        };
        if res == b"unknown" {
            reason = match reason.as_slice() {
                b"not-rendered" => {
                    // A hidden interactive box is drawn (finding 90) unless it is older than that
                    // or confirm-close replaced its window; a headless box always draws: no frame
                    // is a Hyprland that stopped answering (#124).
                    if meta(&ctx.d, "mode").0 != b"interactive" {
                        if hypr_answers(ctx)? {
                            cat!("box '", n, "' sent no frame")
                        } else {
                            hung_text(ctx, "")?
                        }
                    } else if meta(&ctx.d, "drawn_hidden").0 != b"true" {
                        cat!(
                            "box '",
                            n,
                            "' not rendered: an older omabox started it, and it is not drawn while hidden (it needs a restart: ask the user; never switch their workspace or focus)"
                        )
                    } else if ctx.at("run/omabox.reopened").is_file() {
                        cat!(
                            "box '",
                            n,
                            "' not rendered: its window was replaced (confirm-close) and is not drawn while hidden (ask the user; never switch their workspace or focus)"
                        )
                    } else {
                        cat!(
                            "box '",
                            n,
                            "' sent no frame (",
                            ws_text(&meta(&ctx.d, "workspace").0),
                            "; ask the user; never switch their workspace or focus)"
                        )
                    }
                }
                b"hung" => hung_text(ctx, "10 s")?,
                b"refused" => cat!("box '", n, "' refused a screen capture", err_part),
                b"off-screen" => cat!("the region watched is not on the screen of box '", n, "'"),
                _ => {
                    // Its compositor gone: the box is (being) taken down.
                    let gone = now_ms() - self.t0;
                    let end = secs() + 2;
                    while box_alive(ctx)? && secs() < end {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    if !box_alive(ctx)? {
                        cat!("box '", n, "' went down after ", ms_s(gone.to_string()))
                    } else if !hypr_answers(ctx)? {
                        hung_text(ctx, "")?
                    } else {
                        cat!("lost the box's screen", err_part)
                    }
                }
            };
        }
        let geo = if dash("change") == b"-" {
            Vec::new()
        } else {
            xywh_words(&get("change"))
        };
        let ign = if dash("ignored") == b"-" {
            Vec::new()
        } else {
            let why = crate::num::awk_v(&get("why"));
            let w: &[u8] = match why.as_slice() {
                b"caret" => b"caret?",
                b"mask" => b"--ignore",
                _ => &why,
            };
            // (awk's fields: each one alone)
            let ig = get("ignored");
            let line = ig.split(|&c| c == b'\n').next().unwrap_or_default();
            let fl: Vec<&[u8]> = line.split(|&c| c == b',').collect();
            let g = |i: usize| fl.get(i).copied().unwrap_or_default();
            cat!(g(2), "x", g(3), " at ", g(0), ",", g(1), ": ", w)
        };
        let ign_semi = if ign.is_empty() {
            Vec::new()
        } else {
            cat!("; ignored ", ign)
        };
        let mut msg: Vec<u8> = match (res.as_slice(), reason.as_slice()) {
            (b"satisfied", b"changed") => cat!("changed after ", ms_s(get("last")), " at ", geo),
            (b"satisfied", b"still" | b"settled") => {
                let word: &[u8] = if reason == b"still" {
                    b"still"
                } else {
                    b"settled"
                };
                let mut m = cat!(word, " after ", ms_s(get("t")));
                if geo.is_empty() {
                    m = cat!(m, " (no change", ign_semi, ")");
                } else {
                    m = cat!(
                        m,
                        " (last change ",
                        ms_s(get("last")),
                        " at ",
                        geo,
                        ign_semi,
                        ")"
                    );
                }
                m
            }
            (b"unsatisfied", b"nothing") => {
                let i = if ign.is_empty() {
                    Vec::new()
                } else {
                    cat!(" (ignored ", ign, ")")
                };
                cat!("nothing changed in ", ms_s(get("t")), i)
            }
            (b"unsatisfied", b"changing") => {
                let mut m = cat!(
                    "still changing after ",
                    ms_s(get("t")),
                    " (last change ",
                    ms_s(get("last")),
                    " at ",
                    geo,
                    ")"
                );
                // Kept from its answer by one small part of the screen (at most a quarter of what
                // was watched: `ready WxH area=N`, finding 231): an animation there is --ignore'd by
                // name (#131).
                let la = dash("late");
                // (`-le "$area"`: test's reading of N; the product, arithmetic's. A number bash's
                // arithmetic refuses, an 08: its own words, then exit 1.)
                let mut area: Option<Vec<u8>> = None;
                if let Some(re) = shell::Re::new("^([0-9]+)x([0-9]+)( area=([0-9]+))?$") {
                    if let Some(c) = re.captures(&scr, false) {
                        let g = |i: usize| c.get(i).copied().flatten().map(|(a, b)| &scr[a..b]);
                        area = Some(match g(4) {
                            Some(a) => a.to_vec(),
                            None => {
                                let (w, h) = (g(1).unwrap_or_default(), g(2).unwrap_or_default());
                                let (Some(w), Some(h)) = (bash_int(w), bash_int(h)) else {
                                    return Err(Exit(1));
                                };
                                w.wrapping_mul(h).to_string().into_bytes()
                            }
                        });
                    }
                }
                if la != b"-" {
                    if let Some(area) = area {
                        let [_, _, lw, lh] = csv4(&la);
                        let (Some(w), Some(h)) = (bash_int(&lw), bash_int(&lh)) else {
                            return Err(Exit(1));
                        };
                        let small = shell::test_int(&area)
                            .is_some_and(|a| 4i64.wrapping_mul(w).wrapping_mul(h) <= a);
                        if small {
                            // (in the window's own coordinates when the --ignore given was, F8)
                            let (ox, oy) = self
                                .origin
                                .clone()
                                .unwrap_or((b"0".to_vec(), b"0".to_vec()));
                            let lo = xywh_less(&la, &ox, &oy);
                            let laz = xywh_words(&la);
                            let Some(t) = bash_int(&get("t")) else {
                                return Err(Exit(1));
                            };
                            let half = ms_s((t / 2).to_string());
                            if self.origin.is_some() {
                                m = cat!(
                                    m,
                                    "; from ",
                                    half,
                                    " on it changed only at ",
                                    laz,
                                    " (the window's ",
                                    lo,
                                    "): --ignore \"",
                                    lo,
                                    "\" if that is an animation"
                                );
                            } else {
                                m = cat!(
                                    m,
                                    "; from ",
                                    half,
                                    " on it changed only at ",
                                    laz,
                                    ": --ignore \"",
                                    laz,
                                    "\" if that is an animation"
                                );
                            }
                        }
                    }
                }
                m
            }
            _ => reason.clone(),
        };
        // run -d --wait (#297, finding 404): a screen that settled after the job had died settled
        // on something else: unsatisfied, so run -d says the job's end and its log as for a 124.
        if res == b"satisfied" && self.facts == "run" && job_failed(ctx, &self.fpid) {
            res = b"unsatisfied".to_vec();
            msg = cat!(msg, "; the command had ended by then: not its screen");
        }
        // An action's --wait (#274): what changed on the desktop since just before the input; on a
        // 124, where the input went first. Not on unknown (the box may be hung).
        let (mut changes, mut target, mut evs) =
            (b"null".to_vec(), b"null".to_vec(), b"null".to_vec());
        self.looked = false;
        if !self.facts.is_empty() {
            self.snap_take();
            if res != b"unknown" && !self.before.is_empty() {
                let (mut tg, mut tt, mut lw) = (Vec::new(), Vec::new(), Vec::new());
                let after = desk_snap(ctx, false);
                let fx = desk_facts(&self.before, &after);
                let e = jq::jq(&["-c", ".events"], Some(&self.before), jq::Err::Null);
                evs = if e.ok() { e.text() } else { b"null".to_vec() };
                if res == b"unsatisfied" || json {
                    tg = match self.facts {
                        "run" => {
                            if self.fpid.is_empty() || after.is_empty() {
                                Vec::new()
                            } else {
                                desk_target(ctx, Target::Run(&self.fpid), &after)
                            }
                        }
                        "keys" => desk_target(ctx, Target::Keys, &self.before),
                        a => desk_target(ctx, Target::At(a, &self.fxy), &self.before),
                    };
                    if !tg.is_empty() {
                        target = jq::jq(&["-c", "del(.text)"], Some(&tg), jq::Err::Inherit).text();
                        // (run -d's own window is said as opened, among the changes)
                        let k = jq::jq(&["-r", ".kind"], Some(&tg), jq::Err::Inherit);
                        let tk = if k.ok() { k.text() } else { Vec::new() };
                        if !(self.facts == "run" && tk == b"window") {
                            let t = jq::jq(&["-r", ".text"], Some(&tg), jq::Err::Inherit);
                            tt = if t.ok() { t.text() } else { Vec::new() };
                        }
                    }
                }
                if !fx.is_empty() {
                    changes = jq::jq(&["-c", ".changes"], Some(&fx), jq::Err::Inherit).text();
                    lw = jq::jq(&["-r", ".line"], Some(&fx), jq::Err::Inherit).text();
                }
                let _ = &tg;
                if res == b"unsatisfied" {
                    if !tt.is_empty() {
                        msg = cat!(msg, "; ", tt);
                    }
                    if !fx.is_empty() {
                        let l: &[u8] = if lw.is_empty() {
                            b"no window, layer or focus changed"
                        } else {
                            &lw
                        };
                        msg = cat!(msg, "; ", l);
                    }
                    if !tt.is_empty() && !fx.is_empty() {
                        self.looked = true;
                    }
                } else if !lw.is_empty() {
                    msg = cat!(msg, "; ", lw);
                }
            }
        }
        if json {
            let r = Args::new(&["-nc"])
                .kv("--arg", "res", &res)
                .kv("--arg", "cond", cond)
                .kv("--arg", "msg", &msg)
                .kv("--arg", "t", get("t"))
                .kv("--arg", "first", get("first"))
                .kv("--arg", "last", get("last"))
                .kv("--arg", "ch", dash("change"))
                .kv("--arg", "ig", dash("ignored"))
                .kv("--arg", "why", dash("why"))
                .kv("--arg", "frames", get("frames"))
                .kv("--arg", "late", dash("late"))
                .kv("--arg", "facts", self.facts)
                .kv("--argjson", "changes", &changes)
                .kv("--argjson", "target", &target)
                .kv("--argjson", "evs", &evs)
                .arg(crate::jqprog::SETTLE_JSON_JQ)
                .run(None, jq::Err::Inherit);
            // (a write that fails is not settle_end's end: its status is the answer's)
            let _ = out(&r.out);
        } else {
            let _ = out(cat!(res, ": ", msg, "\n"));
        }
        Ok(match res.as_slice() {
            b"satisfied" => 0,
            b"unsatisfied" => {
                if !sent.is_empty() {
                    settle_sent(124, sent, self.looked);
                }
                124
            }
            _ => {
                if !sent.is_empty() {
                    settle_sent(1, sent, self.looked);
                }
                1
            }
        })
    }
}

/// settle_sent RC SENT: what an action did when the answer was not yes.
pub fn settle_sent(rc: i32, sent: &[u8], looked: bool) {
    match rc {
        0 => {}
        // (#274: where the input went, and that nothing on the desktop moved, already said)
        124 => {
            if looked {
                say(cat!(
                    sent,
                    " all the same (124 is about the screen): look before sending anything again"
                ));
            } else {
                say(cat!(
                    sent,
                    " all the same (124 is about the screen): take a shot before sending anything again"
                ));
            }
        }
        _ => say(cat!(sent, " before the screen could be watched to the end")),
    }
}

/// A thread that may write to a pipe whose reader is gone gets EPIPE, not the process's SIGPIPE.
fn block_sigpipe() {
    // SAFETY: a signal set of our own, for this thread's mask only.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGPIPE);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry() {
        assert_eq!(
            geom_parse(b"10,20 30x40").map(|g| geom_csv(&g)),
            Some(b"10,20,30,40".to_vec())
        );
        assert_eq!(
            geom_parse(b"-1,2,3,4").map(|g| geom_csv(&g)),
            Some(b"-1,2,3,4".to_vec())
        );
        assert_eq!(geom_parse(b"1,2 0x4"), None);
        assert_eq!(geom_parse(b"1,2 3x4 "), None);
        assert_eq!(rects_meet(b"0,0,10,10", b"5,5,10,10"), Some(true));
        assert_eq!(rects_meet(b"0,0,10,10", b"10,0,10,10"), Some(false));
        assert_eq!(rects_meet(b"08,0,10,10", b"0,0,1,1"), None);
        assert_eq!(rects_meet(b"010,0,1,1", b"8,0,1,1"), Some(true));
        assert_eq!(cursor_rect(b"100 200"), Some(b"84,184,64,64".to_vec()));
        assert_eq!(cursor_rect(b"-5 3"), Some(b"-21,-13,64,64".to_vec()));
    }

    #[test]
    fn options_as_settle_opt() {
        let c = Ctx::new(b"run", b"/x".to_vec());
        let mut s = Settle::default();
        let w = |v: &[&str]| v.iter().map(|a| a.as_bytes().to_vec()).collect::<Vec<_>>();
        s.opt(&c, &w(&["--timeout", "2s"])).ok();
        assert_eq!(s.took, 2);
        s.opt(&c, &w(&["--wait"])).ok();
        assert_eq!((s.took, s.wait), (1, true));
        s.opt(&c, &w(&["-g", "1,2 3x4"])).ok();
        s.opt(&c, &w(&["--other"])).ok();
        assert_eq!(s.took, 0);
        assert_eq!(s.args, w(&["--timeout", "2000", "--region", "1,2,3,4"]));
        assert!(s.in_window(b"10", b"20").is_some());
        assert_eq!(s.args[3], b"11,22,3,4");
        assert_eq!(ws_text(b"3"), b"workspace 3");
        assert_eq!(
            ws_text(b"special:scratchpad"),
            b"the scratchpad (SUPER+S shows it)"
        );
    }
}
