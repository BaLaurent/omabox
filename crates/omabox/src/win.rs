//! The box's windows as bin/omabox reads them (finding 81): Hyprland's lists annotated by WIN_JQ
//! (box_windows, win_annotate), the one window a selector picks (win_select, #20, #37, #244), and
//! their words in messages (win_desc, win_lines). jq runs the programs, so the answers are jq's.

use crate::cat;
use crate::exit::{Ctx, Exit, Res, say};
use crate::hypr::hypr_json;
use crate::jq::{self, Args};
use crate::jqprog::{WIN_JQ, WIN_SEL_JQ};
use crate::shell::strip_nl;
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
        &[
            "-r",
            r#"if length == 0 then "  (none)" else .[] | "  \(.address)  \(.class)  \(.title | tojson)  workspace \(.workspace)" end"#,
        ],
        Some(list),
        jq::Err::Inherit,
    );
    let _ = std::io::stderr().write_all(&r.out);
}

/// win_desc WINDOW: `0x... class "title"`, for messages.
pub fn win_desc(w: &[u8]) -> Vec<u8> {
    jq::jq(
        &["-r", r#""\(.address) \(.class) \(.title | tojson)""#],
        Some(w),
        jq::Err::Inherit,
    )
    .text()
}

/// `jq -r PROGRAM <<<"$X"` as `$(...)`.
pub fn jq_r(prog: &str, x: &[u8]) -> Vec<u8> {
    jq::jq(&["-r", prog], Some(x), jq::Err::Inherit).text()
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
