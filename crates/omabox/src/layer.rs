//! The box's layer surfaces (the shell's bar, panels, menus) as bin/omabox reads them: one by a
//! selector (layer_select, finding 146: a 1x1 one is a closed menu), their words (layer_lines,
//! layer_desc), and the accessibility tree of one (layer_read, #254, finding 358), read by
//! omabox-tree in the box.

use crate::boxes::meta;
use crate::cat;
use crate::desk::kb_layers;
use crate::exec::{ExecOpts, Redir, box_exec, pipe, read_all};
use crate::exit::{Ctx, Exit, Res, say};
use crate::jq::{self, Args};
use crate::jqprog::LAYER_LIST_JQ;
use crate::shell;
use crate::win::jq_r;
use std::io::Write;
use std::os::fd::{AsRawFd, RawFd};

pub const BOX_TREE: &str = "/opt/omabox/bin/omabox-tree";

/// layer_select LAYERS SEL: one layer of LAYERS (`hyprctl -j layers`) by its namespace,
/// address:0x... or pid:PID; a 1x1 one only to say it is not drawn (exit 1); none or several listed,
/// exit 2.
pub fn layer_select(ctx: &Ctx, lys: &[u8], sel: &[u8]) -> Res<Vec<u8>> {
    let a = jq::jq(&["-c", LAYER_LIST_JQ], Some(lys), jq::Err::Null);
    if !a.ok() {
        return Err(ctx.die(cat!("box '", ctx.name, "' did not list its layers")));
    }
    let all = a.text();
    let hits = Args::new(&["-c"])
        .kv("--arg", "s", sel)
        .arg(crate::jqprog::LAYER_HITS_JQ)
        .run(Some(&all), jq::Err::Inherit)
        .text();
    let big = jq::jq(
        &["-c", crate::jqprog::LAYER_BIG_JQ],
        Some(&hits),
        jq::Err::Inherit,
    )
    .text();
    let n = crate::win::jq_length(&big);
    match n.as_slice() {
        b"1" => return Ok(jq::jq(&["-c", ".[0]"], Some(&big), jq::Err::Inherit).text()),
        b"0" => {
            if crate::win::jq_length(&hits) != b"0" {
                say(cat!(
                    "tree: layer ",
                    sel,
                    " is mapped at 1x1, not drawn (a closed menu: open it first)"
                ));
                return Err(Exit(1));
            }
            say(cat!(
                "no layer matches ",
                sel,
                " (a namespace, address:0x... or pid:PID); the box's layers:"
            ));
            layer_lines(&all);
        }
        _ => {
            say(cat!(
                sel,
                " matches ",
                n,
                " layers: name one by its address (address:0x...):"
            ));
            layer_lines(&big);
        }
    }
    Err(Exit(2))
}

/// layer_lines LIST >&2: a layer a line, for a refusal.
pub fn layer_lines(list: &[u8]) {
    let r = jq::jq(
        &["-r", crate::jqprog::LAYER_LINES_JQ],
        Some(list),
        jq::Err::Inherit,
    );
    let _ = std::io::stderr().write_all(&r.out);
}

/// layer_desc LAYER: `"omarchy-bar" (pid 142, 42x1080 at 0,0)`, for messages.
pub fn layer_desc(l: &[u8]) -> Vec<u8> {
    jq_r(crate::jqprog::LAYER_DESC_JQ, l)
}

/// The layers of LAYERS of LAYER's pid and size (its frame is the one of its pid that is its size).
pub fn layer_tied(l: &[u8], lys: &[u8]) -> Vec<u8> {
    Args::new(&["-c"])
        .arg(format!("{LAYER_LIST_JQ}{}", crate::jqprog::LAYER_TIED_JQ))
        .kv("--argjson", "l", l)
        .run(Some(lys), jq::Err::Inherit)
        .text()
}

/// box_sys ARGV... 2>"$ERR": one of omabox's own helpers in the box (box_exec's BOX_SYS), its
/// stderr, and every word said on the way, in the file ERR is open on.
pub fn box_sys_err(ctx: &mut Ctx, argv: &[Vec<u8>], err: RawFd) -> Res<i32> {
    let _e = Redir::to(2, err);
    box_exec(
        ctx,
        argv,
        ExecOpts {
            sys: true,
            ..ExecOpts::default()
        },
    )
}

/// `$(box_sys ARGV... 2>/dev/null)`: its output, or None where it failed to start.
pub fn box_sys_out(ctx: &mut Ctx, argv: &[Vec<u8>]) -> Option<(i32, Vec<u8>)> {
    let (r, w) = pipe().ok()?;
    let reader = std::thread::spawn(move || read_all(r, None));
    let rc = {
        let _e = Redir::null(2);
        box_exec(
            ctx,
            argv,
            ExecOpts {
                sys: true,
                stdout: Some(w.as_raw_fd()),
                ..ExecOpts::default()
            },
        )
    };
    drop(w);
    let out = reader.join().unwrap_or_default();
    match rc {
        Ok(rc) => Some((rc, shell::subst(&out))),
        Err(_) => None,
    }
}

/// layer_read LAYER LAYERS ERR [omabox-tree ARGS]: LAYER's accessibility tree on stdout, in the
/// screen's coordinates, the tool's words in ERR; its exit code (3: no pid to find its app by). Two
/// layers of one pid and size are told apart only when exactly one holds the keyboard (kb_layers).
pub fn layer_read(
    ctx: &mut Ctx,
    l: &[u8],
    lys: &[u8],
    err: &std::fs::File,
    args: &[Vec<u8>],
) -> Res<i32> {
    let f = shell::read_fields(&jq_r(crate::jqprog::LAYER_FIELDS_JQ, l), 5);
    let (pid, w, h, x, y) = (&f[0], &f[1], &f[2], &f[3], &f[4]);
    if !shell::re_match("^[1-9][0-9]*$", pid) {
        let mut e = err;
        let _ = e.write_all(&cat!(
            "the layer has no pid to find its application by (",
            pid,
            ")\n"
        ));
        return Ok(3);
    }
    let tied = layer_tied(l, lys);
    let mut hint: Vec<Vec<u8>> = Vec::new();
    let more = shell::test_int(&crate::win::jq_length(&tied)).is_some_and(|n| n > 1);
    if more {
        let kb = kb_layers(ctx);
        let ex = Args::new(&["-r"])
            .kv("--arg", "kb", &kb)
            .arg(crate::jqprog::LAYER_KB_JQ)
            .run(Some(&tied), jq::Err::Inherit)
            .text();
        if !ex.is_empty() {
            let yes: &[u8] = if ex == jq_r(".address", l) {
                b"yes"
            } else {
                b"no"
            };
            hint = vec![b"--active".to_vec(), yes.to_vec()];
        }
    }
    let mut argv: Vec<Vec<u8>> = ["/usr/bin/timeout", "30", BOX_TREE, "--pid"]
        .iter()
        .map(|w| w.as_bytes().to_vec())
        .collect();
    argv.extend([
        pid.clone(),
        b"--layer".to_vec(),
        b"--size".to_vec(),
        cat!(w, "x", h),
        b"--offset".to_vec(),
        cat!(x, ",", y),
    ]);
    argv.extend(hint);
    argv.extend(args.iter().cloned());
    box_sys_err(ctx, &argv, err.as_raw_fd())
}

/// layer_qs_note LAYER: after "no accessibility tree" for a layer of a Quickshell, which releases
/// publish one.
pub fn layer_qs_note(ctx: &mut Ctx, l: &[u8]) {
    let pid = jq_r(".pid", l);
    let argv = vec![b"/usr/bin/cat".to_vec(), cat!("/proc/", pid, "/comm")];
    let comm = box_sys_out(ctx, &argv)
        .filter(|(rc, _)| *rc == 0)
        .map(|(_, o)| o)
        .unwrap_or_default();
    if comm != b"quickshell" && comm != b"qs" {
        return;
    }
    let v = meta(&ctx.d, "quickshell_version").0;
    let v = if v.is_empty() {
        Vec::new()
    } else {
        cat!("; this box runs ", v)
    };
    say(cat!(
        "Quickshell publishes its windows' accessibility trees from 0.3.2 on (quickshell-mirror/quickshell 916a0dd)",
        v,
        ": up --quickshell PATH runs a build of your own"
    ));
}
