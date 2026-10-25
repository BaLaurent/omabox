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
    // A jailed caller's paths (OMABOX_JAIL the policy, as the broker sets it).
    ("jail_path", "PATH"),
    ("jail_read", "FILE"),
    ("jail_root", "PATH"),
    ("jail_root_covered", "PATH"),
    ("jail_deep", "PATH"),
    ("jail_readable", "FILE"),
    ("repo_top", ""),
    ("seed_spec", "SRC:DEST"),
    ("seed_blocked", "SRC REL PLACE..."),
    ("jail_cover", "D SRC DEST"),
    ("box_held", "D PID"),
    ("jail_verify", "D BOXPID SRC..."),
    ("covers_held", "NAME"),
    ("jail_sees", "PATH"),
    ("jail_masked", "PATH"),
    ("default_name", ""),
    ("caller_env", "NAME"),
    // The CLI's tables, for the suite's checks of help against them.
    ("cmd_opts_keys", ""),
    ("cmd_opts", "CMD"),
    ("up_box_opts", ""),
    ("usage_text", ""),
    ("shot_fit_default", ""),
    ("repo_git_dirs", "[TOP]"),
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
        (b"jail_path", [p]) => line(&crate::jailpath::jail_path(p).ok_or(Exit(1))?),
        (b"jail_read", [f]) => out(crate::jailpath::jail_read(f).ok_or(Exit(1))?),
        (b"jail_root", [p]) => line(&crate::repo::jail_root(p).ok_or(Exit(1))?),
        (b"jail_root_covered", [p]) => line(&crate::jailpath::jail_root_covered(p).ok_or(Exit(1))?),
        (b"jail_deep", [p]) => yes(crate::jailpath::jail_deep(p)),
        (b"jail_readable", [f]) => yes(crate::jailpath::jail_readable(f)),
        (b"repo_top", []) => line(&crate::repo::repo_top().ok_or(Exit(1))?),
        // SRC, a tab, DEST; a refusal said, 1 (3 for a jailed caller's).
        (b"seed_spec", [spec]) => match crate::upspec::seed_spec(spec) {
            Ok((src, dest)) => line(&cat!(src, "\t", dest)),
            Err(crate::upspec::SeedRefused::Bad) => Err(Exit(1)),
            Err(crate::upspec::SeedRefused::Jail) => Err(Exit(3)),
        },
        // Each PLACE "AT<tab>KIND<tab>NAME", as bash's list; the one blocked, a tab, in or over.
        (b"seed_blocked", [src, rel, places @ ..]) => {
            let mut ps = Vec::new();
            for x in places {
                let f: Vec<&[u8]> = x.split(|&c| c == b'\t').collect();
                let [at, kind, name] = f.as_slice() else {
                    return Err(usage());
                };
                let kind = match *kind {
                    b"plugin" => "plugin",
                    b"theme" => "theme",
                    _ => return Err(usage()),
                };
                ps.push(crate::seed::Place {
                    at: at.to_vec(),
                    kind,
                    name: name.to_vec(),
                });
            }
            let (x, how) = crate::seed::seed_blocked(src, rel, &ps, &crate::jailpath::jail_path)
                .ok_or(Exit(1))?;
            line(&cat!(x.at, "\t", x.kind, "\t", x.name, "\t", how))
        }
        // bash's covers (--ro-bind EMPTY PLACE, one word a line), "--", then cover_srcs.
        (b"jail_cover", [d, src, dest]) => {
            let c = crate::jailpath::jail_cover(src, dest).ok_or(Exit(1))?;
            for (k, at) in &c.binds {
                let empty = match k {
                    crate::jailpath::CoverKind::File => "cover-file",
                    crate::jailpath::CoverKind::Dir => "cover-dir",
                };
                line(&cat!("--ro-bind\n", d, "/", empty, "\n", at))?;
            }
            line(b"--")?;
            match c.src {
                Some(s) => line(&s),
                None => Ok(()),
            }
        }
        (b"box_held", [d, pid]) => {
            ctx.d = d.to_vec();
            yes(crate::ready::box_held(&ctx, pid))
        }
        // What is not covered on stdout (its words, as `$(jail_verify ... 2>&1)` has them), 1.
        (b"jail_verify", [d, bp, srcs @ ..]) => {
            let srcs: Vec<Vec<u8>> = srcs.iter().map(|s| s.to_vec()).collect();
            crate::jailpath::jail_verify(d, bp, &srcs).or_else(|w| {
                out(w)?;
                Err(Exit(1))
            })
        }
        (b"jail_sees", [p]) => line(&crate::jailpath::jail_sees(p).ok_or(Exit(1))?),
        (b"jail_masked", [p]) => yes(crate::jailpath::jail_masked(p)),
        (b"default_name", []) => line(&crate::select::default_name()),
        // (printf '%s': no newline added)
        (b"caller_env", [n]) => out(crate::exec::caller_env(n).ok_or(Exit(1))?),
        (b"cmd_opts_keys", []) => {
            for k in crate::args::cmd_opts_keys() {
                line(k.as_bytes())?;
            }
            Ok(())
        }
        // One option a line, as bash's cmd_opts (an alias's: its command's).
        (b"cmd_opts", [c]) => {
            for o in crate::args::cmd_opts(c).ok_or(Exit(1))?.split(' ') {
                line(o.as_bytes())?;
            }
            Ok(())
        }
        (b"up_box_opts", []) => line(crate::args::UP_BOX_OPTS.as_bytes()),
        (b"usage_text", []) => out(crate::help::TEXT),
        // (nothing for off; a bad OMABOX_SHOT_FIT said, 1)
        (b"shot_fit_default", []) => match crate::shots::shot_fit_default() {
            Ok(v) if v.is_empty() => Ok(()),
            Ok(v) => line(&v),
            Err(()) => Err(Exit(1)),
        },
        // TOP, or repo_top's (none: 1), a jailed caller's read through its jail.
        (b"repo_git_dirs", top @ ([] | [_])) => {
            let top = match top.first() {
                Some(t) => t.to_vec(),
                None => crate::repo::repo_top().ok_or(Exit(1))?,
            };
            for d in crate::install::repo_git_dirs(&top, crate::jailpath::view()) {
                line(&d)?;
            }
            Ok(())
        }
        (b"covers_held", [n]) => {
            ctx.name = n.to_vec();
            ctx.d = cat!(ctx.boxes, "/", n);
            crate::boxes::covers_held(&ctx)
        }
        _ => Err(usage()),
    }
}

/// A test's answer as its status: 0 yes, 1 no.
fn yes(b: bool) -> Res<()> {
    if b { Ok(()) } else { Err(Exit(1)) }
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
