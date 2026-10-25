//! `omabox wait [-b NAME] [--timeout 10s] [--json] COND` (finding 82): until the screen holds still
//! or changes (omabox-still in the box: settle.rs), a window or layer is there or gone, or a command
//! succeeds (polled here). One line on stdout (or --json); 0 satisfied, 124 not in time, 1 unknown.

use crate::args::{optarg, unexpected, unknown_opt, unknown_word};
use crate::boxes::{box_alive, need_box, touch};
use crate::cat;
use crate::exec::{ExecOpts, Redir, box_exec};
use crate::exit::{Ctx, Exit, Res, out, say};
use crate::hypr::{hung_text, hypr_answers_in, hypr_json};
use crate::jq::{self, Args};
use crate::jqprog::WIN_DESC_JQ;
use crate::num::{fixed, ms_duration, ms_s, xywh_less};
use crate::select::select_box;
use crate::settle::{
    Settle, WAIT_MAX_MS, cursor_now, cursor_rect, geom_csv, geom_parse, mono_ms, rects_meet,
};
use crate::shell::{dec, strip_nl};
use crate::shellnote::shell_note;
use crate::win::{
    box_windows, joined, jq_error_text, jq_length, jq_r, win_desc, win_hits, win_select,
};
use std::time::Duration;

/// A polled condition's one try (win_probe, layer_probe, until_probe): "yes DETAIL" or "no DETAIL";
/// status 2 a selector never answered (a bad regex), any other failure the box not answering.
enum Probe<'a> {
    Win(&'a str, &'a [Vec<u8>]),
    Layer(&'a str, &'a [u8]),
    Until(&'a [Vec<u8>]),
}

impl Probe<'_> {
    /// Its name, as poll_until's --json condition has it.
    fn name(&self) -> &'static str {
        match self {
            Probe::Win(..) => "win_probe",
            Probe::Layer(..) => "layer_probe",
            Probe::Until(..) => "until_probe",
        }
    }

    /// `out=$(PROBE)`: its status and its output (trailing newlines off).
    fn run(&self, ctx: &mut Ctx, t0: i64, tmo: i64) -> (i32, Vec<u8>) {
        match self {
            Probe::Win(want, sels) => {
                let wins = {
                    let _quiet = Redir::null(2);
                    box_windows(ctx, None)
                };
                // (`wins=$(box_windows 2>/dev/null) || return 1`)
                match wins {
                    Ok(w) => win_answer(want, &w, sels),
                    Err(_) => (1, Vec::new()),
                }
            }
            Probe::Layer(want, ns) => {
                let layers = {
                    let _quiet = Redir::null(2);
                    hypr_json(ctx, &["layers"])
                };
                let Ok(layers) = layers else {
                    return (1, Vec::new());
                };
                let l = Args::new(&["-r"])
                    .kv("--arg", "n", ns)
                    .arg(crate::jqprog::LAYER_AT_JQ)
                    .run(Some(&strip_nl(&layers)), jq::Err::Inherit)
                    .text();
                let o: Vec<u8> = match (*want, l.is_empty()) {
                    ("gone", true) => b"yes".to_vec(),
                    ("gone", false) | (_, true) => b"no".to_vec(),
                    _ => cat!("yes at ", l),
                };
                (0, o)
            }
            Probe::Until(cmd) => {
                let left = tmo - (mono_ms() - t0);
                match box_alive(ctx) {
                    Ok(true) => {}
                    Ok(false) => return (1, Vec::new()),
                    Err(Exit(rc)) => return (i32::from(rc), Vec::new()),
                }
                let secs = fixed(left.max(100) as f64 / 1000.0, 3);
                let mut argv = vec![
                    b"/usr/bin/timeout".to_vec(),
                    b"-k".to_vec(),
                    b"1".to_vec(),
                    secs.into_bytes(),
                ];
                argv.extend(cmd.iter().cloned());
                let rc = {
                    let _o = Redir::null(1);
                    let _e = Redir::null(2);
                    box_exec(ctx, &argv, ExecOpts::default())
                };
                // box_exec's die ends the probe's `$(...)` with its status: no answer from the
                // box, poll_until's to say (its "went down" or "could not ask").
                let rc = match rc {
                    Ok(rc) => rc,
                    Err(Exit(rc)) => return (i32::from(rc), Vec::new()),
                };
                if rc == 0 {
                    (0, b"yes".to_vec())
                } else {
                    (0, format!("no exit {rc}").into_bytes())
                }
            }
        }
    }
}

/// win_answer WANT WINDOWS SEL...: win_probe's answer for these windows.
pub fn win_answer(want: &str, wins: &[u8], sels: &[Vec<u8>]) -> (i32, Vec<u8>) {
    // No list: the box not answering, not "none match" (finding 176).
    if wins.is_empty() {
        return (1, Vec::new());
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
            return (2, Vec::new());
        }
    };
    let n = jq_length(&hits);
    let prog = match want {
        "gone" => {
            let o = if n == b"0" {
                b"yes".to_vec()
            } else {
                cat!("no ", n, " matching")
            };
            return (0, o);
        }
        "focused" => crate::jqprog::WIN_FOCUSED_JQ,
        _ => crate::jqprog::WIN_PRESENT_JQ,
    };
    let r = jq::jq(
        &["-r", &format!("{WIN_DESC_JQ}{prog}")],
        Some(&hits),
        jq::Err::Inherit,
    );
    (r.rc, r.text())
}

/// poll_until YES NO NEED TIMEOUT_MS JSON PROBE: PROBE every 100 ms until it says yes NEED times in a
/// row (2 for a window or layer: one caught mid-map or mid-close is not an answer), or the deadline.
#[allow(clippy::too_many_arguments)]
fn poll_until(
    ctx: &mut Ctx,
    yes_text: &[u8],
    no_text: &[u8],
    need: u32,
    tmo: i64,
    json: bool,
    probe: Probe,
) -> Res<()> {
    // (the monotonic clock: finding 450)
    let t0 = mono_ms();
    let mut used = std::time::Instant::now();
    let (mut yes, mut detail) = (0u32, Vec::new());
    let mut now: i64;
    let (res, msg): (&str, Vec<u8>);
    loop {
        let (rc, o) = probe.run(ctx, t0, tmo);
        now = mono_ms() - t0;
        if rc == 2 {
            return Err(Exit(2));
        }
        if rc != 0 {
            // A probe that could not ask a box still up: its Hyprland not answering, said so (#125).
            if box_alive(ctx)? {
                if !hypr_answers_in(ctx, "1")? {
                    let t = hung_text(ctx, "")?;
                    return Err(ctx.die(t));
                }
                return Err(ctx.die(cat!("wait: could not ask box '", ctx.name, "'")));
            }
            res = "unknown";
            msg = cat!(
                "box '",
                ctx.name,
                "' went down after ",
                ms_s(now.to_string())
            );
            break;
        }
        detail = match o.iter().position(|&c| c == b' ') {
            Some(i) => o[i + 1..].to_vec(),
            None => Vec::new(),
        };
        if o.starts_with(b"yes") {
            yes += 1;
            if yes >= need {
                let d = if detail.is_empty() {
                    Vec::new()
                } else {
                    cat!(": ", detail)
                };
                res = "satisfied";
                msg = cat!(yes_text, " after ", ms_s(now.to_string()), d);
                break;
            }
        } else {
            yes = 0;
        }
        if now >= tmo {
            let d = if detail.is_empty() {
                Vec::new()
            } else {
                cat!(" (", detail, ")")
            };
            res = "unsatisfied";
            msg = cat!(no_text, " after ", ms_s(now.to_string()), d);
            break;
        }
        if used.elapsed() >= std::time::Duration::from_secs(30) {
            touch(&ctx.at("used"));
            used = std::time::Instant::now();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if json {
        let r = Args::new(&["-nc"])
            .kv("--arg", "res", res)
            .kv("--arg", "cond", probe.name())
            .kv("--arg", "msg", &msg)
            .kv("--arg", "t", now.to_string())
            .kv("--arg", "d", &detail)
            .arg(crate::jqprog::POLL_JSON_JQ)
            .run(None, jq::Err::Inherit);
        out(&r.out)?;
    } else {
        out(cat!(res, ": ", msg, "\n"))?;
    }
    match res {
        "satisfied" => Ok(()),
        "unsatisfied" => Err(Exit(124)),
        _ => Err(Exit(1)),
    }
}

/// A number omabox cannot read in WHAT (one past 64 bits; finding 448: bash's arithmetic refused an
/// X of 08 in its own words): said, the command ends, 1.
fn num_fail(ctx: &Ctx, what: &[u8]) -> Exit {
    ctx.die(cat!("wait: cannot read the numbers of ", what))
}

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    let (mut name, mut json, mut tmo, mut cond) = (Vec::new(), false, 10_000i64, Vec::new());
    let (mut quiet, mut strict, mut geom) = (Vec::new(), false, Vec::new());
    let (mut sels, mut ns, mut cmd): (Vec<Vec<u8>>, Vec<u8>, Vec<Vec<u8>>) =
        (Vec::new(), Vec::new(), Vec::new());
    let mut want = "";
    let mut masks: Vec<Vec<u8>> = Vec::new();
    let mut s = Settle::default();
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
            b"--ignore" => {
                optarg(ctx, a, v)?;
                let m = s.mask_arg(ctx, &args[i + 1])?;
                masks.extend([b"--mask".to_vec(), m]);
                i += 1;
            }
            b"--timeout" => {
                optarg(ctx, a, v)?;
                let Some(ms) = ms_duration(&args[i + 1]) else {
                    return Err(ctx.die("wait: --timeout takes a duration: 500ms, 10s, 2m"));
                };
                if ms > WAIT_MAX_MS {
                    return Err(ctx.die(cat!(
                        "wait: --timeout is at most 10m (got ",
                        args[i + 1],
                        ")"
                    )));
                }
                tmo = ms;
                i += 1;
            }
            b"--quiet" => {
                optarg(ctx, a, v)?;
                let Some(ms) = ms_duration(&args[i + 1]) else {
                    return Err(ctx.die("wait: --quiet takes a duration: 300ms, 1s"));
                };
                if ms > WAIT_MAX_MS {
                    return Err(ctx.die(cat!(
                        "wait: --quiet is at most 10m (got ",
                        args[i + 1],
                        ")"
                    )));
                }
                quiet = ms.to_string().into_bytes();
                i += 1;
            }
            b"--strict" => strict = true,
            b"-g" => {
                optarg(ctx, a, v)?;
                geom = args[i + 1].clone();
                i += 1;
            }
            b"-w" | b"--window" => {
                optarg(ctx, a, v)?;
                sels.push(args[i + 1].clone());
                i += 1;
            }
            b"--active" => sels.push(b"active".to_vec()),
            b"--gone" => want = "gone",
            b"--focused" => want = "focused",
            b"--" => {
                if cond != b"until" {
                    return Err(ctx.die("wait: -- goes with until (omabox wait until -- CMD ...)"));
                }
                cmd = args[i + 1..].to_vec();
                break;
            }
            _ if a.starts_with(b"-") => return Err(unknown_opt(ctx, "wait", a)),
            // `wait cmd` was until's name up to 0.8.0 (#272): refused since 1.0 (#287).
            _ => {
                if cond.is_empty() {
                    if a == b"cmd" {
                        return Err(ctx.usage_die(
                            "wait cmd is refused since 1.0: write wait until -- CMD (the same flags)",
                        ));
                    }
                    cond = a.clone();
                } else if cond == b"window" {
                    sels.push(a.clone());
                } else if cond == b"layer" && ns.is_empty() {
                    ns = a.clone();
                } else {
                    return Err(unexpected(ctx, cat!("wait ", cond), a, ""));
                }
            }
        }
        i += 1;
    }
    let none = geom.is_empty() && quiet.is_empty();
    match cond.as_slice() {
        b"still" | b"change" => {
            if !want.is_empty() {
                return Err(ctx.die("wait: --gone and --focused go with window or layer"));
            }
            if !geom.is_empty() && !sels.is_empty() {
                return Err(ctx.die("wait: -g or --window, not both"));
            }
        }
        b"window" => {
            if sels.is_empty() {
                return Err(ctx.die("wait window: which window? (a SEL, as for shot --window)"));
            }
            if !(none && !strict && masks.is_empty()) {
                return Err(ctx.die(
                    "wait window: -g, --quiet, --strict and --ignore go with still and change",
                ));
            }
        }
        b"layer" => {
            if ns.is_empty() {
                return Err(ctx.die(
                    "wait layer: which layer? its namespace (omabox hyprctl layers): omarchy-menu, ...",
                ));
            }
            if want == "focused" {
                return Err(ctx.die("wait layer: --gone or nothing"));
            }
            if !(sels.is_empty() && none && !strict && masks.is_empty()) {
                return Err(ctx.die("wait layer: only --gone"));
            }
        }
        b"until" => {
            if cmd.is_empty() {
                return Err(ctx.die("wait until: nothing to run (omabox wait until -- CMD ...)"));
            }
            if !masks.is_empty() {
                return Err(ctx.die("wait until: --ignore goes with still and change"));
            }
        }
        b"" => {
            return Err(ctx.die(
                "wait: for what? still, change, window SEL, layer NAMESPACE or until -- CMD (omabox help)",
            ));
        }
        _ => {
            return Err(unknown_word(
                ctx,
                "wait",
                "condition",
                &cond,
                "(still, change, window SEL, layer NAMESPACE or until -- CMD)",
                &["still", "change", "window", "layer", "until"],
            ));
        }
    }
    if cond == b"change" && !quiet.is_empty() {
        return Err(ctx.die("wait change: no --quiet (that is still's)"));
    }
    select_box(ctx, &name)?;
    need_box(ctx)?;
    shell_note(ctx)?;
    match cond.as_slice() {
        b"window" => {
            let w = cat!("window ", joined(&sels));
            let (yes, no) = match want {
                "gone" => (cat!(w, " gone"), cat!(w, " still there")),
                "focused" => (cat!(w, " focused"), cat!(w, " not focused")),
                _ => (w.clone(), cat!("no ", w)),
            };
            let probe = Probe::Win(if want.is_empty() { "present" } else { want }, &sels);
            poll_until(ctx, &yes, &no, 2, tmo, json, probe)
        }
        b"layer" => {
            let l = cat!("layer ", ns);
            let (yes, no) = if want == "gone" {
                (cat!(l, " gone"), cat!(l, " still there"))
            } else {
                (l.clone(), cat!("no ", l))
            };
            let probe = Probe::Layer(if want == "gone" { "gone" } else { "present" }, &ns);
            poll_until(ctx, &yes, &no, 2, tmo, json, probe)
        }
        b"until" => poll_until(
            ctx,
            b"command succeeded",
            b"command still failing",
            1,
            tmo,
            json,
            Probe::Until(&cmd),
        ),
        _ => screen(
            ctx, &mut s, &cond, tmo, &quiet, strict, &geom, &sels, masks, json,
        ),
    }
}

/// wait still|change: omabox-still over the screen, a region of it (-g) or a window's place.
#[allow(clippy::too_many_arguments)]
fn screen(
    ctx: &mut Ctx,
    s: &mut Settle,
    cond: &[u8],
    tmo: i64,
    quiet: &[u8],
    strict: bool,
    geom: &[u8],
    sels: &[Vec<u8>],
    mut masks: Vec<Vec<u8>>,
    json: bool,
) -> Res<()> {
    let mut args: Vec<Vec<u8>> = vec![b"--timeout".to_vec(), tmo.to_string().into_bytes()];
    if !quiet.is_empty() {
        args.extend([b"--quiet".to_vec(), quiet.to_vec()]);
    }
    if strict {
        args.push(b"--strict".to_vec());
    }
    let (mut region, mut w) = (Vec::new(), Vec::new());
    if !geom.is_empty() {
        let Some(g) = geom_parse(geom) else {
            return Err(ctx.die(cat!("wait: -g is \"X,Y WxH\" (or X,Y,W,H), got ", geom)));
        };
        region = geom_csv(&g);
    } else if !sels.is_empty() {
        // Its place on the screen: what is drawn there is what is watched.
        let wins = box_windows(ctx, None)?;
        w = win_select(ctx, &wins, sels)?;
        if jq_r(".onscreen", &w) != b"true" {
            return Err(ctx.die(cat!(
                "wait: window ",
                win_desc(&w),
                " is not on screen (workspace ",
                jq_r(".workspace", &w),
                "): ",
                cond,
                " watches its place on the screen"
            )));
        }
        region = jq_r(crate::jqprog::WIN_REGION_JQ, &w);
    }
    if !region.is_empty() {
        args.extend([b"--region".to_vec(), region.clone()]);
    }
    // --ignore: the screen's coordinates, or with --window the window's own (F8, #287): moved by its
    // place. One that misses the region watched leaves nothing out, said (#162).
    let (mut ox, mut oy) = (Vec::new(), Vec::new());
    let mut place: Vec<u8> = b"the region watched".to_vec();
    if !sels.is_empty() {
        let f: Vec<Vec<u8>> = {
            let line = region.split(|&c| c == b'\n').next().unwrap_or_default();
            let mut it = line.splitn(4, |&c| c == b',');
            (0..4)
                .map(|_| it.next().unwrap_or_default().to_vec())
                .collect()
        };
        (ox, oy) = (f[0].clone(), f[1].clone());
        s.origin = Some((ox.clone(), oy.clone()));
        place = cat!(
            "window ",
            win_desc(&w),
            " (",
            f[2],
            "x",
            f[3],
            ", in its own coordinates)"
        );
    }
    let mut k = 1;
    while k < masks.len() {
        if !ox.is_empty() {
            let line = masks[k].clone();
            let mut it = line.splitn(4, |&c| c == b',');
            let mut f = || it.next().unwrap_or_default().to_vec();
            let (x, y, mw, mh) = (f(), f(), f(), f());
            let add = |a: &[u8], b: &[u8]| -> Option<i64> { Some(dec(a)?.saturating_add(dec(b)?)) };
            let (Some(nx), Some(ny)) = (add(&x, &ox), add(&y, &oy)) else {
                return Err(num_fail(ctx, &cat!(line, " (at ", ox, ",", oy, ")")));
            };
            masks[k] = cat!(nx.to_string(), ",", ny.to_string(), ",", mw, ",", mh);
        }
        if !region.is_empty() {
            match rects_meet(&masks[k], &region) {
                None => return Err(num_fail(ctx, &cat!(masks[k], " and ", region))),
                Some(true) => {}
                Some(false) => {
                    let (zx, zy): (&[u8], &[u8]) = if ox.is_empty() {
                        (b"0", b"0")
                    } else {
                        (&ox, &oy)
                    };
                    say(cat!(
                        "wait: --ignore \"",
                        xywh_less(&masks[k], zx, zy),
                        "\" is outside ",
                        place,
                        ": it leaves nothing out"
                    ));
                }
            }
        }
        k += 2;
    }
    args.extend(masks);
    let p = cursor_now(ctx)?;
    if !p.is_empty() {
        let Some(r) = cursor_rect(&p) else {
            return Err(num_fail(ctx, &cat!("the pointer's place ", p)));
        };
        args.extend([b"--ignore".to_vec(), r]);
    }
    let c = String::from_utf8_lossy(cond).into_owned();
    let mut tool = vec![cond.to_vec()];
    tool.extend(args);
    s.begin(ctx, &tool)?;
    let rc = s.end(ctx, &c, json, b"")?;
    crate::cmd::hyprctl::code(rc)
}
