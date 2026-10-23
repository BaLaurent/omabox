//! The user's settings (~/.config/omabox/config), as bin/omabox's conf_get reads them.

use crate::cat;
use crate::exit::say;
use crate::shell;

/// `sed -n "s/^[[:space:]]*KEY[[:space:]]*=[[:space:]]*//p" CONFIG | tail -n 1 | sed 's/[[:space:]]*$//'`:
/// the config's path and its last KEY's value, blanks around taken off (empty when none).
pub fn conf_raw(key: &str) -> (Vec<u8>, Vec<u8>) {
    let config = cat!(shell::env_or_empty("HOME"), "/.config/omabox/config");
    let p = shell::path(&config);
    let mut v = Vec::new();
    if p.is_file() {
        // (sed's own words for a config it cannot read)
        let data = match std::fs::read(&p) {
            Ok(d) => d,
            Err(e) => {
                let _ = std::io::Write::write_all(
                    &mut std::io::stderr(),
                    &cat!(
                        "sed: can't read ",
                        config,
                        ": ",
                        crate::shots::why(&e),
                        "\n"
                    ),
                );
                Vec::new()
            }
        };
        let blank = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
        for l in data.split(|&c| c == b'\n') {
            let s = &l[l.iter().position(|&c| !blank(c)).unwrap_or(l.len())..];
            let Some(rest) = s.strip_prefix(key.as_bytes()) else {
                continue;
            };
            let rest = &rest[rest.iter().position(|&c| !blank(c)).unwrap_or(rest.len())..];
            let Some(rest) = rest.strip_prefix(b"=".as_slice()) else {
                continue;
            };
            v = rest[rest.iter().position(|&c| !blank(c)).unwrap_or(rest.len())..].to_vec();
        }
        while v.last().is_some_and(|&c| blank(c)) {
            v.pop();
        }
    }
    (config, v)
}

/// conf_get KEY with KEY's check (conf_check) and default (conf_default): a value that does not
/// check out is said and ignored.
pub fn conf_get(key: &str, check: fn(&[u8]) -> Option<Vec<u8>>, default: &[u8]) -> Vec<u8> {
    let (config, v) = conf_raw(key);
    if !v.is_empty() {
        if let Some(c) = check(&v) {
            return c;
        }
        say(cat!(config, ": ignoring ", key, "=", v));
    }
    default.to_vec()
}

/// conf_get workspace: where interactive and peek windows open (finding 70), 9 by default.
pub fn workspace() -> Vec<u8> {
    conf_get("workspace", crate::deskwin::ws_check, b"9")
}
