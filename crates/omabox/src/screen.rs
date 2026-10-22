//! The box's screen as bin/omabox reads it: the layout's size (extent), the main monitor and its mode
//! (main_screen, box_screen: findings 70, 77, #122, #163), a window's capture scale and a region of it
//! in its buffer's pixels (win_scale, win_buf_geom: #227, findings 299, 300), and a recorded shot's
//! pixel back on the box (shot_point, for --in).

use crate::boxes::meta;
use crate::cat;
use crate::exit::{Ctx, Exit, Res};
use crate::hypr::hypr_json;
use crate::jail::in_jail;
use crate::jq::{self, Args};
use crate::jqprog::JQ_MON;
use crate::num::awk_num;
use crate::shell::{self, bash_int, strip_nl};
use crate::win::{box_windows, jq_r, win_desc};
use std::os::unix::fs::MetadataExt;

/// extent: the layout's size, "WxH", or the command ends saying it cannot be read.
pub fn extent(ctx: &Ctx) -> Res<Vec<u8>> {
    let m = strip_nl(&hypr_json(ctx, &["monitors"])?);
    let r = jq::jq(
        &[
            "-er",
            &format!(
                "{JQ_MON}{}",
                r#"[.[] | select(.disabled | not)] |
    "\(map(.x + lw) | max | floor)x\(map(.y + lh) | max | floor)""#
            ),
        ],
        Some(&m),
        jq::Err::Null,
    );
    if !r.ok() {
        return Err(ctx.die(cat!(
            "cannot read the screen size of box '",
            ctx.name,
            "' (it lists no monitor)"
        )));
    }
    Ok(r.text())
}

/// main_screen: the box's main monitor in `hyprctl -j monitors`, as jq: a headless box's HEADLESS-2
/// (WAYLAND-1 on NVIDIA), an interactive box's first window; never one of `monitor add`'s.
pub fn main_screen(ctx: &Ctx) -> &'static str {
    if meta(&ctx.d, "mode").0 == b"interactive" {
        r#"map(select((.disabled | not) and (.name | startswith("WAYLAND-")))) | .[0]"#
    } else {
        r#"map(select((.disabled | not) and (.name == "HEADLESS-2" or .name == "WAYLAND-1"))) | .[0]"#
    }
}

/// box_screen: the main screen's output name and mode ("NAME WxH@HZ").
pub fn box_screen(ctx: &Ctx) -> Res<Vec<u8>> {
    let m = strip_nl(&hypr_json(ctx, &["monitors"])?);
    let prog = format!(
        "{}{}",
        main_screen(ctx),
        r#" // empty | "\(.name) \(.width)x\(.height)@\(.refreshRate * 100 | round / 100)""#
    );
    let r = jq::jq(&["-r", &prog], Some(&m), jq::Err::Inherit);
    if !r.ok() {
        return Err(Exit(u8::try_from(r.rc).unwrap_or(1)));
    }
    Ok(r.text())
}

/// win_scale ADDRESS: the scale of a window's capture (grim -T draws it at its monitor's, #227,
/// finding 299); 1 when it cannot be read.
pub fn win_scale(ctx: &Ctx, addr: &[u8]) -> Res<Vec<u8>> {
    let cl = strip_nl(&hypr_json(ctx, &["clients"])?);
    let mons = strip_nl(&hypr_json(ctx, &["monitors"])?);
    let r = Args::new(&["-r"])
        .kv("--arg", "a", addr)
        .kv("--argjson", "m", &mons)
        .arg("first(.[] | select(.address == $a) | .monitor) as $id | first($m[] | select(.id == $id) | .scale) // 1")
        .run(Some(&cl), jq::Err::Null);
    let sc = if r.ok() { r.text() } else { b"1".to_vec() };
    Ok(if shell::re_match(r"^[0-9]+(\.[0-9]+)?$", &sc) {
        sc
    } else {
        b"1".to_vec()
    })
}

/// win_buf_geom X Y W H SCALE WINDOW_W WINDOW_H: a region of a window (its own coordinates) as grim
/// -T's -g takes it, in the buffer's pixels (finding 300): each edge at SCALE rounded as the buffer's
/// size is, inside the buffer. awk's arithmetic.
pub fn win_buf_geom(v: [&[u8]; 7]) -> Vec<u8> {
    let [x, y, w, h, s, ww, wh] = v.map(awk_num);
    let r = |v: f64| (v * s + 0.5).trunc();
    let (x0, y0) = (r(x), r(y));
    let (mut x1, mut y1) = (r(x + w), r(y + h));
    if x1 > r(ww) {
        x1 = r(ww);
    }
    if y1 > r(wh) {
        y1 = r(wh);
    }
    if x1 <= x0 {
        x1 = x0 + 1.0;
    }
    if y1 <= y0 {
        y1 = y0 + 1.0;
    }
    format!(
        "{},{} {}x{}",
        x0 as i64,
        y0 as i64,
        (x1 - x0) as i64,
        (y1 - y0) as i64
    )
    .into_bytes()
}

/// img_px P IMAGE SCREEN: the screen pixel under the centre of image pixel P (bash's arithmetic).
fn img_px(p: &[u8], image: &[u8], screen: &[u8]) -> Option<i64> {
    let (p, i, s) = (bash_int(p)?, bash_int(image)?, bash_int(screen)?);
    let d = 2i64.wrapping_mul(i);
    if d == 0 {
        return None;
    }
    Some(
        2i64.wrapping_mul(p)
            .wrapping_add(1)
            .wrapping_mul(s)
            .wrapping_div(d),
    )
}

/// `stat -L -c '%s %.9Y' -- FILE`.
fn stamp(f: &[u8]) -> Vec<u8> {
    match std::fs::metadata(shell::path(f)) {
        Ok(m) => format!("{} {}.{:09}", m.size(), m.mtime(), m.mtime_nsec()).into_bytes(),
        Err(_) => Vec::new(),
    }
}

/// Where a shot's pixel is on the box.
pub enum Point {
    Window(Vec<u8>, i64, i64),
    Screen(i64, i64),
}

/// shot_point FILE X Y: image pixel X,Y of a recorded shot (shots.tsv) as the window's own
/// coordinates or the screen's. Refused: a file that is not a shot of this box, changed since, or
/// taken in another screen mode; a window gone or resized.
pub fn shot_point(ctx: &Ctx, file: &[u8], x: &[u8], y: &[u8]) -> Res<Point> {
    let (f, key) = if in_jail() {
        // A jailed caller's omabox opened the shot in the jail and passed it as /dev/fd/N, with its
        // name there (relay_in): the broker opens no path of the caller's.
        let as_ = shell::env_or_empty("OMABOX_RELAY_IN_AS");
        if !(shell::re_match("^/dev/fd/[3-9]$", file)
            && shell::path(file).is_file()
            && !as_.is_empty())
        {
            let mut c = ctx.clone();
            c.parsed();
            return Err(c.die(
                "--in: a jailed agent's shot is read by its own omabox, never at a path here",
            ));
        }
        (file.to_vec(), as_)
    } else {
        match crate::repo::readlink_f(file).filter(|f| shell::path(f).is_file()) {
            Some(f) => (f.clone(), f),
            None => return Err(ctx.die(cat!("--in: no such file: ", file))),
        }
    };
    // (awk's last line whose first tab-separated field is KEY)
    let tsv = std::fs::read(ctx.at("shots.tsv")).unwrap_or_default();
    let rec = tsv
        .split(|&c| c == b'\n')
        .rfind(|l| l.split(|&c| c == b'\t').next() == Some(key.as_slice()) && !l.is_empty())
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    if rec.is_empty() {
        return Err(ctx.die(cat!(
            "--in: ",
            file,
            " is not a shot of box '",
            ctx.name,
            "' (take one with omabox shot)"
        )));
    }
    let r = shell::read_ifs(&rec, 5, b"\t");
    let (stp, mode, kind, geo) = (&r[1], &r[2], &r[3], &r[4]);
    let (kind, ref_) = match kind.iter().position(|&c| c == b' ') {
        Some(i) => (kind[..i].to_vec(), kind[i + 1..].to_vec()),
        None => (kind.clone(), kind.clone()),
    };
    if stamp(&f) != *stp {
        return Err(ctx.die(cat!(
            "--in: ",
            key,
            " changed since the shot: take a new one"
        )));
    }
    let screen = box_screen(ctx)?;
    let smode = match screen.iter().position(|&c| c == b' ') {
        Some(i) => screen[i + 1..].to_vec(),
        None => screen.clone(),
    };
    if smode != *mode {
        return Err(ctx.die(cat!(
            "--in: the box's mode is ",
            smode,
            ", the shot was taken at ",
            mode,
            ": take a new one"
        )));
    }
    let g = shell::read_ifs(geo, 4, b"x ");
    let (lw, lh, iw, ih) = (&g[0], &g[1], &g[2], &g[3]);
    let lt = |a: &[u8], b: &[u8]| {
        shell::test_int(a)
            .zip(shell::test_int(b))
            .is_some_and(|(a, b)| a < b)
    };
    if !(lt(x, iw) && lt(y, ih)) {
        return Err(ctx.die(cat!(
            "--in: ",
            x,
            ",",
            y,
            " is outside the image (",
            iw,
            "x",
            ih,
            ")"
        )));
    }
    let (Some(rx), Some(ry)) = (img_px(x, iw, lw), img_px(y, ih, lh)) else {
        return Err(Exit(1)); // (bash's arithmetic refused it: its own words, then exit 1)
    };
    let half = |v: &[u8], first: bool| -> Option<i64> {
        // ${V%,*} and ${V#*,}
        let p = if first {
            &v[..v.iter().rposition(|&c| c == b',').unwrap_or(v.len())]
        } else {
            &v[v.iter().position(|&c| c == b',').map_or(0, |i| i + 1)..]
        };
        bash_int(p)
    };
    if kind == b"window" {
        // "ADDRESS", or "ADDRESS X,Y WxH" for a -g crop of it: where the crop starts, the window's
        // size.
        let a = shell::read_fields(&ref_, 3);
        let (addr, mut org, mut wsz) = (a[0].clone(), a[1].clone(), a[2].clone());
        if org.is_empty() {
            org = b"0,0".to_vec();
            wsz = cat!(lw, "x", lh);
        }
        let wins = box_windows(ctx, None)?;
        let w = Args::new(&["-c"])
            .kv("--arg", "a", &addr)
            .arg("first(.[] | select(.address == $a)) // empty")
            .run(Some(&wins), jq::Err::Inherit)
            .text();
        if w.is_empty() {
            return Err(ctx.die(cat!("--in: the window of that shot (", addr, ") is gone")));
        }
        let size = jq_r(r#""\(.size[0])x\(.size[1])""#, &w);
        if size != wsz {
            return Err(ctx.die(cat!(
                "--in: window ",
                win_desc(&w),
                " is ",
                size,
                " now, ",
                wsz,
                " in the shot: take a new one"
            )));
        }
        let (Some(ox), Some(oy)) = (half(&org, true), half(&org, false)) else {
            return Err(Exit(1));
        };
        return Ok(Point::Window(
            addr,
            ox.wrapping_add(rx),
            oy.wrapping_add(ry),
        ));
    }
    let (Some(ox), Some(oy)) = (half(&ref_, true), half(&ref_, false)) else {
        return Err(Exit(1));
    };
    Ok(Point::Screen(ox.wrapping_add(rx), oy.wrapping_add(ry)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // What bash's awk wrote for each.
    #[test]
    fn buffer_regions() {
        let g = |v: [&str; 7]| String::from_utf8(win_buf_geom(v.map(str::as_bytes))).unwrap();
        assert_eq!(g(["10", "20", "5", "5", "1", "100", "100"]), "10,20 5x5");
        assert_eq!(g(["10", "20", "5", "5", "1.5", "100", "100"]), "15,30 8x8");
        assert_eq!(g(["98", "0", "5", "1", "2", "100", "100"]), "196,0 4x2");
        assert_eq!(g(["100", "0", "5", "1", "1", "100", "100"]), "100,0 1x1");
        assert_eq!(img_px(b"3", b"100", b"200"), Some(7));
        assert_eq!(img_px(b"3", b"0", b"200"), None);
    }
}
