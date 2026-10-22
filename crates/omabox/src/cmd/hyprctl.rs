//! `omabox hyprctl [-b NAME] ARGS...`: hyprctl against the box's Hyprland (on_box). Its answer is
//! held until it is known not to be hyprctl's own "didn't respond" (#166): a box gone, or one not
//! answering a ping, is said in words; a failure of a box that answers is hyprctl's, as it came.

use crate::args::optarg;
use crate::boxes::{box_alive, need_box};
use crate::exec::{ErrTo, Out, on_box};
use crate::exit::{Ctx, Exit, Res, out};
use crate::hypr::{HYPR_LIMIT, hung_text, hypr_answers, hypr_down_text, hypr_errors_note};
use crate::select::select_box;
use crate::shell;
use std::io::{Read, Seek};
use std::os::fd::OwnedFd;

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    let mut name = Vec::new();
    let mut i = 0;
    while matches!(args.get(i).map(Vec::as_slice), Some(b"-b" | b"--box")) {
        optarg(ctx, &args[i], args.get(i + 1))?;
        name = args[i + 1].clone();
        i += 2;
    }
    let rest = &args[i..];
    select_box(ctx, &name)?;
    need_box(ctx)?;
    // Not for what runs until the caller ends it: rollinglog -f, a repl with no code.
    let joined = crate::cat!(" ", rest.join(&b' '), " ");
    let mut follow = rest.iter().any(|a| a == b"-f" || a == b"--follow")
        && crate::exit::contains(&joined, b" rollinglog ");
    if rest.last().is_some_and(|a| a == b"repl") {
        follow = true;
    }
    let mut argv = vec![b"hyprctl".to_vec()];
    if follow {
        argv.extend(rest.iter().cloned());
        let r = on_box(ctx, &argv, false, Out::Inherit, ErrTo::Inherit, true).or_exit()?;
        return code(r.rc);
    }
    // Held in an unlinked file, byte for byte, printed once it is not hyprctl's own error.
    let Some(mut f) = temp_file() else {
        return Err(ctx.die("hyprctl: no temporary file"));
    };
    let w = f
        .try_clone()
        .map(OwnedFd::from)
        .map_err(|_| ctx.die("hyprctl: no temporary file"))?;
    let mut argv = vec![
        b"timeout".to_vec(),
        HYPR_LIMIT.as_bytes().to_vec(),
        b"hyprctl".to_vec(),
    ];
    argv.extend(rest.iter().cloned());
    let r = on_box(ctx, &argv, false, Out::Fd(w), ErrTo::Inherit, true).or_exit()?;
    ctx.box_gone_said |= r.gone_said;
    let mut rc = r.rc;
    if rc != 0 {
        if !box_alive(ctx)? {
            if ctx.box_gone_said {
                return Err(Exit(1));
            }
            return Err(ctx.die(hypr_down_text(ctx)));
        }
        if rc == 124 {
            let t = hung_text(ctx, &format!("{HYPR_LIMIT} s"))?;
            return Err(ctx.die(t));
        }
        if !hypr_answers(ctx)? {
            let t = hung_text(ctx, "")?;
            return Err(ctx.die(t));
        }
    }
    let mut held = Vec::new();
    let _ = f.rewind();
    let _ = f.read_to_end(&mut held);
    out(held)?;
    // A reload loads the box HOME's ~/.config/hypr modules again (#246): the config's errors said.
    if rc == 0 {
        if let Some(a) = rest.iter().find(|a| !a.starts_with(b"-")) {
            if a == b"reload" && !hypr_errors_note(ctx)? {
                rc = 1;
            }
        }
    }
    code(rc)
}

/// `return RC`.
pub fn code(rc: i32) -> Res<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(Exit(u8::try_from(rc & 0xff).unwrap_or(1)))
    }
}

/// `f=$(mktemp)`, opened and unlinked: mktemp's own words on stderr when it cannot.
fn temp_file() -> Option<std::fs::File> {
    let o = crate::sig::command("mktemp")
        .stderr(std::process::Stdio::inherit())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let p = shell::path(&shell::strip_nl(&o.stdout));
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&p)
        .ok();
    let _ = std::fs::remove_file(&p);
    f
}
