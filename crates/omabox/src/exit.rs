//! Exit codes and messages, one meaning in every command (#273; `omabox help exit-codes`): 1 a
//! command that ran and failed, 2 one called wrong (a usage error), 124 not in time, 125 run's own
//! failure. DIE_RC: the exit code of every refusal when set (2 under `check`, 125 under `run`).
//! PARSING: true while the command line is read (until `parsed` or select_box), when a refusal is a
//! usage error: exit 2, and where the command's options are.

use std::io::Write;

/// How a command ends: its exit code, its words said already.
#[derive(Debug, PartialEq, Eq)]
pub struct Exit(pub u8);

pub type Res<T> = Result<T, Exit>;

/// The bytes of each part, one after the other: a message from names, paths and words.
#[macro_export]
macro_rules! cat {
    ($($p:expr),* $(,)?) => {{
        let mut v: Vec<u8> = Vec::new();
        $( v.extend_from_slice(AsRef::<[u8]>::as_ref(&$p)); )*
        v
    }};
}

/// `say`: one line on stderr.
pub fn say(m: impl AsRef<[u8]>) {
    let mut line = b"omabox: ".to_vec();
    line.extend_from_slice(m.as_ref());
    line.push(b'\n');
    let _ = std::io::stderr().write_all(&line);
}

/// What bash's globals hold for one call: the refusal conventions and the box selected.
#[derive(Clone)]
pub struct Ctx {
    pub parsing: bool,
    pub die_rc: Option<u8>,
    /// MAIN_CMD: the command asked for.
    pub main_cmd: Vec<u8>,
    /// BOXES: $XDG_RUNTIME_DIR/omabox, as written.
    pub boxes: Vec<u8>,
    /// NAME and D (the box dir), set by select_box.
    pub name: Vec<u8>,
    pub d: Vec<u8>,
    /// NAME_DERIVED: the name came from default_name, not from the caller.
    pub name_derived: bool,
    /// PASS (run --pass): the caller's variables for the command, by name.
    pub pass: Vec<Vec<u8>>,
    /// ENVKV (run --env-file, then --env): KEY=VAL entries for the command, before --pass's.
    pub envkv: Vec<Vec<u8>>,
    /// BOX_GONE_SAID: a box that went down while a command entered it was said so (ns_said).
    pub box_gone_said: bool,
}

impl Ctx {
    pub fn new(main_cmd: &[u8], boxes: Vec<u8>) -> Ctx {
        Ctx {
            parsing: true,
            die_rc: None,
            main_cmd: main_cmd.to_vec(),
            boxes,
            name: Vec::new(),
            d: Vec::new(),
            name_derived: false,
            pass: Vec::new(),
            envkv: Vec::new(),
            box_gone_said: false,
        }
    }

    /// `${MAIN_CMD:+$MAIN_CMD: }`.
    pub fn label(&self) -> Vec<u8> {
        if self.main_cmd.is_empty() {
            Vec::new()
        } else {
            cat!(self.main_cmd, ": ")
        }
    }

    /// die: a refusal; a usage error while the command line is read.
    pub fn die(&self, m: impl AsRef<[u8]>) -> Exit {
        if self.parsing {
            return self.usage_die(m);
        }
        say(m);
        Exit(self.die_rc.unwrap_or(1))
    }

    /// A usage error: exit 2 (run's 125), the message followed by where the command's options are.
    pub fn usage_die(&self, m: impl AsRef<[u8]>) -> Exit {
        let mut m = m.as_ref().to_vec();
        if !contains(&m, b"omabox help") && !contains(&m, b"--help") {
            m.extend_from_slice(b" (omabox help");
            if !self.main_cmd.is_empty() {
                m.push(b' ');
                m.extend_from_slice(&self.main_cmd);
            }
            m.push(b')');
        }
        say(m);
        Exit(self.die_rc.unwrap_or(2))
    }

    /// Exit as die would, for a refusal a helper has said already.
    pub fn refused(&self) -> Exit {
        match self.die_rc {
            Some(rc) => Exit(rc),
            None if self.parsing => Exit(2),
            None => Exit(1),
        }
    }

    /// The command line read: refusals from here are failures (1), not usage errors.
    pub fn parsed(&mut self) {
        self.parsing = false;
    }

    /// `$D/REL`.
    pub fn at(&self, rel: &str) -> std::path::PathBuf {
        crate::shell::path(&cat!(self.d, "/", rel))
    }

    /// `$BOXES/REL`.
    pub fn in_boxes(&self, rel: impl AsRef<[u8]>) -> std::path::PathBuf {
        crate::shell::path(&cat!(self.boxes, "/", rel))
    }
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

/// Lines on stdout. A write that fails (stdout closed) ends the command, 1, as bash's echo under
/// set -e does.
pub fn out(v: impl AsRef<[u8]>) -> Res<()> {
    let mut o = std::io::stdout().lock();
    o.write_all(v.as_ref())
        .and_then(|()| o.flush())
        .map_err(|_| Exit(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        let mut c = Ctx::new(b"path", b"/x".to_vec());
        assert_eq!(c.die("a"), Exit(2));
        assert_eq!(c.refused(), Exit(2));
        c.parsed();
        assert_eq!(c.die("a"), Exit(1));
        assert_eq!(c.usage_die("a"), Exit(2));
        c.die_rc = Some(125);
        assert_eq!(c.die("a"), Exit(125));
        assert_eq!(c.refused(), Exit(125));
    }
}
