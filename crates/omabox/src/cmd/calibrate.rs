//! `omabox calibrate [-b NAME] [--size WxH] [CARD | -o CARD]` and `omabox calibrate [CARD] --answer
//! "A X,Y ..." [--json]` (#188): a harness scales a big image down before its model sees it, and some
//! models give points in that smaller image (finding 251). The card is an image the size of a full
//! shot with marks at known points (kept in its PNG comment, which a model is not shown); the
//! readings say which --fit (or none) makes clicks land. Drawn with magick, no box needed: the box
//! only gives its size. The marks and the verdict are awk's, run as bash runs them.

use crate::args::{optarg, unknown_opt};
use crate::cat;
use crate::cmd::shot::on_path;
use crate::exit::{Ctx, Exit, Res, out, say};
use crate::jq::{self, Args};
use crate::jqprog::{CALIB_FIT_AWK, CALIB_MARKS_AWK, JQ_MON};
use crate::num::{awk_num, fixed};
use crate::shell::{self, dec, strip_nl, test_int};
use crate::shots::{dirname, mkdir_p, real, shot_fit_default, two_files};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::process::Stdio;

const CALIB_MARKS: &str = "6";

/// `$RANDOM$RANDOM`: two numbers of 0-32767, one after the other.
fn seed() -> String {
    let mut b = [0u8; 4];
    // SAFETY: b is ours, its length given.
    let got = unsafe { libc::getrandom(b.as_mut_ptr().cast(), b.len(), 0) };
    if got != 4 {
        let t = crate::settle::now_ms();
        b = (t as u32).to_le_bytes();
    }
    let r1 = u16::from_le_bytes([b[0], b[1]]) & 0x7fff;
    let r2 = u16::from_le_bytes([b[2], b[3]]) & 0x7fff;
    format!("{r1}{r2}")
}

/// awk -v NAME=VALUE... PROGRAM: its stdout, and whether it ended 0.
fn awk(vars: &[(&str, &[u8])], prog: &str) -> (Vec<u8>, bool) {
    let mut c = crate::sig::command("awk");
    for (k, v) in vars {
        c.arg("-v").arg(OsStr::from_bytes(&cat!(k, "=", v)));
    }
    c.arg(prog).stdin(Stdio::null()).stderr(Stdio::inherit());
    match c.output() {
        Ok(o) => (o.stdout, o.status.success()),
        Err(_) => (Vec::new(), false),
    }
}

/// calib_marks W H: "L X Y" a line, A-F, sorted.
fn calib_marks(w: &[u8], h: &[u8]) -> Vec<u8> {
    let s = seed();
    let (o, _) = awk(
        &[
            ("w", w),
            ("h", h),
            ("n", CALIB_MARKS.as_bytes()),
            ("seed", s.as_bytes()),
        ],
        CALIB_MARKS_AWK,
    );
    let mut lines: Vec<&[u8]> = o.split(|&c| c == b'\n').filter(|l| !l.is_empty()).collect();
    lines.sort();
    let mut v = lines.join(&b'\n');
    if !v.is_empty() {
        v.push(b'\n');
    }
    v
}

/// calib_card FILE W H MARKS: each mark a red cross in a ring, its letter above it to the right, on
/// light grey; sized from the card's longest side. MARKS in the PNG's comment. None where a number is
/// none (dec).
fn calib_card(file: &[u8], w: &[u8], h: &[u8], marks: &[u8]) -> Option<bool> {
    let (wi, hi) = (dec(w)?, dec(h)?);
    let long = wi.max(hi);
    let arm = (long.saturating_mul(18) / 1920).max(6);
    let sw = (long.saturating_mul(3) / 1920).max(2);
    let r = (long.saturating_mul(11) / 1920).max(4);
    let pt = (long.saturating_mul(34) / 1920).max(12);
    let mut comment = cat!("omabox-calibrate 1 ", w, " ", h);
    let (mut draw, mut text): (Vec<Vec<u8>>, Vec<Vec<u8>>) = (Vec::new(), Vec::new());
    let m = strip_nl(marks);
    for line in m.split(|&c| c == b'\n') {
        let f = shell::read_fields(line, 3);
        let (l, xs, ys) = (&f[0], &f[1], &f[2]);
        let (x, y) = (dec(xs)?, dec(ys)?);
        draw.extend([
            b"-draw".to_vec(),
            format!("line {},{y} {},{y}", x - arm, x + arm).into_bytes(),
            b"-draw".to_vec(),
            format!("line {x},{} {x},{}", y - arm, y + arm).into_bytes(),
            b"-draw".to_vec(),
            format!("circle {x},{y} {},{y}", x + r).into_bytes(),
        ]);
        text.extend([
            b"-draw".to_vec(),
            cat!(
                format!("text {},{} '", x + r + sw * 2, y - r - sw * 2),
                l,
                "'"
            ),
        ]);
        comment = cat!(comment, " ", l, " ", xs, " ", ys);
    }
    // A bold sans the system has (a path: magick's own font names vary); magick's default without one.
    let font = crate::sig::command("fc-match")
        .args(["-f", "%{file}", "sans:bold"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| strip_nl(&o.stdout))
        .filter(|f| shell::path(f).is_file())
        .unwrap_or_default();
    let mut c = crate::sig::command("magick");
    c.arg("-size")
        .arg(OsStr::from_bytes(&cat!(w, "x", h)))
        .arg("xc:#f2f2f2")
        .args(["-fill", "none", "-stroke", "#d4141e", "-strokewidth"])
        .arg(sw.to_string())
        .args(draw.iter().map(|a| OsStr::from_bytes(a)))
        .args(["-stroke", "none", "-fill", "#111111"]);
    if !font.is_empty() {
        c.arg("-font").arg(OsStr::from_bytes(&font));
    }
    c.arg("-pointsize")
        .arg(pt.to_string())
        .args(text.iter().map(|a| OsStr::from_bytes(a)))
        .arg("-set")
        .arg("comment")
        .arg(OsStr::from_bytes(&comment))
        .args(["-depth", "8"])
        .arg(OsStr::from_bytes(&cat!("png24:", file)));
    Some(c.status().is_ok_and(|s| s.success()))
}

/// calib_read FILE: the card's size ("W H") and its marks ("L X Y" lines); None for a file that is
/// not a card.
fn calib_read(file: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let o = crate::sig::command("magick")
        .args(["identify", "-format", "%c", "--"])
        .arg(OsStr::from_bytes(file))
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    let c = shell::subst(&o.stdout);
    let re =
        shell::Re::new(r"^omabox-calibrate\ 1\ ([0-9]+\ [0-9]+)((\ [A-Z]\ [0-9]+\ [0-9]+)+)$")?;
    let size = re.group(&c, 1)?;
    let rest = re.group(&c, 2)?;
    let rest = rest.strip_prefix(b" ").unwrap_or(&rest).to_vec();
    // (sed -E 's/ ([A-Z] )/\n\1/g': a newline before each letter but the first)
    let letter = shell::Re::new("^[A-Z]$")?;
    let words: Vec<&[u8]> = rest.split(|&c| c == b' ').collect();
    let mut marks = Vec::new();
    for (k, w) in words.iter().enumerate() {
        if k > 0 {
            let sep = if letter.is_match(w) && k + 1 < words.len() {
                b'\n'
            } else {
                b' '
            };
            marks.push(sep);
        }
        marks.extend_from_slice(w);
    }
    Some((size, marks))
}

/// calib_answer TEXT: the readings, "L X Y" a line in TEXT's order (a letter, then its two numbers,
/// as "A 123,456 B 789,10" or "A: (123, 456); b=789 10", decimals too); None, said, when it is not.
fn calib_answer(t: &[u8]) -> Option<Vec<Vec<u8>>> {
    // (tr 'a-z:,;()=[]' 'A-Z         ', then the first line's words)
    let up: Vec<u8> = t
        .iter()
        .map(|&c| match c {
            b'a'..=b'z' => c - 32,
            b':' | b',' | b';' | b'(' | b')' | b'=' | b'[' | b']' => b' ',
            c => c,
        })
        .collect();
    let up = strip_nl(&up);
    let first = up.split(|&c| c == b'\n').next().unwrap_or_default();
    let w = shell::words(first);
    if w.is_empty() || w.len() % 3 != 0 {
        say(cat!(
            "calibrate --answer: a letter and its point for each mark, like \"A 123,456 B 789,10\", got: ",
            t
        ));
        return None;
    }
    let mut seen: Vec<Vec<u8>> = Vec::new();
    let mut out = Vec::new();
    for k in (0..w.len()).step_by(3) {
        let (l, x, y) = (&w[k], &w[k + 1], &w[k + 2]);
        let num = r"^[0-9]+(\.[0-9]+)?$";
        if !(shell::re_match("^[A-Z]$", l) && shell::re_match(num, x) && shell::re_match(num, y)) {
            say(cat!(
                "calibrate --answer: '",
                w[k..k + 3].join(&b' '),
                "' is not a letter and its point X,Y (like A 123,456)"
            ));
            return None;
        }
        if seen.contains(l) {
            say(cat!("calibrate --answer: ", l, " twice"));
            return None;
        }
        seen.push(l.clone());
        out.push(cat!(l, " ", x, " ", y));
    }
    Some(out)
}

/// The first word of each line.
fn letters(lines: &[Vec<u8>]) -> Vec<Vec<u8>> {
    lines
        .iter()
        .map(|l| shell::words(l).into_iter().next().unwrap_or_default())
        .collect()
}

/// calib_verdict CARD ANSWER JSON: 1:1, the --fit to use, or that no scale fits. Exit 0, 1 (no scale
/// fits), 2 (no card, or an answer it cannot read).
fn calib_verdict(card: &[u8], answer: &[u8], json: bool) -> Res<()> {
    let Some((size, marks)) = calib_read(card) else {
        say(cat!(
            "calibrate: no card at ",
            card,
            " (omabox calibrate draws one first)"
        ));
        return Err(Exit(2));
    };
    let wh = shell::read_fields(&size, 2);
    let (w, h) = (wh[0].clone(), wh[1].clone());
    let Some(reads) = calib_answer(answer) else {
        return Err(Exit(2));
    };
    let mlines: Vec<Vec<u8>> = marks.split(|&c| c == b'\n').map(<[u8]>::to_vec).collect();
    let (ml, rl) = (letters(&mlines), letters(&reads));
    let unknown: Vec<u8> = rl
        .iter()
        .filter(|l| !ml.contains(l))
        .flat_map(|l| cat!(" ", l))
        .collect();
    if !unknown.is_empty() {
        let its: Vec<u8> = mlines
            .iter()
            .map(|l| shell::cut_line(l, 1).to_vec())
            .collect::<Vec<_>>()
            .join(&b' ');
        say(cat!(
            "calibrate --answer: the card has no mark",
            unknown,
            " (its marks: ",
            its,
            ")"
        ));
        return Err(Exit(2));
    }
    let n = reads.len();
    if n < 3 {
        say(format!("calibrate --answer: at least 3 marks, got {n}"));
        return Err(Exit(2));
    }
    let missing: Vec<u8> = ml
        .iter()
        .filter(|l| !rl.contains(l))
        .flat_map(|l| cat!(" ", l))
        .collect();
    if !missing.is_empty() {
        say(cat!(
            "calibrate: not given:",
            missing,
            format!(" (the verdict is from the other {n})")
        ));
    }
    let (o, _) = awk(
        &[
            ("w", &w),
            ("h", &h),
            ("marks", &marks),
            ("reads", &reads.join(&b'\n')),
        ],
        CALIB_FIT_AWK,
    );
    let mut res = strip_nl(&o);
    if res.is_empty() {
        res = b"none".to_vec();
    }
    let cur = crate::exit::muted(|| shot_fit_default().unwrap_or_default());
    let vd = shell::words(&res);
    let v = |i: usize| vd.get(i).cloned().unwrap_or_default();
    let kind = v(0);
    if json {
        let r = Args::new(&["-nc"])
            .kv("--arg", "r", &res)
            .kv("--arg", "w", &w)
            .kv("--arg", "h", &h)
            .kv("--arg", "cur", &cur)
            .kv("--argjson", "n", n.to_string())
            .arg(crate::jqprog::CALIB_JSON_JQ)
            .run(None, jq::Err::Inherit);
        out(&r.out)?;
        return match kind.as_slice() {
            b"one" | b"fit" => Ok(()),
            _ => Err(Exit(1)),
        };
    }
    let mut t = Vec::new();
    match kind.as_slice() {
        b"one" => {
            t.extend(cat!(
                "read 1:1: every mark within ",
                v(1),
                " px of where it is on this ",
                w,
                "x",
                h,
                " card. Full-size shots and click X Y land; no --fit needed.\n"
            ));
            if !cur.is_empty() {
                t.extend(cat!(
                    "(shot-fit is ",
                    cur,
                    " here: harmless, click --in maps those shots.)\n"
                ));
            }
            out(t)
        }
        b"fit" => {
            let s = awk_num(&v(1));
            let seen = format!(
                "{}x{}",
                fixed((awk_num(&w) * s + 0.5).trunc(), 0),
                fixed((awk_num(&h) * s + 0.5).trunc(), 0)
            );
            t.extend(cat!(
                "read at ",
                v(1),
                "x the card (x ",
                v(2),
                ", y ",
                v(3),
                "): this ",
                w,
                "x",
                h,
                " image reaches you at about ",
                seen,
                ", and points are given in that smaller image, so click X Y off a full-size shot lands short of the target.\n"
            ));
            t.extend(cat!(
                "--fit ",
                v(4),
                " (",
                v(5),
                ", about the size you see) maps these readings back to within ",
                v(6),
                " px (largest: ",
                v(7),
                "). Use shots of that size, with click --in:\n"
            ));
            t.extend(cat!(
                "  omabox shot --fit ",
                v(4),
                "   then   omabox click --in SHOT X Y\n"
            ));
            if cur == v(4) {
                t.extend(cat!(
                    "shot-fit is ",
                    v(4),
                    " here already: every shot comes out at that size.\n"
                ));
            } else {
                t.extend(cat!(
                    "For every shot, the user can run: omabox config shot-fit ",
                    v(4),
                    "   (their setting: suggest it, never set it)\n"
                ));
            }
            t.extend(cat!(
                "Check: omabox calibrate --size ",
                v(5),
                " should read 1:1.\n"
            ));
            out(t)
        }
        b"up" => {
            out(cat!(
                "read at ",
                v(1),
                "x the card: points come out past where the marks are, which no --fit undoes. Draw a new card (omabox calibrate) and read it again; if it reads the same, points read off shots are not reliable here.\n"
            ))?;
            Err(Exit(1))
        }
        _ => {
            // (cut -s -d' ' -f4-: the fields from the fourth, of a line with a space)
            let off: Vec<u8> = if res.contains(&b' ') {
                res.split(|&c| c == b' ')
                    .skip(3)
                    .collect::<Vec<_>>()
                    .join(&b' ')
            } else {
                Vec::new()
            };
            if kind == b"off" && !off.is_empty() && v(1) != b"0" {
                let fits = (n as i64) - (shell::words(&off).len() as i64);
                let others = if fits == 0 {
                    String::new()
                } else {
                    format!(" (the other {fits} fit it)")
                };
                let mut each = Vec::new();
                let re = shell::Re::new("^([A-Z]):([0-9]+)");
                let mut k = 0;
                // (sed -E 's/([A-Z]):([0-9]+)/\1 is \2 px off,/g; s/,$//')
                while k < off.len() {
                    if let Some(m) = re.as_ref().and_then(|r| r.captures(&off[k..], false)) {
                        if let (Some((_, e)), Some((a1, b1)), Some((a2, b2))) = (m[0], m[1], m[2]) {
                            each.extend(cat!(
                                &off[k + a1..k + b1],
                                " is ",
                                &off[k + a2..k + b2],
                                " px off,"
                            ));
                            k += e;
                            continue;
                        }
                    }
                    each.push(off[k]);
                    k += 1;
                }
                if each.last() == Some(&b',') {
                    each.pop();
                }
                t.extend(cat!(
                    "no single scale fits these readings to within ",
                    v(2),
                    " px: at the best one, ",
                    v(1),
                    "x",
                    others,
                    ", ",
                    each,
                    ".\n"
                ));
            } else {
                t.extend(b"no single scale fits these readings.\n");
            }
            t.extend(b"A mark read wrong, or two letters swapped? Draw a new card (omabox calibrate) and read it again, each cross's centre. If it still does not fit, points read off shots are not reliable here: zoom in (shot -g, --zoom) or click --window by what windows says.\n");
            out(t)?;
            Err(Exit(1))
        }
    }
}

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    let (mut name, mut card, mut size, mut answer) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut given, mut json) = (false, false);
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let v = args.get(i + 1);
        match a.as_slice() {
            b"-b" | b"--box" => {
                optarg(ctx, a, v)?;
                name = args[i + 1].clone();
                i += 1;
            }
            b"--json" => json = true,
            b"-o" | b"--output" => {
                optarg(ctx, a, v)?;
                two_files(ctx, "calibrate", &card, &args[i + 1])?;
                card = args[i + 1].clone();
                i += 1;
            }
            b"--size" => {
                optarg(ctx, a, v)?;
                size = args[i + 1].clone();
                i += 1;
            }
            b"--answer" => {
                optarg(ctx, a, v)?;
                answer = args[i + 1].clone();
                given = true;
                i += 1;
            }
            w if w.starts_with(b"-") => return Err(unknown_opt(ctx, "calibrate", a)),
            w => {
                two_files(ctx, "calibrate", &card, w)?;
                card = w.to_vec();
            }
        }
        i += 1;
    }
    if !on_path("magick") {
        ctx.parsed();
        return Err(
            ctx.die("calibrate needs ImageMagick's magick (package imagemagick, part of Omarchy)")
        );
    }
    // One card a session (or repo), where --answer looks for it without being told.
    let tmp = shell::env("TMPDIR")
        .filter(|t| !t.is_empty())
        .unwrap_or(b"/tmp".to_vec());
    let dflt = cat!(
        tmp,
        "/omabox-calibrate-",
        crate::select::tame(&crate::select::default_name()),
        ".png"
    );
    if card.is_empty() {
        card = dflt.clone();
    }
    if given {
        if !size.is_empty() {
            return Err(ctx.die("calibrate --answer: about a card already drawn: no --size"));
        }
        ctx.parsed();
        return calib_verdict(&card, &answer, json);
    }
    if json {
        return Err(ctx.die(
            "calibrate: --json goes with --answer (the verdict); a card's path is its one line",
        ));
    }
    let (w, h, of);
    if !size.is_empty() {
        let re = shell::Re::new("^([0-9]{2,5})x([0-9]{2,5})$");
        let g = re
            .as_ref()
            .and_then(|r| Some((r.group(&size, 1)?, r.group(&size, 2)?)));
        let ok = g.as_ref().is_some_and(|(a, b)| {
            let (a, b) = (test_int(a).unwrap_or(0), test_int(b).unwrap_or(0));
            a >= 64 && b >= 64 && a <= 16384 && b <= 16384
        });
        let Some((gw, gh)) = g.filter(|_| ok) else {
            return Err(ctx.die(cat!("calibrate: --size is WxH, 64-16384 each, got ", size)));
        };
        (w, h, of) = (gw, gh, Vec::new());
    } else {
        // A full shot's size: the layout times the highest scale (finding 299), as `shot` takes it.
        // Asked through omabox itself, so a jailed agent's goes by its broker.
        let mut c = crate::sig::omabox_cmd();
        if !name.is_empty() {
            c.arg("-b").arg(OsStr::from_bytes(&name));
        }
        c.args(["hyprctl", "-j", "monitors"])
            .stdin(Stdio::inherit())
            .stderr(Stdio::null());
        let mons = c
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| shell::subst(&o.stdout));
        let got = mons.and_then(|m| {
            let r = jq::jq(
                &["-er", &format!("{JQ_MON}{}", crate::jqprog::CALIB_SIZE_JQ)],
                Some(&m),
                jq::Err::Null,
            );
            r.ok().then(|| r.text())
        });
        let s = match got {
            Some(s) => {
                of = b"the size of a full shot of the box".to_vec();
                s
            }
            None => {
                let named = if name.is_empty() {
                    Vec::new()
                } else {
                    cat!(" named ", name)
                };
                of = cat!(
                    "no box up",
                    named,
                    ": a common screen's size; --size WxH for another"
                );
                b"1920x1080".to_vec()
            }
        };
        let xi = s.iter().rposition(|&c| c == b'x');
        w = xi.map_or(s.clone(), |i| s[..i].to_vec());
        h = s
            .iter()
            .position(|&c| c == b'x')
            .map_or(s.clone(), |i| s[i + 1..].to_vec());
    }
    ctx.parsed();
    let dir = dirname(&card);
    if !mkdir_p(&dir) {
        return Err(ctx.die(cat!("calibrate: cannot make the directory ", dir)));
    }
    let card = crate::shots::real_or_end(ctx, &card)?;
    let part = cat!(card, ".part");
    let marks = calib_marks(&w, &h);
    let drawn = calib_card(&part, &w, &h, &marks).unwrap_or(false);
    if !drawn || !crate::shots::mv_f(&part, &card) {
        let _ = std::fs::remove_file(shell::path(&part));
        return Err(ctx.die(cat!("calibrate: could not draw the card at ", card)));
    }
    out(cat!(card, "\n"))?;
    let mut cmd = b"omabox calibrate".to_vec();
    if card != real(&dflt) {
        cmd = cat!(cmd, " ", shell::quote_q(&card));
    }
    let of: &[u8] = if of.is_empty() {
        b"as --size asked"
    } else {
        &of
    };
    say(cat!(
        "a ",
        w,
        "x",
        h,
        " test card (",
        of,
        "): ",
        CALIB_MARKS,
        " marks, A to F, each a red cross in a ring, its letter at its upper right."
    ));
    say(
        "Look at it as you look at a shot, and give where each cross's centre is, in the image's pixels, as you would to click it:",
    );
    say(cat!(
        "  ",
        cmd,
        " --answer \"A X,Y B X,Y C X,Y D X,Y E X,Y F X,Y\""
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_as_bash_reads_them() {
        assert_eq!(
            calib_answer(b"a: (1, 2); b=3 4; C 5.5,6"),
            Some(vec![
                b"A 1 2".to_vec(),
                b"B 3 4".to_vec(),
                b"C 5.5 6".to_vec()
            ])
        );
        // (tr, then the first line only)
        assert_eq!(calib_answer(b"A 1,2\nB 3,4"), Some(vec![b"A 1 2".to_vec()]));
        assert_eq!(calib_answer(b"A 1"), None);
        assert_eq!(calib_answer(b"A 1,2 A 3,4 B 5,6"), None);
        assert_eq!(calib_answer(b"AB 1,2 B 3,4 C 5,6"), None);
        assert_eq!(
            letters(&[b"A 1 2".to_vec(), b"B 3 4".to_vec()]),
            vec![b"A".to_vec(), b"B".to_vec()]
        );
    }
}
