//! Reading a command's words: its options, a box name, and what a usage error says about them (the
//! nearest option, the form help gives a value in).

use crate::cat;
use crate::exit::{Ctx, Exit, Res};
use crate::shell::strip_nl;

/// The options of each command ported here, as bin/omabox's CMD_OPTS has them: `=` takes a value,
/// `?` may take one.
fn cmd_opts(cmd: &[u8]) -> Option<&'static str> {
    match cmd {
        b"path" => Some("-b= --box= --logs --run --home --json"),
        b"env" => Some("-b= --box="),
        b"which" => Some("--json"),
        _ => None,
    }
}

/// The first paragraph of `omabox help CMD` (usage_text's lines), for opt_form.
fn usage_head(cmd: &[u8]) -> &'static str {
    match cmd {
        b"path" => {
            "  omabox path [NAME | -b NAME] [--home | --run | --logs [--json]]   the box dir; --home: its /home/sbx (<dir>/home);\n                                                         --run: its /run/user/$UID (<dir>/run); --logs: each log's file"
        }
        b"env" => {
            "  omabox env [NAME | -b NAME]                            exports for host-side Wayland tools"
        }
        b"which" => {
            "  omabox which [--json] PID                              the box a host process is in, or \"not in a box\" (exit 1)"
        }
        _ => "",
    }
}

/// The form help gives OPT's value in, as ": OPT VALUE" (": -g \"X,Y WxH\""), from the command's
/// lines in `omabox help CMD`; nothing when help shows none.
pub fn opt_form(main_cmd: &[u8], opt: &str) -> String {
    if main_cmd.is_empty() {
        return String::new();
    }
    let count = |s: &str, c: char| s.matches(c).count();
    for line in usage_head(main_cmd).lines() {
        if line.is_empty() {
            break;
        }
        let f: Vec<&str> = line.split_ascii_whitespace().collect();
        let mut i = 0;
        while i + 1 < f.len() {
            if f[i].trim_start_matches(['[', '(']) != opt {
                i += 1;
                continue;
            }
            let mut v = f[i + 1].to_string();
            if v.starts_with(['-', '[', '|', '(']) {
                i += 1;
                continue;
            }
            if v.starts_with('"') {
                while !(v.len() > 1 && v[1..].contains('"')) && i + 2 < f.len() {
                    i += 1;
                    v = format!("{v} {}", f[i + 1]);
                }
            }
            if let Some(s) = v.strip_suffix("...") {
                v = s.to_string();
            }
            while v.ends_with([']', ')', '|']) {
                if v.ends_with(']') && count(&v, '[') >= count(&v, ']') {
                    break;
                }
                if v.ends_with(')') && count(&v, '(') >= count(&v, ')') {
                    break;
                }
                v.pop();
            }
            return format!(": {opt} {v}");
        }
    }
    String::new()
}

/// Levenshtein distance with a swap of two neighbours as one edit.
fn dist(a: &[char], b: &[char]) -> usize {
    let (la, lb) = (a.len(), b.len());
    let mut d = vec![vec![0usize; lb + 1]; la + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, x) in d[0].iter_mut().enumerate() {
        *x = j;
    }
    for i in 1..=la {
        for j in 1..=lb {
            let mut x = (d[i - 1][j] + 1).min(d[i][j - 1] + 1);
            x = x.min(d[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                x = x.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = x;
        }
    }
    d[la][lb]
}

/// " (did you mean X?)" for the CANDIDATE nearest WORD: the only one it is the start of, or the
/// only one nearest and a typo away (a letter wrong, missing or extra, or two swapped; two typos past
/// 4 letters, three past 10). Nothing otherwise.
pub fn did_you_mean(word: &str, candidates: &[&str]) -> String {
    if word.is_empty() || candidates.is_empty() {
        return String::new();
    }
    let mut c: Vec<&str> = Vec::new();
    for k in candidates {
        if !k.is_empty() && !c.contains(k) {
            c.push(k);
        }
    }
    let lw = word.to_lowercase();
    let core = lw.trim_start_matches('-');
    if core.chars().count() >= 2 {
        let starts: Vec<&&str> = c
            .iter()
            .filter(|k| k.starts_with(&lw) && **k != lw)
            .collect();
        if let [one] = starts.as_slice() {
            return format!(" (did you mean {one}?)");
        }
    }
    let n = lw.chars().count();
    let t = if n <= 4 {
        1
    } else if n <= 10 {
        2
    } else {
        3
    };
    let w: Vec<char> = lw.chars().collect();
    let (mut best, mut bk, mut ties) = (t + 1, "", 0);
    for k in &c {
        let d = dist(&w, &k.chars().collect::<Vec<_>>());
        if d < best {
            best = d;
            bk = k;
            ties = 0;
        } else if d == best {
            ties += 1;
        }
    }
    if best <= t && best > 0 && ties == 0 {
        format!(" (did you mean {bk}?)")
    } else {
        String::new()
    }
}

/// An option the command does not take: a usage error, with the nearest one it does when one is
/// close. LABEL is what the message starts with, its first word the command.
pub fn unknown_opt(ctx: &Ctx, label: &str, opt: &[u8]) -> Exit {
    let cmd = label.split(' ').next().unwrap_or_default();
    let mut known: Vec<&str> = cmd_opts(cmd.as_bytes())
        .map(|o| {
            o.split_ascii_whitespace()
                .map(|w| w.strip_suffix(['=', '?']).unwrap_or(w))
                .collect()
        })
        .unwrap_or_default();
    known.extend(["-h", "--help"]);
    let word = String::from_utf8_lossy(opt.split(|&c| c == b'=').next().unwrap_or_default());
    ctx.usage_die(cat!(
        label,
        ": unknown option ",
        opt,
        did_you_mean(&word, &known)
    ))
}

/// `optarg "$@"`: an option with no value is a usage error naming the form help gives it.
fn optarg(ctx: &Ctx, opt: &[u8], value: Option<&Vec<u8>>) -> Res<()> {
    if value.is_some_and(|v| !v.is_empty()) {
        return Ok(());
    }
    let o = String::from_utf8_lossy(opt);
    Err(ctx.usage_die(cat!(
        ctx.label(),
        opt,
        " needs a value",
        opt_form(&ctx.main_cmd, &o)
    )))
}

/// NAME, -b NAME, or nothing (the default box): for commands that take nothing else.
pub fn name_arg(ctx: &Ctx, cmd: &str, args: &[Vec<u8>]) -> Res<Vec<u8>> {
    let mut name: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == b"-b" || a == b"--box" {
            optarg(ctx, a, args.get(i + 1))?;
            name = args[i + 1].clone();
            i += 1;
        } else if a.starts_with(b"-") {
            return Err(unknown_opt(ctx, cmd, a));
        } else {
            if !name.is_empty() {
                return Err(ctx.die(cat!(cmd, ": one box name, got ", name, " and ", a)));
            }
            name = a.clone();
        }
        i += 1;
    }
    Ok(strip_nl(&name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_from_help() {
        assert_eq!(opt_form(b"path", "-b"), ": -b NAME");
        assert_eq!(opt_form(b"env", "-b"), ": -b NAME");
        assert_eq!(opt_form(b"path", "--box"), "");
        assert_eq!(opt_form(b"", "-b"), "");
        assert_eq!(opt_form(b"which", "--json"), "");
    }

    #[test]
    fn nearest() {
        let o = [
            "-b", "--box", "--logs", "--run", "--home", "--json", "-h", "--help",
        ];
        assert_eq!(did_you_mean("--lgs", &o), " (did you mean --logs?)");
        assert_eq!(did_you_mean("--lo", &o), " (did you mean --logs?)");
        assert_eq!(did_you_mean("--h", &o), " (did you mean -h?)");
        assert_eq!(did_you_mean("--hme", &o), " (did you mean --home?)");
        assert_eq!(did_you_mean("--Logs", &o), "");
        assert_eq!(did_you_mean("--jsno", &o), " (did you mean --json?)");
        assert_eq!(did_you_mean("--zzzzzz", &o), "");
        assert_eq!(did_you_mean("-x", &o), "");
        assert_eq!(did_you_mean("", &o), "");
    }
}
