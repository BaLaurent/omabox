//! omabox's Rust half (#324). bin/omabox hands it the commands it has taken over, with the words as
//! bash's main has them by then: `-b NAME` moved after the command, `--opt=value` split, `--help`,
//! the relay and the broker already dealt with. Every other command stays in bash.

mod args;
mod boxes;
mod cmd;
mod exit;
mod jail;
mod jobs;
mod json;
mod procfs;
mod select;
mod shell;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

type Run = fn(&mut exit::Ctx, &[Vec<u8>]) -> exit::Res<()>;

fn main() -> ExitCode {
    // As bash: a write to a closed pipe ends the command (Rust ignores SIGPIPE); glob order and
    // `=~` follow the locale.
    // SAFETY: at the start, before any other thread; SIG_DFL is a valid disposition.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    shell::init_locale();
    let mut args = std::env::args_os().skip(1);
    let cmd = args.next().unwrap_or_default();
    let rest: Vec<OsString> = args.collect();
    let ported: Option<Run> = match cmd.to_str() {
        Some("path") => Some(cmd::path::run),
        Some("env") => Some(cmd::env::run),
        Some("which") => Some(cmd::which::run),
        _ => None,
    };
    // BOXES: bash refuses a call without XDG_RUNTIME_DIR before it hands one over.
    let runtime = shell::env_or_empty("XDG_RUNTIME_DIR");
    if let (Some(run), false) = (ported, runtime.is_empty()) {
        let words: Vec<Vec<u8>> = rest.into_iter().map(OsString::into_vec).collect();
        let mut ctx = exit::Ctx::new(cmd.as_encoded_bytes(), cat!(runtime, "/omabox"));
        return match run(&mut ctx, &words) {
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
        // A command bash handed here that this build does not have: a binary older than its bash
        // (a pull without ./install.sh). Bash runs it, told not to hand it back.
        _ => back_to_bash(cmd, rest),
    }
}

/// The checkout this binary belongs to: OMABOX_ROOT from bash, else two levels up from build/bin.
fn root() -> PathBuf {
    if let Some(r) = std::env::var_os("OMABOX_ROOT") {
        return PathBuf::from(r);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn back_to_bash(cmd: OsString, rest: Vec<OsString>) -> ExitCode {
    let script = root().join("bin/omabox");
    let err = Command::new(&script)
        .arg(cmd)
        .args(rest)
        .env("OMABOX_BASH", "1")
        .exec();
    eprintln!("omabox: could not run {}: {err}", script.display());
    ExitCode::from(126)
}
