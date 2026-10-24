//! omabox's Rust half (#324). bin/omabox hands it the commands it has taken over, with the words as
//! bash's main has them by then: `-b NAME` moved after the command, `--opt=value` split, `--help`,
//! the relay and the broker already dealt with (OMABOX_ROOT set). Called straight (build/omabox),
//! it runs that front itself (front.rs, wave 6-front).
mod args;
mod boxes;
mod broker_side;
mod bwrap;
mod cmd;
mod conf;
mod desk;
mod deskwin;
mod enter;
mod exec;
mod exit;
mod front;
mod gpu;
mod guard;
mod help;
mod host;
mod hypr;
mod input;
mod install;
mod jail;
mod jailpath;
mod jobs;
mod jq;
mod jqprog;
mod json;
mod layer;
mod logs;
mod marks;
mod mon;
mod net;
mod num;
mod paths;
mod plugins;
mod procfs;
mod ptr;
mod ready;
mod recprog;
mod relay;
mod render;
mod repo;
mod screen;
mod seed;
mod select;
mod settle;
mod shell;
mod shelljson;
mod shellnote;
mod shots;
mod sig;
mod trap;
mod upspec;
mod win;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::ExitCode;

/// OMABOX_ROOT and OMABOX_CALL_US as bash's hand-off set them.
static HANDOFF: std::sync::OnceLock<(Option<OsString>, Option<OsString>)> =
    std::sync::OnceLock::new();

type Run = fn(&mut exit::Ctx, &[Vec<u8>]) -> exit::Res<()>;

fn main() -> ExitCode {
    // As bash: a write to a closed pipe ends the command (Rust ignores SIGPIPE), unless SIGPIPE came
    // ignored: bash keeps it so, for itself and what it runs (sig::command gives the children it
    // back: std's spawn resets it). No setlocale: text is UTF-8 and lists byte order in every locale
    // (findings 445-447).
    if !sig::pipe_ignored() {
        // SAFETY: at the start, before any other thread; SIG_DFL is a valid disposition.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
    // The native box entry for a command line script runs (exec.rs's box_pty): before anything else,
    // as nsenter was (one thread, nothing caught).
    if std::env::args_os().nth(1).is_some_and(|a| a == "_enter") {
        let rest: Vec<OsString> = std::env::args_os().skip(2).collect();
        return enter::main(&rest);
    }
    // Bash ignores SIGQUIT for itself (a Ctrl-\ does not end the script), and what it runs in the
    // foreground gets the default back: a handler that does nothing is that (reset at exec). One
    // that came ignored stays ignored, for it and its children (sig::catch leaves it).
    sig::catch(libc::SIGQUIT, quit_ignored);
    sig::set_main();
    // What bash's hand-off adds to the environment is this call's, not its children's (bin/omabox up
    // and down, a box's command, git): kept here and taken out, before any thread.
    // (Called straight, the call's time is now: bash's CALL_US, taken before anything else.)
    let handed = std::env::var_os("OMABOX_ROOT").is_some();
    let _ = HANDOFF.set((
        std::env::var_os("OMABOX_ROOT"),
        if handed {
            std::env::var_os("OMABOX_CALL_US")
        } else {
            Some(OsString::from_vec(marks::epoch_us()))
        },
    ));
    let _ = ROOT.set(find_root());
    exec::caller_environ_take();
    // SAFETY: at the start, before any other thread reads the environment.
    unsafe {
        std::env::remove_var("OMABOX_ROOT");
        std::env::remove_var("OMABOX_CALL_US");
        std::env::remove_var("OMABOX_ENVIRON_FD");
        // (bash's: this call's only, not what it starts; called straight, the binary is the call)
        std::env::remove_var("OMABOX_BASH");
    }
    let mut args = std::env::args_os().skip(1);
    let (cmd, rest): (OsString, Vec<OsString>) = if handed {
        (args.next().unwrap_or_default(), args.collect())
    } else {
        // BOXES: bash's first line refuses a call without XDG_RUNTIME_DIR, whatever it asks.
        let runtime = shell::env_or_empty("XDG_RUNTIME_DIR");
        if runtime.is_empty() {
            exit::say("XDG_RUNTIME_DIR is not set");
            return ExitCode::from(1);
        }
        let argv: Vec<Vec<u8>> = args.map(OsString::into_vec).collect();
        match front::front(&cat!(runtime, "/omabox"), &argv) {
            Ok(Some(c)) => (
                OsString::from_vec(c.cmd),
                c.args.into_iter().map(OsString::from_vec).collect(),
            ),
            Ok(None) => return ExitCode::SUCCESS,
            Err(exit::Exit(rc)) => return ExitCode::from(rc),
        }
    };
    let ported: Option<Run> = match cmd.to_str() {
        Some("path") => Some(cmd::path::run),
        Some("env") => Some(cmd::env::run),
        Some("which") => Some(cmd::which::run),
        Some("hyprctl") => Some(cmd::hyprctl::run),
        Some("lua") => Some(cmd::lua::run),
        Some("run") => Some(cmd::run::run),
        Some("log" | "logs") => Some(cmd::log::run),
        Some("events") => Some(cmd::events::run),
        Some("wait") => Some(cmd::wait::run),
        Some("windows") => Some(cmd::windows::run),
        Some("tree") => Some(cmd::tree::run),
        Some("pixel") => Some(cmd::pixel::run),
        Some("keys") => Some(cmd::keys::run),
        Some("click") => Some(cmd::click::run),
        Some("scroll") => Some(cmd::click::scroll),
        Some("pointer") => Some(cmd::pointer::run),
        Some("drag") => Some(cmd::drag::run),
        Some("shot" | "screenshot") => Some(cmd::shot::run),
        Some("calibrate") => Some(cmd::calibrate::run),
        Some("keys-to-box") => Some(cmd::keys_to_box::run),
        Some("monitor") => Some(cmd::monitor::run),
        Some("output") => Some(cmd::output::run),
        Some("mode") => Some(cmd::mode::run),
        Some("gpu") => Some(cmd::gpu::run),
        Some("cpu") => Some(cmd::cpu::run),
        Some("audio") => Some(cmd::audio::run),
        Some("ports") => Some(cmd::ports::run),
        Some("clip") => Some(cmd::clip::run),
        Some("peek") => Some(cmd::peek::run),
        Some("gdb") => Some(cmd::gdb::run),
        Some("check") => Some(cmd::check::run),
        Some("record") => Some(cmd::record::run),
        Some("up") => Some(cmd::up::run),
        Some("down") => Some(cmd::down::run),
        Some("_reap") => Some(cmd::reap::run),
        Some("ls" | "list") => Some(cmd::ls::run),
        Some("save") => Some(cmd::save::save),
        Some("saves") => Some(cmd::save::saves),
        Some("version" | "-V" | "--version") => Some(cmd::version::run),
        Some("config") => Some(cmd::config::run),
        Some("brief") => Some(cmd::brief::run),
        Some("host") => Some(cmd::host::run),
        Some("broker") => Some(cmd::broker::run),
        Some("guard") => Some(cmd::guard::run),
        Some("setup") => Some(cmd::setup::run),
        Some("reload") => Some(cmd::reload::run),
        Some("restart-shell") => Some(cmd::restart_shell::run),
        Some("help" | "-h" | "--help") => Some(|_, a| help::run(a)),
        _ => None,
    };
    // BOXES: bash refuses a call without XDG_RUNTIME_DIR before it hands one over.
    let runtime = shell::env_or_empty("XDG_RUNTIME_DIR");
    if let (Some(run), false) = (ported, runtime.is_empty()) {
        let words: Vec<Vec<u8>> = rest.into_iter().map(OsString::into_vec).collect();
        let mut ctx = exit::Ctx::new(cmd.as_encoded_bytes(), cat!(runtime, "/omabox"));
        let r = run(&mut ctx, &words);
        // (shot --no-pointer's hold, given back however the command ended: bash's EXIT trap)
        ptr::release_at_exit();
        return match r {
            Ok(()) => ExitCode::SUCCESS,
            Err(exit::Exit(rc)) => ExitCode::from(rc),
        };
    }
    match cmd.to_str() {
        // The suite's proof that the hand-off reaches this binary, and with what.
        Some("_rust") => {
            let words: Vec<String> = rest
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            println!("rust {}", words.join(" "));
            ExitCode::SUCCESS
        }
        // The suite's check that the options table here is bash's CMD_OPTS (t_unit_rust_front).
        Some("_rust-opts") => match rest
            .first()
            .and_then(|c| args::cmd_opts(c.as_encoded_bytes()))
        {
            Some(t) => {
                println!("{t}");
                ExitCode::SUCCESS
            }
            None => ExitCode::from(1),
        },
        // The suite's check that the broker reads a jail as bash does (broker_side::probe).
        Some("_rust-jail") => {
            let words: Vec<Vec<u8>> = rest.into_iter().map(OsString::into_vec).collect();
            ExitCode::from(broker_side::probe(&words))
        }
        // A signal's release of shot --no-pointer's hold (ptr::hold_until_exit), in this process.
        Some("_ptr-release") => {
            let words: Vec<Vec<u8>> = rest.into_iter().map(OsString::into_vec).collect();
            ExitCode::from(ptr::release_main(&words))
        }
        // The suite's check that the jq programs here are bash's, byte for byte.
        // record's programs (recprog.rs), each as it stands quoted in bin/omabox.
        Some("_rust-jq") if rest.first().is_some_and(|a| a == "--recprog") => {
            for p in recprog::ALL {
                print!("{p}\0");
            }
            ExitCode::SUCCESS
        }
        Some("_rust-jq") if rest.first().is_some_and(|a| a == "--inline") => {
            for p in jqprog::INLINE
                .iter()
                .chain(jqprog::INLINE_4B)
                .chain(recprog::ALL)
                .chain(install::JQ_INLINE)
                .chain(plugins::JQ_INLINE)
                .chain(ready::JQ_INLINE)
                .chain(shelljson::JQ_INLINE)
                .chain(cmd::ls::JQ_INLINE)
                .chain(cmd::down::JQ_INLINE)
                .chain(cmd::brief::JQ_INLINE)
                .chain(guard::JQ_INLINE)
            {
                print!("{p}\0");
            }
            ExitCode::SUCCESS
        }
        Some("_rust-jq") => match rest
            .first()
            .and_then(|n| jqprog::by_name(&n.to_string_lossy()))
        {
            Some(p) => {
                print!("{p}");
                ExitCode::SUCCESS
            }
            None => ExitCode::from(2),
        },
        // A command bash handed here that this build does not have: a binary older than its bash
        // (a pull without ./install.sh). Bash runs it, told not to hand it back. Called straight,
        // the front's: a command omabox does not have.
        _ if handed => back_to_bash(cmd, rest),
        _ => {
            let exit::Exit(rc) = help::unknown_command(cmd.as_encoded_bytes());
            ExitCode::from(rc)
        }
    }
}

extern "C" fn quit_ignored(_: libc::c_int) {}

/// ROOT: the checkout (or install) this binary belongs to, found once at the start, before anything
/// could replace the file: OMABOX_ROOT from bash's hand-off, else the parent of the directory the
/// binary is in (build/omabox, or bin/omabox once that is the binary: D1).
static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn find_root() -> PathBuf {
    if let Some((Some(r), _)) = HANDOFF.get() {
        return PathBuf::from(r);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub fn root() -> PathBuf {
    ROOT.get_or_init(find_root).clone()
}

/// Whether PATH is this very binary (the same file, through any link).
fn is_this_binary(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(path), std::fs::metadata("/proc/self/exe")) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

fn back_to_bash(cmd: OsString, rest: Vec<OsString>) -> ExitCode {
    let script = root().join("bin/omabox");
    // Only to a bash omabox that handed the call here: bin/omabox as this binary would hand it
    // straight back, for ever (proposal D7's M3).
    if is_this_binary(&script) {
        exit::say(cat!(
            "this omabox has no command ",
            cmd.as_encoded_bytes(),
            ", and no bash omabox to hand it to (",
            shell::bytes(&script),
            " is this binary)"
        ));
        return ExitCode::from(126);
    }
    let mut c = crate::sig::command(&script);
    c.arg(cmd).args(rest);
    // This process's environment, but where bash's globals overwrote a variable the caller exported
    // under the same name (ROOT, BOXES, ...): the caller's value (finding 74). What the broker set
    // (OMABOX_JAIL) or took out (its peer's pidfd, closed by now) stays as it is.
    if let Some(env) = exec::caller_environ() {
        for e in env.split(|&b| b == 0).filter(|e| !e.is_empty()) {
            let i = e.iter().position(|&b| b == b'=').unwrap_or(e.len());
            let (k, v) = (
                OsString::from_vec(e[..i].to_vec()),
                OsString::from_vec(e.get(i + 1..).unwrap_or_default().to_vec()),
            );
            if std::env::var_os(&k).is_some_and(|now| now != v) {
                c.env(k, v);
            }
        }
    }
    c.env("OMABOX_BASH", "1");
    if let Some((_, Some(us))) = HANDOFF.get() {
        c.env("OMABOX_CALL_US", us);
    }
    let err = c.exec();
    eprintln!("omabox: could not run {}: {err}", script.display());
    ExitCode::from(126)
}

/// CALL_US: when this omabox was called, in µs (bash's hand-off says it; now when it does not).
pub fn call_us() -> Vec<u8> {
    if let Some((_, Some(us))) = HANDOFF.get() {
        let b = us.as_encoded_bytes();
        if !b.is_empty() && b.iter().all(u8::is_ascii_digit) {
            return b.to_vec();
        }
    }
    marks::epoch_us()
}
