//! An interactive box's monitors (#123, #174): each a window on the user's desktop, made by the box's
//! Hyprland (`output create wayland NAME`), and the box's windows there laid out as its layout scaled
//! as a whole, a display-settings picture (desk_relay, desk_picture). What is done on the host is
//! done on purpose, by address (no focus moves), and a host rule catching the new window is on for
//! as little as it takes (finding 70).
//!
//! As in mon.rs: what bash runs inside `$(...)` runs with errexit off; ERREXIT says where it is on.

use crate::boxes::{BoxPid, box_pid, meta};
use crate::cat;
use crate::exec::{ErrTo, Out, on_box};
use crate::exit::{Ctx, Exit, Res, say};
use crate::host::{host_eval, host_hyprctl};
use crate::hypr::{HYPR_LIMIT, hypr_json};
use crate::jq::{self, Args};
use crate::jqprog::JQ_MON;
use crate::mon::{
    box_layout, field1, line_starts, monitor_place, monitor_spec, monitors_file, monitors_place,
    monitors_record, monitors_write, records, w, with_line,
};
use crate::screen::main_screen;
use crate::settle::secs;
use crate::shell::{self, bash_int, strip_nl};
use std::time::Duration;

/// ws_check WS: where interactive and peek windows open (a workspace 1-99, `special`, or
/// special:NAME: #292, finding 423), in its normal form; None for anything else.
pub fn ws_check(v: &[u8]) -> Option<Vec<u8>> {
    if v == b"special" {
        return Some(b"special:scratchpad".to_vec());
    }
    if let Some(n) = v.strip_prefix(b"special:") {
        return shell::re_match("^[A-Za-z0-9_-]{1,32}$", n).then(|| v.to_vec());
    }
    shell::re_match("^[1-9][0-9]?$", v).then(|| v.to_vec())
}

/// The box's windows' pid on the host: the outer bwrap's (omabox-wlfd connects, then execs it), the
/// parent of the box's PID 1 (`ps -o ppid= -p "$(box_pid)"`).
fn win_pid(ctx: &Ctx) -> Vec<u8> {
    match box_pid(&ctx.d) {
        BoxPid::Pid(p) => crate::procfs::proc_ppid(&p).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `$(meta workspace)` through ws_check, 9 when box.json has none.
fn box_ws(ctx: &Ctx) -> Option<Vec<u8>> {
    let ws = meta(&ctx.d, "workspace").0;
    ws_check(if ws.is_empty() { b"9" } else { &ws })
}

/// aq_monitor_note VERSION: what an interactive box's monitor windows lack on that aquamarine
/// (findings 238, 239, 247).
fn aq_monitor_note(ctx: &Ctx, v: &[u8]) {
    let n = &ctx.name;
    if !crate::exit::contains(v, b"+layout") {
        say(cat!(
            "monitor add: box '",
            n,
            "''s aquamarine (",
            v,
            ") puts your pointer in a monitor's window in the wrong place, over the whole layout (it lacks omabox's patch): omabox setup --aquamarine, then a new box"
        ));
    } else if !crate::exit::contains(v, b"+cursor") {
        say(cat!(
            "monitor add: box '",
            n,
            "''s aquamarine (",
            v,
            ") hides your cursor when it goes from one monitor's window to another (it lacks omabox's patch): omabox setup --aquamarine, then a new box"
        ));
    }
}

/// `jq ARGS <<<"$INPUT"` as `$(...)` keeps it, its words on stderr; its status too.
fn jqt(a: Args, input: &[u8]) -> (i32, Vec<u8>) {
    let r = a.run(Some(input), jq::Err::Inherit);
    (r.rc, r.text())
}

/// `[ "$(jq --argjson p PID '[.[] | select(.pid == $p)] | length' <<<"$C")" ]`: how many windows
/// of the box's pid.
fn n_of(pid: &[u8], clients: &[u8]) -> Vec<u8> {
    jqt(
        Args::new(&[])
            .kv("--argjson", "p", pid)
            .arg(crate::jqprog::PID_COUNT_JQ),
        clients,
    )
    .1
}

/// `$(desk_windows [CLIENTS])`: the box's windows on the host, "NAME ADDRESS" a line, the main one as
/// `main` first (none when it is closed), then its monitors' ($D/monitor-windows, in the order
/// made) still open. None where bash's returns 1.
pub fn desk_windows(ctx: &Ctx, clients: Option<&[u8]>) -> Option<Vec<u8>> {
    let clients = match clients {
        Some(c) if !c.is_empty() => c.to_vec(),
        _ => {
            let (rc, c) = host_hyprctl(ctx, &[b"-j", b"clients"], false);
            if rc != 0 {
                return None;
            }
            strip_nl(&c)
        }
    };
    let pid = win_pid(ctx);
    if !shell::re_match("^[0-9]+$", &pid) {
        return None;
    }
    let ws = box_ws(ctx)?;
    let mw = std::fs::read(ctx.at("monitor-windows")).unwrap_or_default();
    let mut known: Vec<Vec<u8>> = Vec::new();
    for l in records(&mw) {
        let f = shell::read_fields(l, 2);
        let ok = Args::new(&["-e"])
            .kv("--arg", "a", &f[1])
            .arg(crate::jqprog::ANY_ADDR_JQ)
            .run(Some(&clients), jq::Err::Null)
            .ok();
        if ok {
            known.push(cat!(f[0], " ", f[1]));
        }
    }
    // (`printf '%s\n' "${known[@]}" | jq -R 'select(. != "") | split(" ")[1]' | jq -s .`)
    let lines = cat!(known.join(&b'\n'), "\n");
    let k = jq::tool(
        "jq",
        &["-R", r#"select(. != "") | split(" ")[1]"#],
        Some(&lines),
        jq::Err::Inherit,
    );
    let k = jq::tool("jq", &["-s", "."], Some(&k.out), jq::Err::Inherit).text();
    let r = Args::new(&["-r"])
        .kv("--argjson", "p", &pid)
        .kv("--arg", "ws", &ws)
        .kv("--argjson", "k", &k)
        .arg(crate::jqprog::DESK_MAIN_JQ)
        .run(Some(&clients), jq::Err::Inherit);
    let mut out = r.out;
    for l in &known {
        out.extend_from_slice(&cat!(l, "\n"));
    }
    Some(strip_nl(&out))
}

/// desk_prune: a window closed on the desktop unplugged its monitor: its line and its address go
/// before the picture is laid out again. ERREXIT: a mv of the windows' record that fails ends the
/// command there.
pub fn desk_prune(ctx: &Ctx, errexit: bool) -> Res<()> {
    let Ok(m) = hypr_json(ctx, &["monitors"]) else {
        return Ok(());
    };
    let m = strip_nl(&m);
    let mut keep: Vec<Vec<u8>> = Vec::new();
    let f = monitors_file(ctx);
    for l in records(&f) {
        // (`read -r line`: its blanks at either end off; `${line%% *}` its first word)
        let line = shell::read_fields(l, 1).remove(0);
        let n = line
            .split(|&c| c == b' ')
            .next()
            .unwrap_or_default()
            .to_vec();
        if n.is_empty() {
            continue;
        }
        if crate::cmd::output::listed(&m, &n) {
            keep.push(line);
        }
    }
    let kept = shell::grep_lines(&keep.join(&b'\n'));
    if kept != shell::grep_lines(&strip_nl(&monitors_file(ctx))) {
        monitors_write(ctx, &kept);
    }
    let mw = ctx.at("monitor-windows");
    if mw.is_file() {
        let names = jq::jq(
            &["-r", r#"map(.name) | join("|")"#],
            Some(&m),
            jq::Err::Inherit,
        )
        .text();
        let r = jq::tool(
            "grep",
            &[
                b"-E".to_vec(),
                cat!("^(", names, ") "),
                shell::bytes(&mw).to_vec(),
            ],
            None,
            jq::Err::Inherit,
        );
        let new = cat!(ctx.d, "/monitor-windows.new");
        if let Err(e) = std::fs::write(shell::path(&new), &r.out) {
            say(cat!(new, ": ", crate::shots::why(&e)));
        }
        if !crate::shots::mv_f(&new, shell::bytes(&mw)) && errexit {
            return Err(Exit(1));
        }
    }
    Ok(())
}

/// Under ERREXIT a step whose status is RC ends the command with it.
fn chk(errexit: bool, rc: i32) -> Res<()> {
    if errexit && rc != 0 {
        return Err(Exit(u8::try_from(rc & 0xff).unwrap_or(1)));
    }
    Ok(())
}

/// `$((EXPR))` of the words desk_relay adds up (jq's and the picture's integers): bash's reading,
/// 0 for an empty one; None where bash's arithmetic refuses one (it ends the command, 1).
fn ints(v: &[&[u8]]) -> Option<Vec<i64>> {
    v.iter().map(|x| bash_int(x)).collect()
}

/// desk_relay [LINE]: lay an interactive box's windows out on the host as its layout, scaled as a
/// whole (#174), on the host monitor its main window is on, in that monitor's free area, the
/// picture centred, one scale for all (desk_picture). The main window is floated for it and made the
/// size of its view; with no monitors left it goes back to tiling, if it tiled before. With LINE (a
/// monitor about to be made), its window's size and place relative to the host monitor, "W H X Y".
/// Moves by address: no focus moves.
pub fn desk_relay(ctx: &Ctx, new: &[u8], errexit: bool) -> Res<Vec<u8>> {
    let (rc, clients) = host_hyprctl(ctx, &[b"-j", b"clients"], false);
    if rc != 0 {
        return Err(ctx.die("monitor: cannot read your desktop's windows"));
    }
    let clients = strip_nl(&clients);
    let (rc, mons) = host_hyprctl(ctx, &[b"-j", b"monitors"], false);
    if rc != 0 {
        return Err(ctx.die("monitor: cannot read your desktop's monitors"));
    }
    let mons = strip_nl(&mons);
    let Some(wins) = desk_windows(ctx, Some(&clients)) else {
        return Err(ctx.die(cat!(
            "monitor: cannot find box '",
            ctx.name,
            "''s windows on your desktop"
        )));
    };
    let main = records(&wins)
        .into_iter()
        .find(|l| field1(l) == b"main")
        .map(|l| {
            crate::mon::awk_fields(l)
                .get(1)
                .copied()
                .unwrap_or_default()
                .to_vec()
        })
        .unwrap_or_default();
    let mut mon = Vec::new();
    if !main.is_empty() {
        let (_, id) = jqt(
            Args::new(&[])
                .kv("--arg", "a", &main)
                .arg(crate::jqprog::WIN_MONITOR_JQ),
            &clients,
        );
        let (rc, m) = jqt(
            Args::new(&["-c"])
                .kv("--argjson", "id", &id)
                .arg(crate::jqprog::MON_BY_ID_JQ),
            &mons,
        );
        chk(errexit, rc)?;
        mon = m;
    }
    if mon.is_empty() {
        let (rc, m) = jqt(Args::new(&["-c", crate::jqprog::MON_FOCUSED_JQ]), &mons);
        chk(errexit, rc)?;
        mon = m;
    }
    if mon.is_empty() {
        return Err(ctx.die("monitor: cannot read your monitor's size"));
    }
    // No monitors left: the main window back as it was.
    if new.is_empty() && shell::grep_lines(&monitors_file(ctx)).is_empty() {
        let mf = std::fs::read(ctx.at("main-window")).map(|v| strip_nl(&v));
        if !main.is_empty() && mf.is_ok_and(|v| v == cat!(main, " tiled")) {
            host_eval(
                ctx,
                &cat!(
                    "hl.dispatch(hl.dsp.window.float({ action = 'disable', window = 'address:",
                    main,
                    "' }))"
                ),
            )?;
        }
        let _ = std::fs::remove_file(ctx.at("main-window"));
        return Ok(Vec::new());
    }
    let opt = |o: &[u8], prog: &str| -> Vec<u8> {
        let (rc, v) = host_hyprctl(ctx, &[b"-j", b"getoption", o], false);
        let r = jq::tool("jq", &["-r", prog], Some(&v), jq::Err::Null);
        if rc != 0 || !r.ok() {
            return Vec::new();
        }
        r.text()
    };
    let mut gap = opt(b"general:gaps_out", crate::jqprog::GAPS_JQ);
    let mut bw = opt(b"general:border_size", ".int // empty");
    if !shell::re_match("^[0-9]+$", &gap) {
        gap = b"10".to_vec();
    }
    if !shell::re_match("^[0-9]+$", &bw) {
        bw = b"2".to_vec();
    }
    let Some(g) = ints(&[&gap, &bw]).map(|v| v[0].wrapping_add(v[1])) else {
        return Err(Exit(1));
    };
    let g = g.to_string();
    let r = Args::new(&["-r"])
        .kv("--argjson", "g", &g)
        .arg(format!("{JQ_MON}{}", crate::jqprog::DESK_AREA_JQ))
        .run(Some(&mon), jq::Err::Inherit);
    // (`read -r ... < <(jq ...)`: nothing read is read's failure)
    if r.out.is_empty() {
        chk(errexit, 1)?;
    }
    let a = shell::read_fields(&r.out, 6);
    let (hrc, hw) = jqt(Args::new(&["-r", &format!("{JQ_MON}lw | floor")]), &mon);
    chk(errexit, hrc)?;
    let (hrc, hh) = jqt(Args::new(&["-r", &format!("{JQ_MON}lh | floor")]), &mon);
    chk(errexit, hrc)?;
    let Some(lay) = box_layout(ctx, if new.is_empty() { None } else { Some(new) }) else {
        return Err(ctx.die(cat!(
            "monitor: box '",
            ctx.name,
            "' was started by an omabox before its windows were laid out as its layout (#174): start it again"
        )));
    };
    let r = jq::tool(
        "awk",
        &[crate::jqprog::DESK_PICTURE_AWK],
        Some(&cat!(a[4], " ", a[5], " ", g, "\n", lay, "\n")),
        jq::Err::Inherit,
    );
    if !r.ok() {
        return Err(ctx.die(cat!(
            "monitor: cannot lay out box '",
            ctx.name,
            "''s windows"
        )));
    }
    let pic = strip_nl(&r.out);
    let mut pl = pic.split(|&c| c == b'\n');
    let head = shell::read_fields(pl.next().unwrap_or_default(), 2);
    let (k, fitted) = (&head[0], &head[1]);
    if fitted == b"0" {
        say(cat!(
            "monitor: box '",
            ctx.name,
            "''s layout does not fit on your ",
            hw,
            "x",
            hh,
            " monitor even at a quarter of its size (Hyprland's smallest scale): its windows go past its edge"
        ));
    } else {
        let pct = jq::tool(
            "awk",
            &[
                b"-v".to_vec(),
                cat!("k=", k),
                crate::jqprog::DESK_PCT_AWK.as_bytes().to_vec(),
            ],
            None,
            jq::Err::Inherit,
        );
        say(cat!(
            "box '",
            ctx.name,
            "''s windows: its layout at ",
            pct.out,
            " of its size"
        ));
    }
    // The main window floated (it tiled: kept, to tile again when the monitors are gone), then each
    // to its place; the new one's printed.
    if !main.is_empty() {
        let (_, fl) = jqt(
            Args::new(&["-r"])
                .kv("--arg", "a", &main)
                .arg(crate::jqprog::WIN_FLOATING_JQ),
            &clients,
        );
        if fl != b"true" {
            if let Err(e) = std::fs::write(ctx.at("main-window"), cat!(main, " tiled\n")) {
                say(cat!(ctx.d, "/main-window: ", crate::shots::why(&e)));
                chk(errexit, 1)?;
            }
            host_eval(
                ctx,
                &cat!(
                    "hl.dispatch(hl.dsp.window.float({ action = 'enable', window = 'address:",
                    main,
                    "' }))"
                ),
            )?;
        }
    }
    let mut said = Vec::new();
    let rows: Vec<&[u8]> = pl.collect();
    let new_name = new.split(|&c| c == b' ').next().unwrap_or_default();
    for row in &rows {
        let f = shell::read_fields(row, 5);
        let (name, x, y, wd, ht) = (&f[0], &f[1], &f[2], &f[3], &f[4]);
        if !new.is_empty() && name == new_name {
            let Some(v) = ints(&[&a[2], &a[0], x, &a[3], &a[1], y]) else {
                return Err(Exit(1));
            };
            said.extend_from_slice(&cat!(
                wd,
                " ",
                ht,
                " ",
                (v[0] - v[1] + v[2]).to_string(),
                " ",
                (v[3] - v[4] + v[5]).to_string(),
                "\n"
            ));
            continue;
        }
        let addr = records(&wins)
            .into_iter()
            .find(|l| field1(l) == name.as_slice())
            .map(|l| {
                crate::mon::awk_fields(l)
                    .get(1)
                    .copied()
                    .unwrap_or_default()
                    .to_vec()
            })
            .unwrap_or_default();
        if addr.is_empty() {
            continue;
        }
        host_eval(
            ctx,
            &cat!(
                "hl.dispatch(hl.dsp.window.resize({ window = 'address:",
                addr,
                "', x = ",
                wd,
                ", y = ",
                ht,
                " }))"
            ),
        )?;
        let Some(v) = ints(&[&a[2], x, &a[3], y]) else {
            return Err(Exit(1));
        };
        host_eval(
            ctx,
            &cat!(
                "hl.dispatch(hl.dsp.window.move({ window = 'address:",
                addr,
                "', x = ",
                (v[0] + v[1]).to_string(),
                ", y = ",
                (v[2] + v[3]).to_string(),
                " }))"
            ),
        )?;
    }
    // The box's monitors follow their windows' new sizes: wait for the main one, at most 3 s.
    if !main.is_empty() {
        let end = secs() + 3;
        let want = rows
            .iter()
            .filter(|l| field1(l) == b"main")
            .map(|l| {
                let f = crate::mon::awk_fields(l);
                let g = |i: usize| f.get(i).copied().unwrap_or_default();
                cat!(g(3), "x", g(4))
            })
            .collect::<Vec<_>>()
            .join(&b'\n');
        loop {
            let now = match hypr_json(ctx, &["monitors"]) {
                Ok(m) => jq::tool(
                    "jq",
                    &[
                        "-r",
                        &format!("{}{}", main_screen(ctx), crate::jqprog::WH_JQ),
                    ],
                    Some(&m),
                    jq::Err::Inherit,
                )
                .text(),
                Err(_) => Vec::new(),
            };
            if now == want || secs() >= end {
                break;
            }
            crate::trap::sleep(b"0.05");
        }
    }
    Ok(strip_nl(&said))
}

const RULE_OFF: &[u8] = b"if omabox_monitor_rule then omabox_monitor_rule:set_enabled(false) end";

/// monitor_add_window's EXIT trap (MON_RULE_ON): the host rule off however the subshell ends (a
/// refusal; INT, TERM or HUP), quietly; once its window is there when the box was told to make it
/// (MON_CREATING: finding 429), at most 5 s: turned off first, the window the box makes anyway
/// would open on the user's active workspace, focused (finding 70).
struct RuleGuard {
    ctx: Ctx,
    trap: Option<crate::trap::Trap>,
    creating: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl RuleGuard {
    fn arm(ctx: &Ctx, pid: &[u8], n0: &[u8]) -> RuleGuard {
        let c = ctx.clone();
        let creating = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (cr, pid, n0) = (creating.clone(), pid.to_vec(), n0.to_vec());
        let trap = crate::trap::on_exit(move || {
            crate::exit::muted(|| {
                if cr.load(std::sync::atomic::Ordering::SeqCst) {
                    window_wait(&c, &pid, &n0);
                }
                host_hyprctl(&c, &[b"eval", RULE_OFF], true);
            });
        });
        RuleGuard {
            ctx: ctx.clone(),
            trap: Some(trap),
            creating,
        }
    }

    /// `MON_CREATING=1` / `MON_CREATING=""`.
    fn creating(&self, on: bool) {
        self.creating.store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// `host_eval "omabox_monitor_rule:set_enabled(false)"; MON_RULE_ON=""`.
    fn off(&mut self) -> Res<()> {
        host_eval(&self.ctx, b"omabox_monitor_rule:set_enabled(false)")?;
        if let Some(t) = self.trap.take() {
            t.forget();
        }
        Ok(())
    }
}

/// mon_window_wait PID N0: until the host has more than N0 windows of PID, 5 s at most (the trap's).
fn window_wait(ctx: &Ctx, pid: &[u8], n0: &[u8]) {
    for _ in 0..50 {
        let (rc, c) = host_hyprctl(ctx, &[b"-j", b"clients"], true);
        let n = jq::tool(
            "jq",
            &[
                b"--argjson".to_vec(),
                b"p".to_vec(),
                pid.to_vec(),
                crate::jqprog::PID_COUNT_JQ.as_bytes().to_vec(),
            ],
            Some(&c),
            jq::Err::Null,
        )
        .text();
        let more = shell::test_int(&n)
            .zip(shell::test_int(n0))
            .is_some_and(|(a, b)| a > b);
        if rc == 0 && more {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `$(monitor_add_window SPEC)`: an interactive box's monitor (#123), a window on the user's
/// desktop as its first one is: the host rule for aquamarine windows (workspace, silent, no focus,
/// floating at its size and place) on from just before the window is made until it is there, at
/// most 5 s. In the box it is placed as a headless box's, kept in omabox.monitors; on the host the
/// picture is laid out again with it (#174). Its line as Hyprland made it.
pub fn monitor_add_window(ctx: &mut Ctx, spec: &[u8]) -> Res<Vec<u8>> {
    let Ok([mode, scale, place]) = monitor_spec(spec) else {
        return Err(Exit(1));
    };
    let Some(ws) = box_ws(ctx) else {
        return Err(ctx.die(cat!(
            "monitor add: box '",
            ctx.name,
            "' has no usable workspace in box.json"
        )));
    };
    let pid = win_pid(ctx);
    if !shell::re_match("^[0-9]+$", &pid) {
        return Err(ctx.die(cat!(
            "monitor add: cannot find box '",
            ctx.name,
            "''s windows on your desktop"
        )));
    }
    let aq = Args::new(&["-r", r#".aquamarine.version // "?""#])
        .arg(cat!(ctx.d, "/box.json"))
        .run_file(jq::Err::Inherit)
        .text();
    aq_monitor_note(ctx, &aq);
    desk_prune(ctx, false)?;
    // Named here (the next free WAYLAND-N), so its line, and so its place and scale, are there
    // before it is.
    let m = hypr_json(ctx, &["monitors"])
        .map(|m| strip_nl(&m))
        .unwrap_or_default();
    let mut i = 2;
    loop {
        let n = format!("WAYLAND-{i}");
        if !(crate::cmd::output::listed(&m, n.as_bytes())
            || line_starts(&monitors_file(ctx), n.as_bytes()))
        {
            break;
        }
        i += 1;
    }
    let name = format!("WAYLAND-{i}").into_bytes();
    let line = cat!(name, " ", mode, " ", monitor_place(&place), " ", scale);
    // The picture with it: the windows there moved (and the main one floated) first, then its line.
    let rect = desk_relay(ctx, &line, false).map_err(|_| Exit(1))?;
    let rest = strip_nl(&monitors_file(ctx));
    if !monitors_write(ctx, &with_line(&rest, &line)) {
        return Err(ctx.die(cat!(
            "monitor add: could not record ",
            name,
            " for the box's config reloads"
        )));
    }
    if monitors_place(ctx).is_err() {
        monitors_write(ctx, &rest);
        return Err(ctx.die(cat!(
            "monitor add: placing box '",
            ctx.name,
            "''s monitors failed"
        )));
    }
    let (rc, clients) = host_hyprctl(ctx, &[b"-j", b"clients"], false);
    if rc != 0 {
        return Err(ctx.die("monitor add: cannot read your desktop's windows"));
    }
    let n0 = n_of(&pid, &strip_nl(&clients));
    let r4 = shell::read_fields(&rect, 4);
    let mut guard = RuleGuard::arm(ctx, &pid, &n0);
    // The rule's `move` is relative to its monitor; always given: a rule of the same name keeps an
    // earlier one's.
    host_eval(
        ctx,
        &cat!(
            "omabox_monitor_rule = hl.window_rule({ name = 'omabox-monitor', match = { class = '^aquamarine$' }, workspace = '",
            ws,
            " silent', no_initial_focus = true, render_unfocused = true, float = true, size = '",
            r4[0],
            " ",
            r4[1],
            "', move = '",
            r4[2],
            " ",
            r4[3],
            "' })"
        ),
    )?;
    guard.creating(true);
    let r = on_box(
        ctx,
        &w(&[
            b"timeout",
            HYPR_LIMIT.as_bytes(),
            b"hyprctl",
            b"output",
            b"create",
            b"wayland",
            &name,
        ]),
        false,
        Out::Capture(None),
        ErrTo::Out,
        false,
    );
    let res = shell::subst(&r.out);
    if res != b"ok" {
        guard.creating(false);
        monitors_write(ctx, &rest);
        return Err(ctx.die(cat!(
            "monitor add: the box could not make another window: ",
            res
        )));
    }
    let end = secs() + 5;
    let mut clients;
    loop {
        let (rc, c) = host_hyprctl(ctx, &[b"-j", b"clients"], false);
        clients = strip_nl(&c);
        if rc == 0 {
            let n = n_of(&pid, &clients);
            let gt = shell::test_int(&n)
                .zip(shell::test_int(&n0))
                .is_some_and(|(a, b)| a > b);
            if gt {
                break;
            }
        }
        if secs() >= end {
            guard.creating(false);
            return Err(ctx.die(cat!(
                "monitor add: no new window of box '",
                ctx.name,
                "' on your desktop in 5 s"
            )));
        }
        crate::trap::sleep(b"0.05");
    }
    guard.off()?;
    // The new window: the box's one that is neither the main one nor a known monitor's.
    let wins = desk_windows(ctx, Some(&clients)).unwrap_or_default();
    let addrs: Vec<u8> = records(&wins)
        .into_iter()
        .map(|l| {
            cat!(
                crate::mon::awk_fields(l)
                    .get(1)
                    .copied()
                    .unwrap_or_default(),
                "\n"
            )
        })
        .collect::<Vec<_>>()
        .concat();
    let known = jq::tool("jq", &["-R", "."], Some(&addrs), jq::Err::Inherit);
    let known = jq::tool("jq", &["-sc", "."], Some(&known.out), jq::Err::Inherit).text();
    let addr = jqt(
        Args::new(&["-r"])
            .kv("--argjson", "p", &pid)
            .kv("--argjson", "k", &known)
            .arg(crate::jqprog::NEW_WINDOW_JQ),
        &clients,
    )
    .1;
    let end = secs() + 5;
    loop {
        if let Ok(m) = hypr_json(ctx, &["monitors"]) {
            if crate::cmd::output::listed(&strip_nl(&m), &name) {
                break;
            }
        }
        if secs() >= end {
            return Err(ctx.die(cat!("monitor add: the box lists no monitor ", name)));
        }
        crate::trap::sleep(b"0.05");
    }
    if !addr.is_empty() {
        use std::io::Write;
        let f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(ctx.at("monitor-windows"));
        match f {
            Ok(mut f) => {
                let _ = f.write_all(&cat!(name, " ", addr, "\n"));
            }
            Err(e) => say(cat!(ctx.d, "/monitor-windows: ", crate::shots::why(&e))),
        }
    }
    // Its own view's scale, from its window's size (the views' until then).
    if monitors_place(ctx).is_err() {
        return Err(ctx.die(cat!(
            "monitor add: ",
            name,
            " is there, but setting its place and scale failed"
        )));
    }
    crate::trap::sleep(b"0.3");
    monitors_record(ctx, false)?;
    // (`hypr_json monitors | jq ...` last: its status the subshell's, pipefail's)
    let m = hypr_json(ctx, &["monitors"])?;
    let r = jq::tool(
        "jq",
        &[
            b"-r".to_vec(),
            b"--arg".to_vec(),
            b"n".to_vec(),
            name.clone(),
            crate::jqprog::MON_LINE_JQ.as_bytes().to_vec(),
        ],
        Some(&m),
        jq::Err::Inherit,
    );
    crate::cmd::hyprctl::code(r.rc)?;
    Ok(r.text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspaces_as_ws_check_takes_them() {
        assert_eq!(ws_check(b"special"), Some(b"special:scratchpad".to_vec()));
        assert_eq!(
            ws_check(b"special:omabox"),
            Some(b"special:omabox".to_vec())
        );
        assert_eq!(ws_check(b"special:"), None);
        assert_eq!(ws_check(b"9"), Some(b"9".to_vec()));
        assert_eq!(ws_check(b"99"), Some(b"99".to_vec()));
        assert_eq!(ws_check(b"0"), None);
        assert_eq!(ws_check(b"100"), None);
    }
}
