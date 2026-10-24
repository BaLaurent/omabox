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
        b"down" => Some("-b= --box= --all --orphans"),
        b"ls" | b"list" | b"saves" | b"--version" => Some("--json"),
        b"save" => Some("-b= --box= --force"),
        b"run" => Some(concat!(
            "-b= --box= -d --detach -q --print-log --replace --pass= --env-file= ",
            "--wait --start= --quiet= --timeout= --strict --json --ignore= -g= ",
            "--size= --plugin= --overlay= --ro-bind= --seed= --no-seed= --monitor= --theme-dir= ",
            "--theme= --net= --allow= --addr= --env= --idle= --from= --hyprland= --quickshell= ",
            "--omarchy= --keep= --stock-bar --clean --desk-shell --systemd --audio --xwayland ",
            "--autoreload --no-shell --no-pointer"
        )),
        b"hyprctl" => Some("-b= --box="),
        b"lua" => Some("-b= --box= --json"),
        b"log" | b"logs" => Some("-b= --box= -f --follow --crash --grep= -i -n="),
        b"events" => Some(
            "-b= --box= --since= --mark? --grep= -i --json -f --follow --until= --timeout= -n=",
        ),
        b"wait" => Some(
            "-b= --box= --json --ignore= --timeout= --quiet= --strict -g= -w= --window= --active --gone --focused",
        ),
        b"windows" => Some("-b= --box= --json"),
        b"tree" => Some("-b= --box= -w= --window= --active -l= --layer= --find= --json --all"),
        b"pixel" => Some("-b= --box= -w= --window= --active --in= --json"),
        b"keys" => Some(
            "-b= --box= -w= --window= --active -t= -s= --pass= --wait --start= --quiet= --timeout= --strict --json --ignore= -g=",
        ),
        b"keys-to-box" => Some("-b= --box="),
        b"click" => Some(
            "-b= --box= -w= --window= --active --in= --no-raise --double --steps= --glide= --mod= --wait --start= --quiet= --timeout= --strict --json --ignore= -g=",
        ),
        b"scroll" => Some(
            "-b= --box= -w= --window= --active --in= --no-raise --mod= --source= --wait --start= --quiet= --timeout= --strict --json --ignore= -g=",
        ),
        b"pointer" => Some("-b= --box= -w= --window= --active --in= --steps= --glide= --mod="),
        b"drag" => Some(
            "-b= --box= -w= --window= --active --in= --no-raise --steps= --glide= --hold= --shot= --shot-fit= --shot-g= --shot-monitor= --mod= --wait --start= --quiet= --timeout= --strict --json --ignore= -g=",
        ),
        b"shot" | b"screenshot" => Some(
            "-b= --box= -o= --output= --no-pointer -g= -w= --window= --active --monitor= --fit= --zoom= --burst= --every= --sheet --diff --changed --since= --ignore= --after --json",
        ),
        b"calibrate" => Some("-b= --box= -o= --output= --size= --answer= --json"),
        b"mode" => Some("-b= --box= --json"),
        b"output" => Some("-b= --box= --for= --cycles="),
        b"monitor" => Some("-b= --box= --name= --json"),
        b"gpu" => Some("-b= --box= --json"),
        b"cpu" => Some("-b= --box= --settle= --pid= --app= --threads --json"),
        b"audio" => Some("-b= --box= --json"),
        b"ports" => Some("-b= --box= --json"),
        b"clip" => Some("-b= --box= --from-box"),
        b"peek" => Some("-b= --box= --fps= --workspace= --focus --monitor="),
        b"gdb" => Some("-b= --box= --shell --pid= --watch"),
        b"record" => Some(concat!(
            "-b= --box= -o= --output= --monitor= --no-pointer --audio --glide= --show-input --captions --cut-idle? --fps= ",
            "--hold= --for= --sheet --frames= --every= --fit= --json"
        )),
        b"check" => Some(concat!(
            "-b= --box= --approve --runs= --base= -o= --output= --speck= --ignore= --report= -w= --window= --active -g= ",
            "--timeout= --json --size= --plugin= --overlay= --ro-bind= --seed= --no-seed= --monitor= --theme-dir= --theme= --net= --allow= --addr= --env= --idle= --from= --hyprland= --quickshell= --omarchy= --keep= --stock-bar --clean --desk-shell --systemd --audio --xwayland --autoreload --no-shell --no-pointer"
        )),
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
        b"down" => {
            "  omabox down [NAME | -b NAME | --all | --orphans]       --orphans: the boxes ls calls orphans (no idle limit, starter gone)"
        }
        b"save" => {
            "  omabox save SAVE [-b NAME] [--force]                   keep the box's HOME: up/run --from SAVE start from it"
        }
        b"run" => RUN_HEAD,
        // (up's lines are run's after its own two: its UP OPTIONS)
        b"up" => RUN_HEAD
            .find("  omabox up ")
            .map_or(RUN_HEAD, |i| &RUN_HEAD[i..]),
        b"hyprctl" => "  omabox hyprctl [-b NAME] ARGS...",
        b"lua" => {
            "  omabox lua [-b NAME] [--json] (EXPR | -)               Lua in the box's Hyprland, printing what it returns"
        }
        b"log" | b"logs" => LOG_HEAD,
        b"events" => EVENTS_HEAD,
        b"wait" => {
            "  omabox wait [-b NAME] [--timeout 10s] [--json] COND   until COND holds (exit 0), 124 if not in time\n         COND: still [--quiet 300ms] [-g GEOM | --window SEL] [--ignore GEOM]... [--strict]\n               | change [-g GEOM | --window SEL] [--ignore GEOM]...\n               | window SEL [--gone | --focused] | layer NAMESPACE [--gone] | until -- CMD [ARG...]"
        }
        b"windows" => {
            "  omabox windows [-b NAME] [--json]                      the box's windows, where they are, what covers them"
        }
        b"tree" => {
            "  omabox tree [-b NAME] [--window SEL | --layer SEL] [--find TEXT] [--json] [--all]   a window's (or a layer's) accessibility tree as text"
        }
        b"pixel" => {
            "  omabox pixel [-b NAME] [--window SEL | --active | --in SHOT] [--json] X Y [X Y]...   the colour there, #rrggbb"
        }
        b"keys" => {
            r#"  omabox keys [-b NAME] [--window SEL] [--wait] (COMBO | -t TEXT | --pass VAR | -s DURATION | down:KEY | up:KEY)...  e.g. super+space  -t hi  Return"#
        }
        b"keys-to-box" => {
            r#"  omabox keys-to-box [-b NAME] [on|off]                  interactive: SUPER keys to the box while focused, pointer on it"#
        }
        b"click" => {
            r#"  omabox click [-b NAME] [--window SEL [--no-raise] | --in SHOT] [--wait] X Y [BTN] [--double]
               [--steps N | --glide DURATION] [--mod MODS]  --steps: travel there; --glide: in that time; --mod ctrl: ctrl-click"#
        }
        b"scroll" => {
            r#"  omabox scroll [-b NAME] [--window SEL [--no-raise] | --in SHOT] [--wait] X Y DY [DX] [--source wheel|finger|continuous|tilt]
                [--mod MODS]
                                                         scroll there (DX: sideways; 15 is a wheel notch)"#
        }
        b"pointer" => {
            r#"  omabox pointer [-b NAME] [--window SEL | --in SHOT] [--steps N | --glide DURATION] [--mod MODS] -- ARGS...
                 raw: move X Y [--steps N], click/down/up [BTN], scroll DY, hscroll DX,
                 source wheel|finger|continuous|tilt, sleep DURATION
  omabox pointer [-b NAME] hide | show                   the pointer out of shots and recordings (it still moves), over reloads"#
        }
        b"drag" => {
            r#"  omabox drag [-b NAME] [--window SEL [--no-raise] | --in SHOT] [--wait | --shot FILE] X1 Y1 X2 Y2 [BTN]
              [--steps 10 | --glide DURATION] [--hold DURATION] [--mod MODS]  press, move, hold, release
              [--shot-fit N | --shot-g "X,Y WxH" | --shot-monitor NAME]   (with --shot: as shot's --fit, -g, --monitor)"#
        }
        b"shot" => {
            r#"  omabox shot [-b NAME] [-o FILE | FILE] [--window SEL | --active] [-g "X,Y WxH"] [--fit N | --zoom N] [--no-pointer] [--json]
              [--changed [--since SHOT] [--ignore "X,Y WxH"]...]    only what changed since the last shot of it
              [--burst N [--every DURATION] [--diff] [--sheet] [-o DIR] [--after -- ACTION...]]   frames over time"#
        }
        b"screenshot" => {
            r#"  omabox shot [-b NAME] [-o FILE | FILE] [--window SEL | --active] [-g "X,Y WxH"] [--fit N | --zoom N] [--no-pointer] [--json]
              [--changed [--since SHOT] [--ignore "X,Y WxH"]...]    only what changed since the last shot of it
              [--burst N [--every DURATION] [--diff] [--sheet] [-o DIR] [--after -- ACTION...]]   frames over time"#
        }
        b"calibrate" => {
            r#"  omabox calibrate [-b NAME] [--size WxH] [CARD]        a test card to read as a shot: do your clicks need --fit?
  omabox calibrate [CARD] --answer "A X,Y B X,Y ..." [--json]   where you see each mark: 1:1, or the --fit N that maps it"#
        }
        b"mode" => {
            "  omabox mode [-b NAME] [--json | WxH[@HZ][,scale=S]|host]   show or change a headless box's screen mode"
        }
        b"output" => {
            "  omabox output [-b NAME] (drop [OUT] [--for DURATION] [--cycles N] | back [OUT])   a screen gone (a monitor off), and back"
        }
        b"monitor" => {
            "  omabox monitor [-b NAME] (add SPEC [--name N] | remove NAME | list [--json])   a box's other monitors"
        }
        b"gpu" => {
            "  omabox gpu [-b NAME] [DURATION] [--json]               GPU engine time of this box's processes (default 5s)\n  omabox gpu release GPU                                 take down the headless boxes rendering on that GPU"
        }
        b"cpu" => {
            "  omabox cpu [-b NAME] [DURATION] [--settle DURATION] [--pid PID | --app NAME] [--threads] [--json]\n                                                         CPU (% of one core) and memory of this box's processes (default 5s)"
        }
        b"audio" => {
            "  omabox audio [-b NAME] [DURATION] [--json]             an up --audio box's sound for DURATION (3s): when, how loud"
        }
        b"ports" => {
            "  omabox ports [-b NAME] [--json]                        boxes' TCP servers, and which of them your 127.0.0.1 reaches"
        }
        b"clip" => {
            "  omabox clip [-b NAME] [--from-box]                     your clipboard into an interactive box, or back"
        }
        b"peek" => {
            r#"  omabox peek [-b NAME] [--monitor NAME] [--fps N] [--focus] [--workspace WS]  live view-only window of a box (workspace 9)"#
        }
        b"gdb" => {
            r#"  omabox gdb [-b NAME] [--shell | --pid PID] [--watch | -- GDB ARGS]   a backtrace of the box's Hyprland (or shell)"#
        }
        b"check" => {
            r#"  omabox check run [--approve [--runs 3] | --json] [--base DIR] [-o DIR] [--speck N] [UP OPTIONS] -- SCENARIO [ARG...]
                                                         a visual check: the scenario's checkpoints against a baseline
  omabox check point ID [-b NAME] [--window SEL] [-g "X,Y WxH"] [--ignore "X,Y WxH"]... [--timeout 15s] [-o DIR]
                                                         a checkpoint, in a scenario: once still, a shot (and the tree)
  omabox check diff A.png B.png [--ignore "X,Y WxH"]... [--speck 32] [-o MARKED.png] [--report R.png] [--json]
                                                         what differs, as a check sees it"#
        }
        b"record" => {
            r#"  omabox record [-b NAME] (start [-o FILE] [--monitor NAME|all] [--fps 30] [--no-pointer] [--audio] [--glide DURATION]
                | mark TEXT [--hold DURATION] | hold DURATION | stop [SHEET] [POLISH] | status [--json])   a video of the screen
  omabox record [-b NAME] [-o FILE] [--monitor NAME|all] [--fps 30] [--hold 1s] [--no-pointer] [--audio] [--glide DURATION]
                [SHEET] [POLISH] -- ACTION...   one action, recorded
  omabox record [-b NAME] [-o FILE] [--monitor NAME|all] [--fps 30] [--no-pointer] [--audio] [SHEET] [POLISH]
                --for DURATION   the screen for that long, no action
  omabox record [-b NAME] sheet VIDEO [-o PNG] [--frames 15 | --every DURATION] [--fit N]   a contact sheet of a video
              SHEET: --sheet [--frames 15 | --every DURATION] [--fit N]   the recording's contact sheet, FILE.sheet.png
              POLISH: [--show-input] [--captions] [--cut-idle [1s]]   clicks, pointer, keys and marks drawn in; still stretches cut"#
        }
        _ => "",
    }
}

/// run's help shows up's lines too: its UP OPTIONS are up's, for a throwaway box.
const RUN_HEAD: &str = "  omabox run [-b NAME] [-d [--wait] [--replace] [-q] [--print-log]] [--pass VAR]... [--env-file FILE]...\n             [UP OPTIONS] -- CMD [ARG...]                UP OPTIONS: up's, for a throwaway box only\n  omabox up [NAME] [--size WxH[@HZ][,scale=S]|host] [--interactive] [--plugin PATH|ID]... [--overlay DIR]...\n            [--ro-bind DIR[:DEST]]... [--seed SRC:DEST]... [--net isolated [--allow PORT,...] [--addr ADDR]...]\n            [--env KEY=VAL]...\n            [--no-seed extensions,fonts]                 your plugins' settings files, your fonts left on the desk\n            [--theme-dir DIR]...                         a theme you are editing, live, in the box's themes/\n            [--theme NAME]                               start on that theme, not your desktop's current one\n            [--monitor WxH[@HZ][,scale=S][,right|below|X,Y]]...     more monitors\n            [--idle DURATION] [--stock-bar] [--systemd] [--xwayland] [--autoreload] [--no-shell] [--from SAVE]\n            [--desk-shell]                               your desktop's whole shell: every plugin, shell.json, their files\n            [--audio]                                    its own PipeWire: a speaker nobody hears, a silent mic\n            [--no-pointer]                               the pointer hidden from the start (omabox pointer show)\n            [--clean [--keep animations,bar,clock,fonts,locale,pointer,wallpaper]]   only what is tested moves\n            [--hyprland PATH]                            a Hyprland build of yours instead of the installed one\n            [--quickshell PATH]                          a Quickshell build of yours for the box's shell\n            [--omarchy DIR]                              an Omarchy tree (a checkout) instead of /usr/share/omarchy\n            [--workspace WS] [--confirm-close | --no-confirm-close]      (interactive)\n            [--new]                                      start it under a free name, box-1, box-2, ... (printed)\n            [--json]                                     the box as ls --json lists it, on stdout";
const LOG_HEAD: &str = "  omabox log [-b NAME] [LOG...|all] [-n 100|all] [--grep RE [-i]] [-f]\n                                                         a box's logs: hyprland (the default), shell, apps, run,\n                                                         events, keyring, labwc, systemd, audio, gdb,\n                                                         journal, box, reap\n  omabox log [-b NAME] gdb --crash                       only the backtrace of the thread a crash hit";
const EVENTS_HEAD: &str = "  omabox events [-b NAME] [--since MARK] [--grep RE [-i]] [-n N] [--json]   Hyprland's events\n  omabox events [-b NAME] --mark [MARK] [--json]         from here: prints the offset, keeps MARK for --since\n  omabox events [-b NAME] [--since MARK] (--until RE [--timeout 10s] | -f) [--json]";

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
    unknown_opt_hint(ctx, label, opt, "")
}

/// unknown_opt, the message ending in HINT (when there is one).
pub fn unknown_opt_hint(ctx: &Ctx, label: &str, opt: &[u8], hint: &str) -> Exit {
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
    let hint = if hint.is_empty() {
        String::new()
    } else {
        format!(" {hint}")
    };
    ctx.usage_die(cat!(
        label,
        ": unknown option ",
        opt,
        did_you_mean(&word, &known),
        hint
    ))
}

/// A word that is not one of CHOICES where one of them goes: "LABEL: unknown WHAT WORD (did you
/// mean X?) TEXT", a usage error.
pub fn unknown_word(
    ctx: &Ctx,
    label: &str,
    what: &str,
    w: &[u8],
    text: &str,
    choices: &[&str],
) -> Exit {
    let near = did_you_mean(&String::from_utf8_lossy(w), choices);
    let text = if text.is_empty() {
        String::new()
    } else {
        format!(" {text}")
    };
    ctx.usage_die(cat!(label, ": unknown ", what, " ", w, near, text))
}

/// A word with no place in LABEL's command line: a usage error. One that names a box says the box
/// goes with -b (audit E4).
pub fn unexpected(ctx: &Ctx, label: impl AsRef<[u8]>, w: &[u8], hint: &str) -> Exit {
    let mut why = hint.as_bytes().to_vec();
    if crate::shell::re_match("^[A-Za-z0-9][A-Za-z0-9_.-]{0,39}$", w) && ctx.in_boxes(w).is_dir() {
        why = cat!("(box '", w, "'? a box goes with -b: -b ", w, ")");
    }
    let sep: &[u8] = if why.is_empty() { b"" } else { b" " };
    ctx.usage_die(cat!(label.as_ref(), ": unexpected ", w, sep, why))
}

/// `optarg "$@"`: an option with no value is a usage error naming the form help gives it.
pub fn optarg(ctx: &Ctx, opt: &[u8], value: Option<&Vec<u8>>) -> Res<()> {
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

/// `usage CMD` (a command's --help after its other words): its help as `omabox help CMD` prints it,
/// its status the command's. The help text is bash's until the front is ported (#324, wave 6).
pub fn usage(cmd: &str) -> Res<()> {
    let mut c = crate::sig::omabox_cmd();
    c.args(["help", cmd]);
    let rc = crate::sig::run_status(c).unwrap_or(126);
    crate::cmd::hyprctl::code(rc)
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
