//! jq, run as bin/omabox runs it, for the programs whose answer is jq's own (the desktop's windows,
//! layers and changes: WIN_JQ, DESK_FACTS_JQ, ...): the same program text, the same arguments, so the
//! same bytes out, errors and status included. What a command reads with a plain path stays in
//! json.rs.

use crate::exec::{feed, read_all};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Stdio};

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
    let mut c = Command::new("jq");
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
    let child = c.spawn();
    drop(c);
    let Ok(mut child) = child else {
        let _ = read_all(r, None);
        return Ran {
            rc: 127,
            out: Vec::new(),
        };
    };
    // A here-string's newline after the input.
    let feeder = match (input, child.stdin.take()) {
        (Some(i), Some(si)) => Some(feed(si.into(), crate::cat!(i, "\n"))),
        _ => None,
    };
    let out = read_all(r, None);
    if let Some(f) = feeder {
        let _ = f.join();
    }
    let rc = child.wait().map(|s| crate::exec::status(s).0).unwrap_or(1);
    Ran { rc, out }
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
}
