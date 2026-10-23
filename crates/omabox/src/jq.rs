//! jq, run as bin/omabox runs it, for the programs whose answer is jq's own (the desktop's windows,
//! layers and changes: WIN_JQ, DESK_FACTS_JQ, ...): the same program text, the same arguments, so the
//! same bytes out, errors and status included. What a command reads with a plain path stays in
//! json.rs.

use crate::exec::{feed, read_all};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::process::Stdio;

/// Where jq's stderr goes: this process's (the caller's 2>), nowhere, or into the output (`2>&1`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Err {
    Inherit,
    Null,
    Out,
}

/// What a jq run gave: its status (127 when there was no jq to run) and its stdout.
pub struct Ran {
    pub rc: i32,
    pub out: Vec<u8>,
}

impl Ran {
    /// `$(jq ...)`: the output without its trailing newlines.
    pub fn text(&self) -> Vec<u8> {
        crate::shell::strip_nl(&self.out)
    }

    pub fn ok(&self) -> bool {
        self.rc == 0
    }
}

/// `jq ARGS... <<<"$INPUT"` (INPUT None: no stdin, for -n).
pub fn jq<A: AsRef<[u8]>>(args: &[A], input: Option<&[u8]>, err: Err) -> Ran {
    let input = input.map(|i| crate::cat!(i, "\n"));
    tool("jq", args, input.as_deref(), err)
}

/// `awk ARGS... <<<"$INPUT"`: awk as bash runs it, its program verbatim (gawk's numbers are its
/// own); INPUT None: no stdin.
pub fn awk<A: AsRef<[u8]>>(args: &[A], input: Option<&[u8]>, err: Err) -> Ran {
    let input = input.map(|i| crate::cat!(i, "\n"));
    tool("awk", args, input.as_deref(), err)
}

/// `NAME ARGS...` fed INPUT exactly as given (a pipe's bytes; None: /dev/null).
pub fn tool<A: AsRef<[u8]>>(name: &str, args: &[A], input: Option<&[u8]>, err: Err) -> Ran {
    let mut c = crate::sig::command(name);
    c.args(args.iter().map(|a| OsStr::from_bytes(a.as_ref())));
    c.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let Ok((r, w)) = crate::exec::pipe() else {
        return Ran {
            rc: 1,
            out: Vec::new(),
        };
    };
    let w2 = if err == Err::Out {
        w.try_clone().ok()
    } else {
        None
    };
    c.stdout(Stdio::from(w));
    match (err, w2) {
        (Err::Out, Some(w2)) => c.stderr(Stdio::from(w2)),
        (Err::Null, _) | (Err::Out, None) => c.stderr(Stdio::null()),
        (Err::Inherit, _) => c.stderr(Stdio::inherit()),
    };
    // (spawn and wait one foreground step, as bash's child is from its fork on)
    crate::sig::foreground(|| {
        let child = c.spawn();
        drop(c);
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = read_all(r, None);
                // bash's own line for a command it could not run (in the child, after its
                // redirections: where jq's stderr goes), less its "SCRIPT: line N: ".
                let (rc, m) = not_run(name, &e);
                match err {
                    Err::Inherit => {
                        let _ = std::io::Write::write_all(&mut std::io::stderr(), &m);
                    }
                    Err::Out => return Ran { rc, out: m },
                    Err::Null => {}
                }
                return Ran {
                    rc,
                    out: Vec::new(),
                };
            }
        };
        let feeder = match (input, child.stdin.take()) {
            (Some(i), Some(si)) => Some(feed(si.into(), i.to_vec())),
            _ => None,
        };
        let out = read_all(r, None);
        if let Some(f) = feeder {
            let _ = f.join();
        }
        let rc = child.wait().map(|s| crate::exec::status(s).0).unwrap_or(1);
        Ran { rc, out }
    })
}

/// Bash's status and words for a tool not run: 127 "jq: command not found" when none is on PATH, else
/// 126 "PATH_OF_JQ: REASON" (an argument past 128 KiB: "Argument list too long").
fn not_run(name: &str, e: &std::io::Error) -> (i32, Vec<u8>) {
    let found = crate::shell::env_or_empty("PATH")
        .split(|&c| c == b':')
        .map(|d| crate::cat!(if d.is_empty() { b".".as_slice() } else { d }, "/", name))
        .find(|p| crate::exec::executable(p) && crate::shell::path(p).is_file());
    match (found, e.raw_os_error()) {
        (None, _) | (_, Some(libc::ENOENT)) => (127, crate::cat!(name, ": command not found\n")),
        (Some(p), code) => {
            // SAFETY: strerror's string for a valid errno, read at once.
            let why = unsafe {
                std::ffi::CStr::from_ptr(libc::strerror(code.unwrap_or(libc::EIO)))
                    .to_string_lossy()
                    .into_owned()
            };
            (126, crate::cat!(p, ": ", why, "\n"))
        }
    }
}

/// `jq -c -n --argjson K V ... PROGRAM` and the like: the words of a call, built in order.
#[derive(Default)]
pub struct Args(Vec<Vec<u8>>);

impl Args {
    pub fn new(first: &[&str]) -> Args {
        Args(first.iter().map(|a| a.as_bytes().to_vec()).collect())
    }

    pub fn arg(mut self, a: impl AsRef<[u8]>) -> Args {
        self.0.push(a.as_ref().to_vec());
        self
    }

    pub fn kv(self, flag: &str, k: &str, v: impl AsRef<[u8]>) -> Args {
        self.arg(flag).arg(k).arg(v)
    }

    pub fn run(&self, input: Option<&[u8]>, err: Err) -> Ran {
        jq(&self.0, input, err)
    }

    /// The words as built.
    pub fn into_words(self) -> Vec<Vec<u8>> {
        self.0
    }

    /// jq reading the file its last word names (no here-string).
    pub fn run_file(&self, err: Err) -> Ran {
        jq(&self.0, None, err)
    }

    /// The same words given to awk (`awk -v K=V ... PROGRAM <<<"$INPUT"`).
    pub fn awk(&self, input: Option<&[u8]>, err: Err) -> Ran {
        awk(&self.0, input, err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // What bash said for each: `jq -n --argjson c "$BIG"` (one argument past 128 KiB), a jq not on
    // PATH (both less "SCRIPT: line N: ").
    #[test]
    fn not_run_as_bash_says() {
        let big = vec![b'1'; 200_000];
        let r = Args::new(&["-n"])
            .kv("--argjson", "c", &big)
            .arg("1")
            .run(None, Err::Out);
        assert_eq!(r.rc, 126);
        assert!(r.out.ends_with(b"/jq: Argument list too long\n"));
        let e = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(
            not_run("jq", &e),
            (127, b"jq: command not found\n".to_vec())
        );
    }
}
