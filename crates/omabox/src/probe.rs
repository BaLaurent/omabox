//! `omabox _rust probe NAME [ARG...]`: the suite's read-only view of the helpers it used to source from
//! bin/omabox (#324 wave 6-suite, D5): its startup (the host's session, the render node) and what its
//! box tests read (a box's processes, a recording's overlay, which aquamarine a system dir gives).
//!
//! Internal: an `_` word, so the front never parses it and a broker refuses it (broker_check). Only
//! the names in NAMES, each of which reads and prints, never changes anything: no box, setting or file
//! is made or removed, no host command is run but the read-only ones the helper itself runs (hyprctl
//! instances, ldd, sort and awk). Not `host_hyprctl`: it passes its words to hyprctl on the real
//! desktop (a dispatch); the suite runs hyprctl itself with the signature host_session prints.

use crate::cat;
use crate::exit::{Ctx, Exit, Res, out};
use crate::install::{AQ_BUILD, AQ_FIX_SYMBOL, AQ_FIXED_AFTER};

/// The helpers the probe answers for, and the words each takes.
pub const NAMES: &[(&str, &str)] = &[
    ("host_session", ""),
    ("render_node", ""),
    ("render_nodes", ""),
    ("render_driver", "NODE"),
    ("hypr_aq_soname", ""),
    ("aq_pick", "SONAME SYSDIR"),
    ("aq_has_fix", "FILE"),
    ("aq_ver_fixed", "VERSION"),
    ("aq_const", "AQ_FIX_SYMBOL|AQ_FIXED_AFTER|AQ_BUILD"),
    ("desk_facts", "BEFORE AFTER"),
    ("box_pids", "NAME"),
    (
        "record_overlay",
        "INPUT MARKS FIRST X0 Y0 K W H DUR SHOW CAPS [AREA]",
    ),
];

/// One line a word: the probe's own usage, for a name it does not have.
fn usage() -> Exit {
    let mut m = b"omabox _rust probe: one of".to_vec();
    for (n, a) in NAMES {
        m.extend_from_slice(b"\n  ");
        m.extend_from_slice(n.as_bytes());
        if !a.is_empty() {
            m.push(b' ');
            m.extend_from_slice(a.as_bytes());
        }
    }
    m.push(b'\n');
    let _ = std::io::Write::write_all(&mut std::io::stderr(), &m);
    Exit(2)
}

fn line(v: &[u8]) -> Res<()> {
    out(cat!(v, "\n"))
}

/// `_rust probe NAME ARG...`: NAME's answer on stdout, its refusal on stderr, as the helper gives
/// them; 2 for a name not on the list or the wrong number of words.
pub fn run(boxes: Vec<u8>, words: &[Vec<u8>]) -> Res<()> {
    let mut ctx = Ctx::new(b"", boxes);
    ctx.parsed();
    let Some((name, args)) = words.split_first() else {
        return Err(usage());
    };
    let a: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
    match (name.as_slice(), a.as_slice()) {
        (b"host_session", []) => {
            let (sig, wl) = crate::host::host_session_die(&ctx)?;
            line(&cat!(sig, " ", wl))
        }
        (b"render_node", []) => line(crate::render::render_node(&ctx)?.as_bytes()),
        (b"render_nodes", []) => {
            for n in crate::render::render_nodes() {
                line(n.as_bytes())?;
            }
            Ok(())
        }
        (b"render_driver", [n]) => line(&crate::gpu::render_driver(n)),
        (b"hypr_aq_soname", []) => line(&crate::install::hypr_aq_soname(&ctx)?),
        // As OMABOX_AQUAMARINE=system with SYSDIR the system's lib dir: KIND|VER|FIXED|DESC.
        (b"aq_pick", [so, sys]) => {
            let aq = crate::install::aq_pick_system(&ctx, so, sys).ok_or(Exit(1))?;
            line(&cat!(
                aq.kind,
                "|",
                aq.ver,
                "|",
                if aq.fixed { "1" } else { "0" },
                "|",
                aq.desc()
            ))
        }
        (b"aq_has_fix", [f]) => crate::install::aq_has_fix(f).then_some(()).ok_or(Exit(1)),
        (b"aq_ver_fixed", [v]) => crate::install::aq_ver_fixed(v).then_some(()).ok_or(Exit(1)),
        (b"aq_const", [c]) => match *c {
            b"AQ_FIX_SYMBOL" => line(AQ_FIX_SYMBOL.as_bytes()),
            b"AQ_FIXED_AFTER" => line(AQ_FIXED_AFTER.as_bytes()),
            b"AQ_BUILD" => line(AQ_BUILD.as_bytes()),
            _ => Err(usage()),
        },
        (b"desk_facts", [before, after]) => out(crate::desk::desk_facts(before, after)),
        (b"box_pids", [n]) => {
            ctx.name = n.to_vec();
            ctx.d = cat!(ctx.boxes, "/", n);
            for p in crate::jobs::box_pids(&ctx) {
                line(&p)?;
            }
            Ok(())
        }
        (
            b"record_overlay",
            [
                input,
                marks,
                first,
                x0,
                y0,
                k,
                w,
                h,
                dur,
                show,
                caps,
                area @ ..,
            ],
        ) if area.len() <= 1 => {
            let area = match area.first() {
                Some(a) => a.to_vec(),
                None => cat!("0 0 ", w, " ", h),
            };
            let ass = crate::cmd::record_stop::record_overlay(
                input,
                marks,
                first,
                x0,
                y0,
                k,
                w,
                h,
                dur,
                *show == b"1",
                *caps == b"1",
                &area,
            )
            .ok_or(Exit(1))?;
            out(ass)
        }
        _ => Err(usage()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_none_writes() {
        let mut seen = std::collections::HashSet::new();
        for (n, _) in NAMES {
            assert!(seen.insert(*n), "{n} twice");
            // (host_hyprctl passes its words to the real desktop's hyprctl: never a probe)
            assert_ne!(*n, "host_hyprctl");
        }
    }
}
