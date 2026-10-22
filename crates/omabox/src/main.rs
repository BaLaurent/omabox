//! omabox's Rust half (#324). bin/omabox hands it the commands it has taken over, with the words as
//! bash's main has them by then: `-b NAME` moved after the command, `--opt=value` split, `--help`,
//! the relay and the broker already dealt with. Every other command stays in bash.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let cmd = args.next().unwrap_or_default();
    let rest: Vec<OsString> = args.collect();
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
