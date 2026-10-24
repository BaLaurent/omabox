//! The box's bwrap command (#324 wave 5a, #355), built only through methods that hold SECURITY.md's
//! rules by construction:
//! - S1, the seat: no bind's source or destination is one of the seat's paths (a DRM card node,
//!   `/dev/input`, `/run/seatd.sock`, the system bus's `/run/dbus`, your real `$XDG_RUNTIME_DIR`) or a
//!   folder holding one; devices go in only as render nodes and NVIDIA's own nodes (DevNode); the one
//!   card path a box has is an empty file of the box dir's (finding 294's placeholder), never the
//!   device (seat_path, check_bind).
//! - S4, host code read-only: a host folder goes in read-only (`--ro-bind`) or as an overlay whose
//!   writes are thrown away (`--tmp-overlay`); the only writable binds are the box's own runtime dir
//!   and HOME, and a --systemd box's own cgroup (own_rw, cgroup_rw).
//! - S3: what a user or a repo names goes through refuse_src and refuse_dest (user_ro); the fixed
//!   system binds (/usr, /etc, /sys) are a list of their own (sys_ro), and omabox's own files must be
//!   under the checkout or the box's own dirs (own_ro).
//!
//! The words come out in bash's order (cmd_up's `args`), so launch.sh is the same both ways.

use crate::cat;

/// Why P is the seat's (S1), or None: a DRM card node, the input devices, seatd's socket, the
/// system bus, the user's runtime dir (RUNTIME), at or under them.
pub fn seat_path(p: &[u8], runtime: &[u8]) -> Option<&'static str> {
    let p = lexical(p);
    let under = |root: &[u8]| p == root || p.starts_with(&cat!(root, "/"));
    if let Some(n) = p.strip_prefix(b"/dev/dri/card".as_slice()) {
        let n = n.split(|&c| c == b'/').next().unwrap_or_default();
        if !n.is_empty() && n.iter().all(u8::is_ascii_digit) {
            return Some("a DRM card node (the seat's display)");
        }
    }
    if under(b"/dev/input") {
        return Some("the input devices");
    }
    if under(b"/run/seatd.sock") {
        return Some("seatd's socket");
    }
    if under(b"/run/dbus") {
        return Some("the system bus");
    }
    if !runtime.is_empty() && under(&lexical(runtime)) {
        return Some("your runtime dir");
    }
    None
}

/// Why a bind of folder P would bring a seat path in (P holds one), or None.
pub fn holds_seat(p: &[u8], runtime: &[u8]) -> Option<&'static str> {
    let p = lexical(p);
    let holds = |x: &[u8]| p.as_slice() == b"/" || x.starts_with(&cat!(p, "/"));
    for (x, why) in [
        (b"/dev/dri/card0".as_slice(), "it holds the DRM card nodes"),
        (b"/dev/input", "it holds the input devices"),
        (b"/run/seatd.sock", "it holds seatd's socket"),
        (b"/run/dbus", "it holds the system bus"),
    ] {
        if holds(x) {
            return Some(why);
        }
    }
    if !runtime.is_empty() && holds(&lexical(runtime)) {
        return Some("it holds your runtime dir");
    }
    None
}

/// P made absolute-lexical: `.` and empty parts dropped, `..` taken back (no links followed: the
/// sources reach here resolved already; this keeps `/dev//input` and `/dev/./input` from slipping by).
pub fn lexical(p: &[u8]) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = Vec::new();
    for c in p.split(|&c| c == b'/') {
        match c {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            _ => parts.push(c),
        }
    }
    let mut out = Vec::new();
    for c in parts {
        out.push(b'/');
        out.extend_from_slice(c);
    }
    if out.is_empty() {
        out.push(b'/');
    }
    out
}

/// A device a box may have (S1): a render node, NVIDIA's numbered nodes, its control node and its
/// unified memory node (#149). Nothing else can be made one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevNode(Vec<u8>);

impl DevNode {
    pub fn new(p: &[u8]) -> Option<DevNode> {
        let digits = |s: &[u8]| !s.is_empty() && s.iter().all(u8::is_ascii_digit);
        let ok = p
            .strip_prefix(b"/dev/dri/renderD".as_slice())
            .is_some_and(digits)
            || p.strip_prefix(b"/dev/nvidia".as_slice())
                .is_some_and(digits)
            || p == b"/dev/nvidiactl"
            || p == b"/dev/nvidia-uvm";
        ok.then(|| DevNode(p.to_vec()))
    }

    pub fn path(&self) -> &[u8] {
        &self.0
    }
}

/// A bind the bwrap command is refused (one S1, S3 or S4 does not allow), in words.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused(pub Vec<u8>);

/// The bwrap words, in order, each bind checked as it is added.
pub struct Bwrap {
    args: Vec<Vec<u8>>,
    /// The user's runtime dir (the seat's, S1), and the box's own dir in it (D): a source under the
    /// runtime dir is allowed only in D.
    runtime: Vec<u8>,
    own: Vec<u8>,
    /// The box's own writable folders (S4): its runtime dir and its HOME.
    rw: Vec<Vec<u8>>,
    /// Where omabox's own files may come from besides D: the checkout, a private aquamarine.
    own_roots: Vec<Vec<u8>>,
}

/// The fixed system binds (S3's list of its own): read-only, at their own path.
pub const SYS_RO: [&str; 3] = ["/usr", "/etc", "/sys"];

impl Bwrap {
    /// RUNTIME: the user's runtime dir; D: the box dir; RW: the box's runtime dir and HOME;
    /// OWN_ROOTS: where omabox's own files may be besides D.
    pub fn new(runtime: &[u8], d: &[u8], rw: Vec<Vec<u8>>, own_roots: Vec<Vec<u8>>) -> Bwrap {
        Bwrap {
            args: Vec::new(),
            runtime: runtime.to_vec(),
            own: lexical(d),
            rw: rw.iter().map(|p| lexical(p)).collect(),
            own_roots: own_roots.iter().map(|p| lexical(p)).collect(),
        }
    }

    pub fn into_args(self) -> Vec<Vec<u8>> {
        self.args
    }

    /// Plain words (`--unshare-pid`, `--proc /proc`, ...): never a bind.
    pub fn words(&mut self, w: &[&[u8]]) {
        debug_assert!(!w.iter().any(|f| is_bind(f)));
        self.args.extend(w.iter().map(|x| x.to_vec()));
    }

    pub fn setenv(&mut self, k: &[u8], v: &[u8]) {
        self.args.push(b"--setenv".to_vec());
        self.args.push(k.to_vec());
        self.args.push(v.to_vec());
    }

    /// The seat check for one bind (S1): its source neither the seat's nor holding it (a source in
    /// the runtime dir only in the box's own dir), its destination none of the seat's.
    fn check_bind(&self, src: &[u8], dest: &[u8]) -> Result<(), Refused> {
        let s = lexical(src);
        let in_own = s == self.own || s.starts_with(&cat!(self.own, "/"));
        let why = if in_own {
            None
        } else {
            seat_path(&s, &self.runtime).or_else(|| holds_seat(&s, &self.runtime))
        };
        if let Some(why) = why {
            return Err(Refused(cat!(
                "refusing to mount ",
                src,
                " into a box: ",
                why
            )));
        }
        // (a destination at the runtime dir's path is the box's own run dir, bound there)
        let d = lexical(dest);
        if let Some(why) = seat_path(&d, b"").or_else(|| holds_seat(&d, b"")) {
            return Err(Refused(cat!(
                "refusing to mount at ",
                dest,
                " in a box: ",
                why
            )));
        }
        Ok(())
    }

    fn push_bind(&mut self, how: &[u8], src: &[u8], dest: &[u8]) -> Result<(), Refused> {
        self.check_bind(src, dest)?;
        self.args.push(how.to_vec());
        self.args.push(src.to_vec());
        self.args.push(dest.to_vec());
        Ok(())
    }

    /// One of SYS_RO, read-only at its own path.
    pub fn sys_ro(&mut self, p: &str) -> Result<(), Refused> {
        if !SYS_RO.contains(&p) {
            return Err(Refused(cat!(p, " is not one of the box's system folders")));
        }
        self.push_bind(b"--ro-bind", p.as_bytes(), p.as_bytes())
    }

    /// One of omabox's own files or folders (the checkout's share and tools, the box dir's, a private
    /// aquamarine), read-only.
    pub fn own_ro(&mut self, src: &[u8], dest: &[u8]) -> Result<(), Refused> {
        let s = lexical(src);
        let ok = self
            .own_roots
            .iter()
            .chain(std::iter::once(&self.own))
            .any(|r| s == *r || s.starts_with(&cat!(r, "/")));
        if !ok {
            return Err(Refused(cat!(src, " is not one of omabox's own files")));
        }
        self.push_bind(b"--ro-bind", src, dest)
    }

    /// What a user, a repo or a desk names (S3): refused by refuse_src or refuse_dest, else
    /// read-only (S4).
    pub fn user_ro(&mut self, src: &[u8], dest: &[u8]) -> Result<(), Refused> {
        if let Some(why) = crate::repo::refuse_src(src) {
            return Err(Refused(cat!(
                "refusing to mount ",
                src,
                " into a box: ",
                why
            )));
        }
        if let Some(why) = crate::upspec::refuse_dest(dest) {
            return Err(Refused(cat!(
                "refusing to mount ",
                src,
                " at ",
                dest,
                " in a box: ",
                why
            )));
        }
        self.push_bind(b"--ro-bind", src, dest)
    }

    /// A read-only bind under a rule of its own (a desk font folder at its place in the box HOME, a
    /// jailed caller's cover from the box dir): refuse_src unless it is the box dir's, and the seat
    /// check.
    pub fn checked_ro(&mut self, src: &[u8], dest: &[u8]) -> Result<(), Refused> {
        let s = lexical(src);
        let own = s == self.own || s.starts_with(&cat!(self.own, "/"));
        if !own && let Some(why) = crate::repo::refuse_src(src) {
            return Err(Refused(cat!(
                "refusing to mount ",
                src,
                " into a box: ",
                why
            )));
        }
        self.push_bind(b"--ro-bind", src, dest)
    }

    /// The box's own runtime dir or HOME, writable (S4: the only writable host folders).
    pub fn own_rw(&mut self, src: &[u8], dest: &[u8]) -> Result<(), Refused> {
        if !self.rw.contains(&lexical(src)) {
            return Err(Refused(cat!(
                "refusing to mount ",
                src,
                " writable into a box: only its own runtime dir and HOME are"
            )));
        }
        self.push_bind(b"--bind", src, dest)
    }

    /// A --systemd box's own cgroup, writable as its /sys/fs/cgroup (finding 61): bash's
    /// `@CGROUP@`, filled in inside the scope, never a host path named here.
    pub fn cgroup_rw(&mut self) {
        self.args.push(b"--bind".to_vec());
        self.args.push(b"@CGROUP@".to_vec());
        self.args.push(b"/sys/fs/cgroup".to_vec());
    }

    /// A device node (S1: DevNode's only).
    pub fn dev(&mut self, n: &DevNode) -> Result<(), Refused> {
        self.push_bind(b"--dev-bind", n.path(), n.path())
    }

    /// NVIDIA's EGL stats the GPU's card node (finding 294): an empty read-only file of the box
    /// dir's at DEST, /dev/dri/cardN, never the device. FILE must be that file, empty and regular.
    pub fn card_placeholder(&mut self, file: &[u8], dest: &[u8]) -> Result<(), Refused> {
        let s = lexical(file);
        let ok_card = dest
            .strip_prefix(b"/dev/dri/card".as_slice())
            .is_some_and(|n| !n.is_empty() && n.iter().all(u8::is_ascii_digit));
        let ok_file = s.starts_with(&cat!(self.own, "/"))
            && std::fs::symlink_metadata(crate::shell::path(&s))
                .is_ok_and(|m| m.file_type().is_file() && m.len() == 0);
        if !ok_card || !ok_file {
            return Err(Refused(cat!(
                "refusing a card placeholder ",
                file,
                " at ",
                dest,
                ": not an empty file of the box's"
            )));
        }
        self.args.push(b"--ro-bind".to_vec());
        self.args.push(file.to_vec());
        self.args.push(dest.to_vec());
        Ok(())
    }

    /// A folder at its own path whose writes are thrown away (`--overlay`, S4).
    pub fn tmp_overlay(&mut self, p: &[u8]) -> Result<(), Refused> {
        if let Some(why) = crate::repo::refuse_src(p).or_else(|| crate::upspec::refuse_dest(p)) {
            return Err(Refused(cat!("refusing to overlay ", p, " in a box: ", why)));
        }
        self.check_bind(p, p)?;
        for w in [b"--overlay-src".as_slice(), p, b"--tmp-overlay", p] {
            self.args.push(w.to_vec());
        }
        Ok(())
    }

    /// /etc as a read-only overlay with the box dir's ETC on top (findings 135, 363).
    pub fn etc_overlay(&mut self, etc: &[u8]) -> Result<(), Refused> {
        self.check_bind(b"/etc", b"/etc")?;
        self.check_bind(etc, b"/etc")?;
        for w in [
            b"--overlay-src".as_slice(),
            b"/etc",
            b"--overlay-src",
            etc,
            b"--ro-overlay",
            b"/etc",
        ] {
            self.args.push(w.to_vec());
        }
        Ok(())
    }

    /// One of the box dir's /etc files over the host's (the box's own omarchy.conf, locale.conf).
    pub fn etc_file(&mut self, src: &[u8], dest: &[u8]) -> Result<(), Refused> {
        let s = lexical(src);
        if !s.starts_with(&cat!(self.own, "/etc/")) || !dest.starts_with(b"/etc/") {
            return Err(Refused(cat!(src, " is not one of the box's /etc files")));
        }
        self.push_bind(b"--ro-bind", src, dest)
    }

    /// A resolv.conf the host's /etc/resolv.conf links to under /run (finding 52), at its own path.
    pub fn resolv(&mut self, p: &[u8]) -> Result<(), Refused> {
        if !p.starts_with(b"/run/") {
            return Err(Refused(cat!(p, " is not a resolver file under /run")));
        }
        self.push_bind(b"--ro-bind", p, p)
    }
}

/// Whether W is a bind (what `words` must never be handed).
fn is_bind(w: &[u8]) -> bool {
    matches!(
        w,
        b"--bind"
            | b"--ro-bind"
            | b"--dev-bind"
            | b"--bind-try"
            | b"--ro-bind-try"
            | b"--dev-bind-try"
            | b"--overlay-src"
            | b"--bind-fd"
            | b"--ro-bind-fd"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const RT: &[u8] = b"/run/user/1000";

    #[test]
    fn seat_paths() {
        for p in [
            "/dev/dri/card0",
            "/dev/dri/card12",
            "/dev/input",
            "/dev/input/event3",
            "/dev//input/./mice",
            "/run/seatd.sock",
            "/run/dbus",
            "/run/dbus/system_bus_socket",
            "/run/user/1000",
            "/run/user/1000/wayland-1",
            "/dev/dri/../dri/card1",
        ] {
            assert!(seat_path(p.as_bytes(), RT).is_some(), "{p}");
        }
        for p in [
            "/dev/dri/renderD128",
            "/dev/dri/cardx",
            "/run/user/10000",
            "/usr",
            "/run/systemd/resolve/stub-resolv.conf",
        ] {
            assert!(seat_path(p.as_bytes(), RT).is_none(), "{p}");
        }
        for p in ["/", "/dev", "/dev/dri", "/run", "/run/user"] {
            assert!(holds_seat(p.as_bytes(), RT).is_some(), "{p}");
        }
        assert!(holds_seat(b"/usr", RT).is_none());
    }

    #[test]
    fn devices() {
        for p in [
            "/dev/dri/renderD128",
            "/dev/nvidia0",
            "/dev/nvidiactl",
            "/dev/nvidia-uvm",
        ] {
            assert!(DevNode::new(p.as_bytes()).is_some(), "{p}");
        }
        for p in [
            "/dev/dri/card0",
            "/dev/dri/renderD",
            "/dev/dri/renderD1x",
            "/dev/input/event0",
            "/dev/nvidia-modeset",
            "/dev/dri/renderD128/../card0",
            "/dev/nvidia",
        ] {
            assert!(DevNode::new(p.as_bytes()).is_none(), "{p}");
        }
    }

    fn bw() -> Bwrap {
        Bwrap::new(
            RT,
            b"/run/user/1000/omabox/b",
            vec![
                b"/run/user/1000/omabox/b/run".to_vec(),
                b"/home/u/.cache/omabox/b/home".to_vec(),
            ],
            vec![b"/home/u/omabox".to_vec()],
        )
    }

    #[test]
    fn binds() {
        let mut b = bw();
        assert!(b.sys_ro("/usr").is_ok());
        assert!(b.sys_ro("/dev").is_err());
        assert!(
            b.own_rw(b"/run/user/1000/omabox/b/run", RT).is_ok(),
            "the box's run dir at the runtime path"
        );
        assert!(b.own_rw(RT, RT).is_err(), "the real runtime dir");
        assert!(b.own_rw(b"/home/u/src", b"/home/u/src").is_err());
        assert!(
            b.own_ro(b"/home/u/omabox/share", b"/opt/omabox/share")
                .is_ok()
        );
        assert!(b.own_ro(b"/home/u/.ssh", b"/opt/omabox/share").is_err());
        assert!(
            b.own_ro(b"/run/user/1000/omabox/b/../c/run", b"/x")
                .is_err()
        );
        assert!(b.resolv(b"/run/systemd/resolve/stub-resolv.conf").is_ok());
        assert!(b.resolv(b"/run/dbus/system_bus_socket").is_err());
        assert!(b.resolv(b"/etc/passwd").is_err());
        assert_eq!(b.into_args()[0], b"--ro-bind");
    }

    fn path_strategy() -> impl Strategy<Value = Vec<u8>> {
        let parts = prop::sample::select(vec![
            "dev",
            "dri",
            "card0",
            "card7",
            "renderD128",
            "input",
            "event1",
            "run",
            "dbus",
            "seatd.sock",
            "user",
            "1000",
            "..",
            ".",
            "",
            "usr",
            "home",
            "u",
            "tmp",
            "nvidia0",
        ]);
        prop::collection::vec(parts, 1..6).prop_map(|v| format!("/{}", v.join("/")).into_bytes())
    }

    proptest! {
        // S1: whatever a bind is asked for, no seat path is ever a bind's source or destination in
        // what comes out.
        #[test]
        fn no_seat_path_gets_through(srcs in prop::collection::vec(path_strategy(), 1..8),
                                     dests in prop::collection::vec(path_strategy(), 1..8)) {
            let mut b = bw();
            for (s, d) in srcs.iter().zip(dests.iter()) {
                let _ = b.user_ro(s, d);
                let _ = b.own_ro(s, d);
                let _ = b.own_rw(s, d);
                let _ = b.checked_ro(s, d);
                let _ = b.tmp_overlay(s);
                let _ = b.resolv(s);
                if let Some(n) = DevNode::new(s) { let _ = b.dev(&n); }
            }
            let args = b.into_args();
            let mut i = 0;
            while i < args.len() {
                if is_bind(&args[i]) && args[i].as_slice() != b"--overlay-src" {
                    let (s, d) = (&args[i + 1], &args[i + 2]);
                    let own = lexical(s).starts_with(b"/run/user/1000/omabox/b/");
                    prop_assert!(own || seat_path(s, RT).is_none(), "source {:?}", String::from_utf8_lossy(s));
                    prop_assert!(own || holds_seat(s, RT).is_none(), "source {:?}", String::from_utf8_lossy(s));
                    prop_assert!(seat_path(d, b"").is_none(), "dest {:?}", String::from_utf8_lossy(d));
                    i += 3;
                } else if args[i].as_slice() == b"--overlay-src" {
                    let s = &args[i + 1];
                    prop_assert!(seat_path(s, RT).is_none() && holds_seat(s, RT).is_none());
                    i += 2;
                } else { i += 1; }
            }
        }
    }
}
