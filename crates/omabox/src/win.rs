//! The box's windows as bin/omabox reads them (finding 81): Hyprland's lists annotated by WIN_JQ
//! (box_windows, win_annotate), the one window a selector picks (win_select, #20, #37, #244), and
//! their words in messages (win_desc, win_lines). jq runs the programs, so the answers are jq's.

use crate::cat;
use crate::exec::{ErrTo, Out, on_box};
use crate::exit::{Ctx, Exit, Res, say};
use crate::hypr::{HYPR_LIMIT, hypr_json};
use crate::jq::{self, Args};
use crate::jqprog::{WIN_JQ, WIN_SEL_JQ};
use crate::settle::secs;
use crate::shell::{self, strip_nl};
use std::io::Write;

/// win_annotate CLIENTS MONITORS ACTIVEWINDOW [LAYERS]: WIN_JQ's list (its status and output as
/// jq's; its errors on this process's stderr).
pub fn win_annotate(c: &[u8], m: &[u8], a: &[u8], l: Option<&[u8]>) -> jq::Ran {
    Args::new(&["-c", "-n"])
        .kv("--argjson", "c", c)
        .kv("--argjson", "m", m)
        .kv("--argjson", "a", a)
        .kv("--argjson", "l", l.unwrap_or(b"null"))
        .arg(WIN_JQ)
        .run(None, jq::Err::Inherit)
}

/// box_windows [LAYERS]: one question at a time, the first left unanswered the end (hypr_json, its
/// die); then win_annotate, whose status is the function's (`wins=$(box_windows) || exit $?`).
pub fn box_windows(ctx: &Ctx, layers: Option<&[u8]>) -> Res<Vec<u8>> {
    let c = strip_nl(&hypr_json(ctx, &["clients"])?);
    let m = strip_nl(&hypr_json(ctx, &["monitors"])?);
    let a = strip_nl(&hypr_json(ctx, &["activewindow"])?);
    let r = win_annotate(&c, &m, &a, layers.filter(|l| !l.is_empty()));
    if !r.ok() {
        return Err(Exit(u8::try_from(r.rc).unwrap_or(1)));
    }
    Ok(r.text())
}

/// `${hits#jq: error*: }`: jq's error without its own lead ("jq: error (at <stdin>:1): ").
pub fn jq_error_text(hits: &[u8]) -> &[u8] {
    const LEAD: &[u8] = b"jq: error";
    if !hits.starts_with(LEAD) {
        return hits;
    }
    match hits[LEAD.len()..].windows(2).position(|w| w == b": ") {
        Some(i) => &hits[LEAD.len() + i + 2..],
        None => hits,
    }
}

/// `$*` of the selectors.
pub fn joined(sels: &[Vec<u8>]) -> Vec<u8> {
    sels.join(&b' ')
}

/// WIN_SEL_JQ's matches of SELS among WINS, as `hits=$(jq ... 2>&1)`: Ok(hits), or Err(jq's words)
/// for a selector jq cannot read (a bad regex).
pub fn win_hits(wins: &[u8], sels: &[Vec<u8>]) -> Result<Vec<u8>, Vec<u8>> {
    let mut a = Args::new(&["-c"]).arg(WIN_SEL_JQ).arg("--args");
    for s in sels {
        a = a.arg(s);
    }
    let r = a.run(Some(wins), jq::Err::Out);
    if r.ok() { Ok(r.text()) } else { Err(r.text()) }
}

/// `jq length <<<"$X"`.
pub fn jq_length(x: &[u8]) -> Vec<u8> {
    jq::jq(&["length"], Some(x), jq::Err::Inherit).text()
}

/// win_select WINDOWS SEL...: the one window (win_annotate's object); none or several are exit 2,
/// with the candidates.
pub fn win_select(ctx: &Ctx, wins: &[u8], sels: &[Vec<u8>]) -> Res<Vec<u8>> {
    if sels.is_empty() {
        let c: &[u8] = if ctx.main_cmd.is_empty() {
            b"omabox"
        } else {
            &ctx.main_cmd
        };
        return Err(ctx.die(cat!(c, ": --window SEL: no selector given")));
    }
    // No list at all (not an empty one): the box did not answer (finding 176).
    if wins.is_empty() {
        return Err(ctx.die(cat!(
            "box '",
            ctx.name,
            "' did not list its windows (is it going down?)"
        )));
    }
    let hits = match win_hits(wins, sels) {
        Ok(h) => h,
        Err(e) => {
            say(cat!(
                "bad window selector (",
                joined(sels),
                "): ",
                jq_error_text(&e)
            ));
            return Err(Exit(2));
        }
    };
    let n = jq_length(&hits);
    match n.as_slice() {
        b"1" => return Ok(jq::jq(&["-c", ".[0]"], Some(&hits), jq::Err::Inherit).text()),
        b"0" => {
            say(cat!(
                "no window matches ",
                joined(sels),
                "; the box's windows:"
            ));
            win_lines(wins);
        }
        _ => {
            say(cat!(
                joined(sels),
                " matches ",
                n,
                " windows: name one by its address (or add a selector):"
            ));
            win_lines(&hits);
        }
    }
    Err(Exit(2))
}

/// win_lines LIST >&2: a window a line, for a refusal.
pub fn win_lines(list: &[u8]) {
    let r = jq::jq(
        &["-r", crate::jqprog::WIN_LINES_JQ],
        Some(list),
        jq::Err::Inherit,
    );
    let _ = std::io::stderr().write_all(&r.out);
}

/// win_desc WINDOW: `0x... class "title"`, for messages.
pub fn win_desc(w: &[u8]) -> Vec<u8> {
    jq::jq(
        &["-r", crate::jqprog::WIN_DESC_ONE_JQ],
        Some(w),
        jq::Err::Inherit,
    )
    .text()
}

/// `jq -r PROGRAM <<<"$X"` as `$(...)`.
pub fn jq_r(prog: &str, x: &[u8]) -> Vec<u8> {
    jq::jq(&["-r", prog], Some(x), jq::Err::Inherit).text()
}

/// `out=$(on_box ARGS... 2>&1) || true`.
fn said(ctx: &Ctx, args: &[&[u8]]) -> Vec<u8> {
    let a: Vec<Vec<u8>> = args.iter().map(|a| a.to_vec()).collect();
    let r = on_box(ctx, &a, false, Out::Capture(None), ErrTo::Out);
    shell::subst(&r.out)
}

/// A window's address, checked as one: it goes into Hyprland's Lua.
fn address(ctx: &Ctx, w: &[u8]) -> Res<Vec<u8>> {
    let addr = jq_r(".address", w);
    if !shell::re_match("^0x[0-9a-f]+$", &addr) {
        return Err(ctx.die(cat!("bad window address: ", addr)));
    }
    Ok(addr)
}

/// win_raise WINDOW: focused as a user would focus it (its workspace to the screen, a group's tab to
/// the front, a floating one raised; a tiled one stays under the floats), until it is the active
/// window, 3 s at most.
pub fn win_raise(ctx: &Ctx, w: &[u8]) -> Res<()> {
    let addr = address(ctx, w)?;
    // (audit E12: which window, and the way round: --no-raise acts on it where it is)
    let way = "--no-raise acts on it where it is, if uncovered; shot --window takes it covered";
    let out = said(
        ctx,
        &[
            b"hyprctl",
            b"dispatch",
            &cat!("hl.dsp.focus({ window = 'address:", addr, "' })"),
        ],
    );
    if out != b"ok" {
        return Err(ctx.die(cat!(
            ctx.label(),
            "could not focus window ",
            win_desc(w),
            ": ",
            out,
            " (",
            way,
            ")"
        )));
    }
    if jq_r(".floating", w) == b"true" {
        let out = said(
            ctx,
            &[
                b"hyprctl",
                b"dispatch",
                &cat!(
                    "hl.dsp.window.alter_zorder({ mode = 'top', window = 'address:",
                    addr,
                    "' })"
                ),
            ],
        );
        if out != b"ok" {
            return Err(ctx.die(cat!(
                ctx.label(),
                "could not raise window ",
                win_desc(w),
                ": ",
                out,
                " (",
                way,
                ")"
            )));
        }
    }
    let deadline = secs() + 3;
    loop {
        let a = strip_nl(&hypr_json(ctx, &["activewindow"])?);
        if jq_r(crate::jqprog::ACTIVE_ADDR_JQ, &a) == addr {
            return Ok(());
        }
        if secs() >= deadline {
            return Err(ctx.die(cat!(
                ctx.label(),
                "window ",
                win_desc(w),
                " did not take focus in 3 s: something holds it (a layer, a fullscreen window: omabox windows; ",
                way,
                ")"
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// win_refocus WINDOW: the active window focused again, the pointer left where it is (keys --window,
/// #201): Hyprland shows a window active while the keyboard is elsewhere; a focus gives it back. Its
/// warp is held off.
pub fn win_refocus(ctx: &Ctx, w: &[u8]) -> Res<()> {
    let addr = address(ctx, w)?;
    let lua = cat!(
        "local nw = hl.get_config('cursor.no_warps')\nhl.config({ cursor = { no_warps = true } })\nlocal ok, err = pcall(hl.dispatch, hl.dsp.focus({ window = 'address:",
        addr,
        "' }))\nhl.config({ cursor = { no_warps = nw } })\nif not ok then error(err, 0) end"
    );
    let out = said(
        ctx,
        &[b"timeout", HYPR_LIMIT.as_bytes(), b"hyprctl", b"eval", &lua],
    );
    if out != b"ok" {
        return Err(ctx.die(cat!(
            ctx.label(),
            "could not focus window ",
            win_desc(w),
            " again: ",
            out,
            " (omabox windows; keys without --window types into what has the keyboard)"
        )));
    }
    Ok(())
}

/// `jq -r --argjson x X --argjson y Y PROGRAM <<<"$W"` as `$(...)`.
fn jq_xy(prog: &str, w: &[u8], x: &[u8], y: &[u8]) -> jq::Ran {
    Args::new(&["-r"])
        .kv("--argjson", "x", x)
        .kv("--argjson", "y", y)
        .arg(prog)
        .run(Some(w), jq::Err::Inherit)
}

/// How win_point treats what is over the point: click and scroll raise the window (1) or not (0,
/// --no-raise); pointer never raises it, refuses a window over it and only says a layer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Raise {
    Yes,
    No,
    Pointer,
}

/// win_point RAISE X Y SEL...: the screen point "SX SY" for window coordinates X,Y of the window
/// SEL picks, raised first when it is not on screen or something covers the point (unless RAISE
/// says not); a point still covered is refused, naming what covers it. A layer surface over the
/// point (#263) is said, and refused under --no-raise; one spanning its output may or may not be
/// over it: said as such, never refused (findings 398, 421).
pub fn win_point(ctx: &Ctx, raise: Raise, x: &[u8], y: &[u8], sels: &[Vec<u8>]) -> Res<Vec<u8>> {
    // The layers read once: raising a window changes none of them.
    let lys = strip_nl(&hypr_json(ctx, &["layers"])?);
    let wins = box_windows(ctx, Some(&lys))?;
    let mut w = win_select(ctx, &wins, sels)?;
    let desc = win_desc(&w);
    let inside = Args::new(&["-e"])
        .kv("--argjson", "x", x)
        .kv("--argjson", "y", y)
        .arg(crate::jqprog::WIN_INSIDE_JQ)
        .run(Some(&w), jq::Err::Inherit);
    if !inside.ok() {
        return Err(ctx.die(cat!(
            x,
            ",",
            y,
            " is outside window ",
            desc,
            " (",
            jq_r(crate::jqprog::WIN_SIZE_JQ, &w),
            "; coordinates are the window's own)"
        )));
    }
    let mut why = jq_xy(crate::jqprog::WIN_BLOCKED_JQ, &w, x, y).text();
    if !why.is_empty() {
        if raise != Raise::Yes {
            return Err(ctx.die(cat!(
                "window ",
                desc,
                " is ",
                why,
                " (click --window raises it; --no-raise and pointer do not)"
            )));
        }
        win_raise(ctx, &w)?;
        let wins = box_windows(ctx, Some(&lys))?;
        w = win_select(ctx, &wins, &[jq_r(".address", &w)])?;
        why = jq_xy(crate::jqprog::WIN_BLOCKED_JQ, &w, x, y).text();
        if !why.is_empty() {
            return Err(ctx.die(cat!(
                "window ",
                desc,
                " is still ",
                why,
                " after focusing it: move or close that window first"
            )));
        }
    }
    let lay = jq_xy(crate::jqprog::WIN_LAYER_JQ, &w, x, y).text();
    if !lay.is_empty() {
        // certain|possible "NAMESPACE" LAYER WxH at X,Y: only the rectangle is known.
        let (how, lay) = match lay.iter().position(|&c| c == b' ') {
            Some(i) => (lay[..i].to_vec(), lay[i + 1..].to_vec()),
            None => (lay.clone(), lay.clone()),
        };
        let (first, rest) = match lay.iter().position(|&c| c == b' ') {
            Some(i) => (lay[..i].to_vec(), lay[i + 1..].to_vec()),
            None => (lay.clone(), lay.clone()),
        };
        let n = jq::jq(&["-r", "."], Some(&first), jq::Err::Null);
        let ns = if n.ok() { n.text() } else { Vec::new() };
        if how == b"possible" {
            let tail: &[u8] = if ns == b"omarchy-notifications" {
                b" (omabox run -- omarchy-shell notifications dismissAll clears the toasts)"
            } else {
                b""
            };
            say(cat!(
                "a layer spanning the screen, ",
                first,
                " (",
                rest,
                "), is up over window ",
                desc,
                ": input at ",
                x,
                ",",
                y,
                " may go to it if one of its items (a toast, a dock icon) is there; elsewhere it goes to the window",
                tail
            ));
        } else {
            let lay = cat!(
                "a layer surface ",
                first,
                " (",
                rest,
                ") is over ",
                x,
                ",",
                y,
                " of window ",
                desc,
                ": where it takes input (a notification, a panel), input there goes to it, not to the window"
            );
            if raise == Raise::No {
                let w = if ns.is_empty() {
                    Vec::new()
                } else {
                    cat!("; omabox wait layer ", ns, " --gone waits it out")
                };
                return Err(ctx.die(cat!(
                    lay,
                    " (refused under --no-raise; without it, this is a warning",
                    w,
                    ")"
                )));
            }
            let w = if ns.is_empty() {
                Vec::new()
            } else {
                cat!(" (omabox wait layer ", ns, " --gone waits it out)")
            };
            say(cat!(lay, w));
        }
    }
    Ok(jq_xy(crate::jqprog::WIN_AT_JQ, &w, x, y).text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jq_lead_taken_off() {
        assert_eq!(
            jq_error_text(b"jq: error (at <stdin>:1): x: y"),
            b"x: y".as_slice()
        );
        assert_eq!(jq_error_text(b"jq: errorless"), b"jq: errorless".as_slice());
        assert_eq!(jq_error_text(b"other"), b"other".as_slice());
    }
}
