//! `omabox version` (`--version`, `-V`): omabox's version, then what a new box would run on: the
//! aquamarine (finding 125). Nothing else is taken (finding 348).

use crate::args::unknown_opt;
use crate::cat;
use crate::exit::{Ctx, Exit, Res, muted, out, say};
use crate::install::{Aq, aq_resolve, hypr_aq_soname};
use crate::json;
use serde_json::{Map, Value};

pub fn run(ctx: &mut Ctx, args: &[Vec<u8>]) -> Res<()> {
    let mut as_json = false;
    for a in args {
        if a == b"--json" {
            as_json = true;
        } else if a.starts_with(b"-") {
            return Err(unknown_opt(ctx, "--version", a));
        } else {
            say(cat!(
                "--version: unknown argument ",
                a,
                " (omabox --version [--json])"
            ));
            return Err(Exit(2));
        }
    }
    ctx.parsed();
    // (`2>/dev/null` on both: a Hyprland ldd cannot read, a bad OMABOX_AQUAMARINE, said nowhere)
    let aq = muted(|| {
        hypr_aq_soname(ctx)
            .ok()
            .and_then(|want| aq_resolve(ctx, &want))
    });
    let version = version_text();
    if as_json {
        return out(cat!(json_line(&version, aq.as_ref()), "\n"));
    }
    out(cat!("omabox ", version, "\n"))?;
    if let Some(aq) = aq {
        out(cat!("aquamarine: ", aq.desc(), "\n"))?;
    }
    Ok(())
}

/// The checkout's VERSION when this binary was built (wave 6-front, A5; proposal D2): the same
/// bytes as `$(cat "$ROOT/VERSION")` while binary and tree agree, and the truth for a binary older
/// than the tree it runs from (a pull without ./install.sh), where reading the file would say the
/// pulled tree's.
const VERSION: &str = include_str!("../../../../VERSION");

pub fn version_text() -> Vec<u8> {
    crate::shell::strip_nl(VERSION.as_bytes())
}

/// --json (#273, audit J3): the version, and the aquamarine new boxes use as ls --json names a box's
/// (kind private or system, version, fixed), its words as message; null when none is found.
fn json_line(version: &[u8], aq: Option<&Aq>) -> String {
    let s = |v: &[u8]| Value::String(String::from_utf8_lossy(v).into_owned());
    let mut m = Map::new();
    m.insert("version".into(), s(version));
    let a = aq.map_or(Value::Null, |aq| {
        let mut a = Map::new();
        a.insert("kind".into(), s(&aq.kind));
        a.insert("version".into(), s(&aq.ver));
        a.insert("fixed".into(), Value::Bool(aq.fixed));
        a.insert("message".into(), s(&aq.desc()));
        Value::Object(a)
    });
    m.insert("aquamarine".into(), a);
    json::compact(&Value::Object(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_as_jq_writes_it() {
        assert_eq!(
            json_line(b"1.0.1", None),
            r#"{"version":"1.0.1","aquamarine":null}"#
        );
        let aq = Aq {
            kind: b"system".to_vec(),
            ver: b"0.16.0".to_vec(),
            fixed: true,
            ..Aq::default()
        };
        assert_eq!(
            json_line(b"1.0.1", Some(&aq)),
            r#"{"version":"1.0.1","aquamarine":{"kind":"system","version":"0.16.0","fixed":true,"message":"the system's, 0.16.0"}}"#
        );
    }
}
