//! `omabox windows [-b NAME] [--json]`: the box's mapped windows (finding 81), bottom to top as they
//! stack, where each is, what covers it, and the layers over them (#263, findings 398, 421).

use crate::args::{optarg, unexpected, unknown_opt};
use crate::boxes::{meta, need_box};
use crate::cat;
use crate::exit::{Ctx, Res, out};
use crate::hypr::hypr_json;
use crate::jq;
use crate::select::select_box;
use crate::shell::strip_nl;
use crate::shellnote::shell_note;
use crate::win::box_windows;

/// One line, each rectangle as x, y, w, h too, as every other --json gives one (#273, audit N2, N3).
const JSON_JQ: &str = "def r: . + {x: .at[0], y: .at[1], w: .size[0], h: .size[1]};
      map(r | .cover |= map(r) | if .layers then .layers |= map(r) else . end)";
/// windows_table: the rows, padded in jq (finding 174), the title JSON-quoted once.
const TABLE_JQ: &str = r#"def pad($n): tostring | . + (" " * ($n - length) // "");
    .[] | [(.address | pad(16)), (.pid | pad(8)), (.workspace | pad(10)),
      ((if .onscreen | not then "off-screen" elif .cover != [] then "covered" else "on-screen" end)
        + (if .active then ",active" else "" end) | pad(20)),
      ("\(.size[0])x\(.size[1]) at \(.at[0]),\(.at[1])" | pad(22)), (.class | pad(16)), (.title | tojson)] | join(" ")"#;
/// windows_layers: the layer surfaces over windows (#263), one line each, naming the windows.
const LAYERS_JQ: &str = r#"[.[] | .address as $a | .layers[]? | {k: [.namespace, .layer, .at, .size, (.certain != false)], a: $a}]
    | group_by(.k)[] | .[0].k as [$n, $l, $at, $sz, $c]
    | "layer \($n | tojson) (\($l) \($sz[0])x\($sz[1]) at \($at[0]),\($at[1])) "
      + if $c then "is over \(map(.a) | join(", ")): where it takes input, input there goes to it"
        else "spans the screen, possibly over \(map(.a) | join(", ")): input goes to it only where one of its items (a toast, a dock icon) is, elsewhere to the window" end"#;

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    let (mut name, mut json) = (Vec::new(), false);
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_slice() {
            b"-b" | b"--box" => {
                optarg(ctx, a, args.get(i + 1))?;
                name = args[i + 1].clone();
                i += 1;
            }
            b"--json" => json = true,
            _ if a.starts_with(b"-") => return Err(unknown_opt(ctx, "windows", a)),
            _ => return Err(unexpected(ctx, "windows", a, "")),
        }
        i += 1;
    }
    select_box(ctx, &name)?;
    need_box(ctx)?;
    shell_note(ctx);
    let lys = strip_nl(&hypr_json(ctx, &["layers"])?);
    let wins = box_windows(ctx, Some(&lys))?;
    if json {
        let r = jq::jq(&["-c", JSON_JQ], Some(&wins), jq::Err::Inherit);
        out(&r.out)?;
        return crate::cmd::hyprctl::code(r.rc);
    }
    // A box on a Hyprland build of its own (finding 116) says so first; one whose shell runs a
    // Quickshell build (finding 358) too: its layer surfaces are not listed here.
    let built = |key: &str, ver: &str, what: &str| -> Res<()> {
        let b = meta(&ctx.d, key).0;
        if b.is_empty() {
            return Ok(());
        }
        let v = meta(&ctx.d, ver).0;
        let v = if v.is_empty() {
            Vec::new()
        } else {
            cat!(" (", v, ")")
        };
        out(cat!("box ", ctx.name, " ", what, " ", b, v, "\n"))
    };
    built("hyprland", "hyprland_version", "runs Hyprland")?;
    built(
        "quickshell",
        "quickshell_version",
        "runs its shell on Quickshell",
    )?;
    // Bottom to top, as they stack.
    out(format!(
        "{:<16} {:<8} {:<10} {:<20} {:<22} {:<16} {}\n",
        "ADDRESS", "PID", "WORKSPACE", "STATE", "GEOMETRY", "CLASS", "TITLE"
    ))?;
    let t = jq::jq(&["-r", TABLE_JQ], Some(&wins), jq::Err::Inherit);
    out(&t.out)?;
    if !t.ok() {
        return crate::cmd::hyprctl::code(t.rc);
    }
    let l = jq::jq(&["-r", LAYERS_JQ], Some(&wins), jq::Err::Inherit);
    out(&l.out)?;
    crate::cmd::hyprctl::code(l.rc)
}
