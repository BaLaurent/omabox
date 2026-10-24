//! The box HOME, seeded from the user's look through an allow list (#324 wave 5a, #355 S2):
//! seed_home and what it calls.
//!
//! What seed_home may read of the user's HOME is one table, SEED_ALLOW: a host path to copy is a
//! `Src`, and a Src is made only from that table (`Look::src`), from --seed's SRC (`Src::asked`, the
//! user's own ask, checked by seed_spec), or inside desk_files' walk (--desk-shell: its deny-list and
//! content check). A --from save is copied by up itself, before this. Every write into the box HOME
//! is at a `Dest`: a path relative to it with no `..`, its links cleared first (home_unlink, finding
//! 168). The git identity is the one thing read that is no file: two values of `git config --global`.

use crate::cat;
use crate::exit::{Ctx, Exit, Res, say};
use crate::jail::in_jail;
use crate::repo::{readlink_e, readlink_f, refuse_src, refuse_src_look};
use crate::shell;
use crate::shelljson;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;

/// Where an allowed source is looked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Base {
    /// The user's HOME.
    Home,
    /// desk_dir: ~/.config/omarchy, or the suite's fake desk (OMABOX_DESK_CONFIG).
    Desk,
    /// desk_home: HOME, or the suite's fake desk's (OMABOX_DESK_HOME): its fonts.
    DeskHome,
    /// /etc/skel: the HOME a new Omarchy user starts with (finding 69).
    Skel,
    /// The box's Omarchy: --omarchy's tree, else /usr/share/omarchy.
    Omarchy,
}

/// One thing seed_home may read: BASE/REL, a `*` in REL standing for one name that is not hidden
/// (a leading `*` for the start of one: `*xdg-terminals.list`), "." for BASE itself.
pub struct Allowed {
    pub base: Base,
    pub rel: &'static str,
    /// What it is, in words: the table's own documentation (its test reads it).
    #[cfg_attr(not(test), allow(dead_code))]
    pub what: &'static str,
}

const fn a(base: Base, rel: &'static str, what: &'static str) -> Allowed {
    Allowed { base, rel, what }
}

/// Everything of the user's look a box HOME is seeded from (finding 69), none of it secret: never
/// ~/.config/omarchy wholesale (api-keys.env lives there, hooks/ are the user's scripts).
pub const SEED_ALLOW: &[Allowed] = &[
    a(Base::Skel, ".", "a new Omarchy user's HOME"),
    a(
        Base::Omarchy,
        "config/omarchy/shell.json",
        "Omarchy's own bar (--stock-bar)",
    ),
    a(
        Base::Omarchy,
        "applications/*",
        "Omarchy's desktop entries, without /etc/skel",
    ),
    a(Base::Desk, "shell.json", "the desk's bar"),
    a(Base::Desk, "shell.toml", "the desk's bar sizes and fonts"),
    a(Base::Desk, "extensions", "the plugins' settings (#231)"),
    a(
        Base::Home,
        ".local/state/omarchy/current/theme",
        "the current theme",
    ),
    a(
        Base::Home,
        ".local/state/omarchy/current/theme.name",
        "its name",
    ),
    a(
        Base::Home,
        ".config/omarchy/backgrounds/*/*",
        "the user's own wallpapers for a theme (#184)",
    ),
    a(
        Base::Home,
        ".config/foot",
        "foot's config (the theme's colours)",
    ),
    a(Base::Home, ".config/alacritty", "alacritty's config"),
    a(Base::Home, ".config/ghostty", "ghostty's config"),
    a(Base::Home, ".config/kitty", "kitty's config"),
    a(
        Base::Home,
        ".config/*xdg-terminals.list",
        "the terminal choice",
    ),
    a(
        Base::Home,
        ".config/omarchy/branding",
        "About's text and logo",
    ),
    a(
        Base::Home,
        ".config/omarchy/themes/*",
        "the user's themes (the theme picker)",
    ),
    a(
        Base::Home,
        ".local/state/omarchy/toggles/hypr",
        "Hyprland toggles",
    ),
    a(
        Base::DeskHome,
        ".local/share/fonts",
        "the desk's fonts, mounted (#261)",
    ),
    a(Base::DeskHome, ".fonts", "the desk's fonts, mounted (#261)"),
];

/// A host path seed_home may copy (or mount: the fonts) into a box: see the module's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Src(Vec<u8>);

impl Src {
    /// --seed's SRC (seed_spec's: resolved and checked), the user's own ask; nothing else.
    pub fn asked(path: Vec<u8>) -> Src {
        Src(path)
    }

    /// A file desk_walk took (--desk-shell): past its name and content checks.
    fn desk_file(path: Vec<u8>) -> Src {
        Src(path)
    }

    pub fn path(&self) -> &[u8] {
        &self.0
    }
}

/// The roots of SEED_ALLOW's bases for one up: where each is looked for.
pub struct Look<'a> {
    /// --omarchy's tree, or empty for /usr/share/omarchy.
    pub omarchy: &'a [u8],
}

impl Look<'_> {
    fn root(&self, b: Base) -> Vec<u8> {
        match b {
            Base::Home => shell::env_or_empty("HOME"),
            Base::Desk => desk_dir(),
            Base::DeskHome => desk_home(),
            Base::Skel => b"/etc/skel".to_vec(),
            Base::Omarchy => omarchy_or(self.omarchy),
        }
    }

    /// BASE/REL when SEED_ALLOW has it, else None.
    pub fn src(&self, base: Base, rel: &[u8]) -> Option<Src> {
        let e = SEED_ALLOW
            .iter()
            .find(|e| e.base == base && allowed(e.rel.as_bytes(), rel))?;
        let root = self.root(e.base);
        Some(Src(if rel == b"." {
            root
        } else {
            cat!(root, "/", rel)
        }))
    }
}

/// Whether REL is what PATTERN (an entry's rel) allows: part for part, no empty, `.` or `..` one.
fn allowed(pattern: &[u8], rel: &[u8]) -> bool {
    if pattern == b"." || rel == b"." {
        return pattern == rel;
    }
    let ps: Vec<&[u8]> = pattern.split(|&c| c == b'/').collect();
    let rs: Vec<&[u8]> = rel.split(|&c| c == b'/').collect();
    ps.len() == rs.len()
        && ps.iter().zip(&rs).all(|(pat, r)| {
            if r.is_empty() || *r == b"." || *r == b".." {
                return false;
            }
            match pat.strip_prefix(b"*") {
                Some(end) => r[0] != b'.' && r.ends_with(end),
                None => pat == r,
            }
        })
}

/// A path in the box HOME: relative, no `..` (home_unlink refused one: "no .. in a HOME path").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dest(Vec<u8>);

impl Dest {
    pub fn new(rel: &[u8]) -> Option<Dest> {
        let ok = !rel.is_empty()
            && rel[0] != b'/'
            && rel.split(|&c| c == b'/').all(|c| c != b"..")
            && rel
                .split(|&c| c == b'/')
                .any(|c| !c.is_empty() && c != b".");
        ok.then(|| Dest(rel.to_vec()))
    }

    /// One of omabox's own fixed paths.
    fn of(rel: &str) -> Dest {
        debug_assert!(Dest::new(rel.as_bytes()).is_some());
        Dest(rel.as_bytes().to_vec())
    }

    /// DEST/NAME (a name from a folder's listing, a plugin's id or a theme's).
    fn join(&self, name: &[u8]) -> Option<Dest> {
        Dest::new(&cat!(self.0, "/", name))
    }

    pub fn rel(&self) -> &[u8] {
        &self.0
    }

    /// HOME/DEST on the host.
    fn on(&self, h: &[u8]) -> Vec<u8> {
        cat!(h, "/", self.0)
    }
}

/// What seed_home reads of up's state, as cmd_up has it by then.
#[derive(Default)]
pub struct SeedPlan {
    /// FROM: the box starts from a save (its HOME copied in already): no /etc/skel.
    pub from: bool,
    /// STOCK_BAR (--stock-bar, or --clean without --keep bar): Omarchy's shell.json, no shell.toml.
    pub stock_bar: bool,
    /// DESK_SHELL (--desk-shell, #206), DESK (desk_config's folder, resolved), DESK_SHOWN (how it is
    /// said) and DESK_IDS (the desk's plugins in PLUGINS).
    pub desk_shell: bool,
    pub desk: Vec<u8>,
    pub desk_shown: Vec<u8>,
    pub desk_ids: Vec<Vec<u8>>,
    /// PLUGINS: (id, dir) each, the --plugin ones then the desk's.
    pub plugins: Vec<(Vec<u8>, Vec<u8>)>,
    /// LINKED_PLUGINS: the ids the box sees at their own path (finding 241): linked, not mounted.
    pub linked: Vec<Vec<u8>>,
    /// THEME (--theme, named as omarchy-theme-set names it) and THEME_DIRS (--theme-dir's, resolved).
    pub theme: Vec<u8>,
    pub theme_dirs: Vec<Vec<u8>>,
    /// NO_SEED: --no-seed's list, comma-separated (#231).
    pub no_seed: Vec<u8>,
    /// CLEAN (--clean, #252) and CLEAN_KEEP, comma-separated.
    pub clean: bool,
    pub clean_keep: Vec<u8>,
    /// --omarchy's tree, or empty for /usr/share/omarchy.
    pub omarchy: Vec<u8>,
}

impl SeedPlan {
    fn no_seed(&self, what: &str) -> bool {
        self.no_seed
            .split(|&c| c == b',')
            .any(|w| w == what.as_bytes())
    }

    /// clean_has WHAT.
    pub fn clean_has(&self, what: &str) -> bool {
        self.clean_keep
            .split(|&c| c == b',')
            .any(|w| w == what.as_bytes())
    }
}

fn listed(list: &[Vec<u8>], id: &[u8]) -> bool {
    list.iter().any(|x| x == id)
}

fn omarchy_or(o: &[u8]) -> Vec<u8> {
    if o.is_empty() {
        b"/usr/share/omarchy".to_vec()
    } else {
        o.to_vec()
    }
}

fn p(v: &[u8]) -> PathBuf {
    shell::path(v)
}

fn is_link(v: &[u8]) -> bool {
    p(v).is_symlink()
}

/// `[ -e P ] || [ -L P ]`.
fn there(v: &[u8]) -> bool {
    std::fs::symlink_metadata(p(v)).is_ok()
}

/// `rm -rf -- P`: what is there, never what a link leads to.
fn rm_rf(v: &[u8]) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p(v)) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p(v)),
        Ok(_) => std::fs::remove_file(p(v)),
        Err(_) => Ok(()),
    }
}

/// `rm -f -- P` (a folder left).
fn rm_f(v: &[u8]) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p(v)) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => std::fs::remove_file(p(v)),
        Err(_) => Ok(()),
    }
}

/// What bash's `set -e` ended up on (a mkdir, rm or ln that failed), in omabox's words.
fn failed(ctx: &Ctx, what: &[u8], e: &std::io::Error) -> Exit {
    ctx.die(cat!(
        "up: could not ",
        what,
        " in the box HOME: ",
        e.to_string()
    ))
}

/// `mkdir -p -- P`.
fn mkdir_p(ctx: &Ctx, v: &[u8]) -> Res<()> {
    std::fs::create_dir_all(p(v)).map_err(|e| failed(ctx, &cat!("make ", v), &e))
}

/// `cat > F`: F written whole.
fn write(ctx: &Ctx, f: &[u8], v: &[u8]) -> Res<()> {
    std::fs::write(p(f), v).map_err(|e| failed(ctx, &cat!("write ", f), &e))
}

/// `mv -- A B`.
fn mv(ctx: &Ctx, a: &[u8], b: &[u8]) -> Res<()> {
    std::fs::rename(p(a), p(b)).map_err(|e| failed(ctx, &cat!("move ", a, " to ", b), &e))
}

/// A child's status that ended bash's `set -e` there (its words said by it).
fn status_end(rc: i32) -> Exit {
    Exit(u8::try_from(rc).unwrap_or(1))
}

/// `cp ARGS...`'s status, its words dropped (`2>/dev/null`) or the caller's.
fn cp(args: &[&[u8]], quiet: bool) -> i32 {
    let mut c = crate::sig::command("cp");
    c.args(args.iter().map(|a| p(a))).stdin(Stdio::null());
    if quiet {
        c.stderr(Stdio::null());
    }
    crate::sig::run_status(c).unwrap_or(127)
}

/// `cp SRC DIR/ 2>/dev/null || true`: a file into a folder, a failure let be.
fn cp_into(src: &Src, dir: &[u8]) {
    cp(&[src.path(), &cat!(dir, "/")], true);
}

/// `grep ARGS...`'s status, quiet, in the C locale (omabox's own patterns in ASCII, finding 445).
fn grep(args: &[&[u8]]) -> i32 {
    let mut c = crate::sig::command("grep");
    c.args(args.iter().map(|a| p(a)))
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::sig::run_status(c).unwrap_or(2)
}

/// Where the producing tar of a pipe runs.
enum Cwd<'a> {
    Path(&'a [u8]),
    /// A folder held open (seed_copy_in, finding 257), run from by fchdir.
    Held(&'a OwnedFd),
}

/// The child runs from the folder FD holds.
fn from_held(c: &mut std::process::Command, fd: &OwnedFd) {
    let raw = fd.as_raw_fd();
    // SAFETY: fchdir is async-signal-safe; RAW stays open for the whole spawn (the caller holds
    // it), and only the child's cwd changes.
    unsafe {
        c.pre_exec(move || {
            if libc::fchdir(raw) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

/// `(cd CWD && tar CREATE...) | tar -C TO XFLAG -`: pipefail's status (the last that failed). QUIET:
/// both tars' words dropped.
fn tar_pipe(cwd: Cwd, create: &[&[u8]], to: &[u8], xflag: &str, quiet: bool) -> i32 {
    let mut c = crate::sig::command("tar");
    c.args(create.iter().map(|a| p(a)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    if quiet {
        c.stderr(Stdio::null());
    }
    match cwd {
        Cwd::Path(d) => {
            c.current_dir(p(d));
        }
        Cwd::Held(fd) => from_held(&mut c, fd),
    }
    let Ok(mut prod) = c.spawn() else {
        return 1;
    };
    let mut x = crate::sig::command("tar");
    x.arg("-C").arg(p(to)).arg(xflag).arg("-");
    match prod.stdout.take() {
        Some(o) => x.stdin(Stdio::from(o)),
        None => x.stdin(Stdio::null()),
    };
    if quiet {
        x.stderr(Stdio::null());
    }
    let rx = crate::sig::run_status(x).unwrap_or(127);
    let rc = prod.wait().map_or(1, |s| crate::exec::status(s).0);
    if rx != 0 { rx } else { rc }
}

/// The folder at PATH held open, when `cd PATH && pwd -P` is PATH itself (finding 257: a part of it
/// swapped for a link since it was checked is not followed).
fn held_dir(path: &[u8]) -> Option<OwnedFd> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(p(path))
        .ok()?;
    let fd = OwnedFd::from(f);
    let at = std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd())).ok()?;
    (shell::bytes(&at) == path).then_some(fd)
}

/// home_unlink HOME DEST: a link or FIFO at HOME/DEST, or at any folder on the way, removed (the
/// entry itself, never what it points at), so what is then written at HOME/DEST lands in HOME
/// (finding 168). Each part looked at only once those above it are real folders.
pub fn home_unlink(ctx: &Ctx, h: &[u8], d: &Dest) -> Res<()> {
    let mut at = h.to_vec();
    for part in d.0.split(|&c| c == b'/') {
        if part.is_empty() || part == b"." {
            continue;
        }
        at = cat!(at, "/", part);
        if let Ok(m) = std::fs::symlink_metadata(p(&at)) {
            if m.file_type().is_symlink() || m.file_type().is_fifo() {
                std::fs::remove_file(p(&at)).map_err(|e| failed(ctx, &cat!("remove ", at), &e))?;
            }
        }
        if !p(&at).is_dir() {
            break;
        }
    }
    Ok(())
}

/// home_link HOME DEST TARGET: a link at HOME/DEST to TARGET (a path as the box sees it), in place of
/// whatever is there; links on the way cleared first (finding 168), parents made.
pub fn home_link(ctx: &Ctx, h: &[u8], d: &Dest, target: &[u8]) -> Res<()> {
    home_unlink(ctx, h, d)?;
    let at = d.on(h);
    if let Some(parent) = p(&at).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| failed(ctx, &cat!("make the folder of ", at), &e))?;
    }
    rm_rf(&at).map_err(|e| failed(ctx, &cat!("remove ", at), &e))?;
    std::os::unix::fs::symlink(p(target), p(&at)).map_err(|e| failed(ctx, &cat!("link ", at), &e))
}

/// desk_dir: the desk's Omarchy config folder the bar and extensions come from (#231):
/// OMABOX_DESK_CONFIG, the suite's fake desk, never a jailed caller's; else ~/.config/omarchy.
pub fn desk_dir() -> Vec<u8> {
    match shell::env("OMABOX_DESK_CONFIG") {
        Some(d) if !d.is_empty() && !in_jail() => d,
        _ => cat!(shell::env_or_empty("HOME"), "/.config/omarchy"),
    }
}

/// desk_home: the desk's HOME whose font folders seed_fonts mounts (#261): OMABOX_DESK_HOME, the
/// suite's fake desk, never a jailed caller's; else HOME.
pub fn desk_home() -> Vec<u8> {
    match shell::env("OMABOX_DESK_HOME") {
        Some(d) if !d.is_empty() && !in_jail() => d,
        _ => shell::env_or_empty("HOME"),
    }
}

/// seed_copy SRC HOME DEST: one of the user's folders into the box HOME. The folder may be a link
/// (dotfiles), but links inside it stay links, to host paths the box does not have (finding 74).
/// No hidden files or node_modules, nothing refuse_src keeps out but the look (`look`). DEST is
/// cleared of links first (finding 168); a link a save left inside where the copy writes is replaced,
/// not followed (GNU tar's way at a folder it makes, and at a file). SKIP: paths in the folder (as
/// `NAME/...` from its top) left out besides.
pub fn seed_copy(ctx: &Ctx, src: &Src, h: &[u8], d: &Dest, skip: &[Vec<u8>]) -> Res<()> {
    let Some(s) = readlink_f(src.path()).filter(|s| p(s).is_dir()) else {
        return Ok(());
    };
    if refuse_src_look(&s).is_some() {
        return Ok(());
    }
    home_unlink(ctx, h, d)?;
    let to = d.on(h);
    mkdir_p(ctx, &to)?;
    // (`set -- *`: its entries but the hidden ones; none, nothing to copy)
    let names = shell::glob(&p(&s), false);
    if names.is_empty() {
        return Ok(());
    }
    let mut args: Vec<Vec<u8>> = [
        "--exclude=.[!.]*",
        "--exclude=node_modules",
        "--exclude=api-keys.env",
    ]
    .iter()
    .map(|a| a.as_bytes().to_vec())
    .collect();
    if !skip.is_empty() {
        args.push(b"--anchored".to_vec());
        args.push(b"--no-wildcards".to_vec());
        args.extend(skip.iter().map(|k| cat!("--exclude=", k)));
    }
    args.extend([b"-cf".to_vec(), b"-".to_vec(), b"--".to_vec()]);
    args.extend(names);
    let a: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
    // (`|| true`: what tar could not copy stays out, unsaid)
    tar_pipe(Cwd::Path(&s), &a, &to, "-xf", true);
    Ok(())
}

/// seed_theme: the user's current theme, by seed_copy (finding 171: its links stay links). A save's
/// goes first, or the copy would merge with it.
fn seed_theme(ctx: &Ctx, look: &Look, h: &[u8]) -> Res<()> {
    let d = Dest::of(".local/state/omarchy/current/theme");
    let Some(src) = look.src(Base::Home, d.rel()) else {
        return Err(ctx.die("up: the current theme is not on seed_home's list"));
    };
    home_unlink(ctx, h, &d)?;
    let dst = d.on(h);
    rm_rf(&dst).map_err(|e| failed(ctx, &cat!("remove ", dst), &e))?;
    seed_copy(ctx, &src, h, &d, &[])?;
    let copied = std::fs::read_dir(p(&dst)).is_ok_and(|mut r| r.next().is_some());
    if !copied {
        return Err(ctx.die(cat!(
            "could not copy your current theme (",
            src.path(),
            ") into the box HOME (",
            dst,
            ")"
        )));
    }
    Ok(())
}

const PICS: [&str; 6] = [".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp"];

/// Whether NAME is a wallpaper's, as omarchy-theme-set and omarchy-theme-bg-next pick them: not
/// hidden, jpg, jpeg, png, gif, bmp or webp in any case (ASCII, finding 445).
fn wallpaper_name(n: &[u8]) -> bool {
    let l = n.to_ascii_lowercase();
    n.first().is_some_and(|&c| c != b'.')
        && PICS
            .iter()
            .any(|e| l.len() > e.len() && l.ends_with(e.as_bytes()))
}

/// seed_backgrounds THEME HOME [FIRST]: the user's own wallpapers for THEME
/// (~/.config/omarchy/backgrounds/THEME) into the box HOME at the same place (#184). Regular files
/// named as a wallpaper, none a link, each up to 64 MiB and 256 MiB in all, FIRST before the rest;
/// backgrounds/ and the folder are not followed when they are links. A save's folder goes first.
/// The names copied, in byte order where bash's glob had the locale's (finding 446).
pub fn seed_backgrounds(
    ctx: &Ctx,
    look: &Look,
    theme: &[u8],
    h: &[u8],
    first: &[u8],
) -> Res<Vec<Vec<u8>>> {
    let mut names = Vec::new();
    if !shell::re_match("^[a-zA-Z0-9][a-zA-Z0-9._-]*$", theme) {
        return Ok(names);
    }
    let Some(d) = Dest::new(&cat!(".config/omarchy/backgrounds/", theme)) else {
        return Ok(names);
    };
    let src = cat!(shell::env_or_empty("HOME"), "/.config/omarchy/backgrounds");
    let dst = d.on(h);
    home_unlink(ctx, h, &d)?;
    rm_rf(&dst).map_err(|e| failed(ctx, &cat!("remove ", dst), &e))?;
    let folder = cat!(src, "/", theme);
    if is_link(&src) || is_link(&folder) || !p(&folder).is_dir() {
        return Ok(names);
    }
    mkdir_p(ctx, &dst)?;
    let mut list: Vec<Vec<u8>> = Vec::new();
    if !first.is_empty() {
        list.push(first.to_vec());
    }
    list.extend(shell::glob(&p(&folder), false));
    let mut total: u64 = 0;
    for n in list {
        let Ok(m) = std::fs::symlink_metadata(p(&cat!(folder, "/", n))) else {
            continue;
        };
        let to = cat!(dst, "/", n);
        if !m.is_file() || there(&to) || !wallpaper_name(&n) {
            continue;
        }
        let Some(s) = look.src(Base::Home, &cat!(d.rel(), "/", n)) else {
            continue;
        };
        let size = m.len();
        if size > 64 << 20 || total + size > 256 << 20 {
            continue;
        }
        // -P: a file swapped for a link since the check is copied as that link (dangling in the box).
        if cp(&[b"-P", b"--reflink=auto", b"--", s.path(), &to], true) != 0 {
            continue;
        }
        total += size;
        names.push(n);
    }
    Ok(names)
}

/// box_background HOME THEME BG: the box's current/background link (#184) for the desk's link BG on
/// theme THEME, as omarchy-theme-set would have made it in the box: to the box's copy of the theme or
/// of the user's folder for it, never a host path; else the first background the box has (by byte
/// order where bash's sort had the locale's, finding 446); no link on the desk, none in the box.
pub fn box_background(ctx: &Ctx, look: &Look, h: &[u8], t: &[u8], bg: &[u8]) -> Res<()> {
    let home = shell::env_or_empty("HOME");
    let sbx = "/home/sbx";
    let cur = ".local/state/omarchy/current";
    let oc = cat!(home, "/.config/omarchy");
    let o = readlink_f(&oc).unwrap_or(oc);
    let mut n: Vec<u8> = Vec::new();
    if !t.is_empty() {
        for pre in [
            cat!(home, "/.config/omarchy/backgrounds/", t, "/"),
            cat!(o, "/backgrounds/", t, "/"),
        ] {
            if let Some(rest) = bg.strip_prefix(pre.as_slice()) {
                if !rest.contains(&b'/') {
                    n = rest.to_vec();
                    break;
                }
            }
        }
    }
    let names = if t.is_empty() {
        Vec::new()
    } else {
        seed_backgrounds(ctx, look, t, h, &n)?
    };
    let link = cat!(h, "/", cur, "/background");
    let rm = |ctx: &Ctx| rm_f(&link).map_err(|e| failed(ctx, &cat!("remove ", link), &e));
    if bg.is_empty() {
        return rm(ctx);
    }
    // (`${BG#*/current/theme/}`, when BG has it)
    let theme_part = |b: &[u8]| -> Option<Vec<u8>> {
        let k = b"/current/theme/";
        let i = b.windows(k.len()).position(|w| w == k)?;
        Some(b[i + k.len()..].to_vec())
    };
    let mut to: Vec<u8> = Vec::new();
    if !n.is_empty() && names.contains(&n) {
        to = cat!(sbx, "/.config/omarchy/backgrounds/", t, "/", n);
    } else if let Some(rest) = theme_part(bg).filter(|r| there(&cat!(h, "/", cur, "/theme/", r))) {
        to = cat!(sbx, "/", cur, "/theme/", rest);
    } else {
        // (find's: regular files, no links, hidden ones too, a wallpaper's ending in any case)
        let mut found: Vec<Vec<u8>> = Vec::new();
        let mut starts: Vec<Vec<u8>> = Vec::new();
        if !t.is_empty() {
            starts.push(cat!(".config/omarchy/backgrounds/", t, "/"));
        }
        starts.push(cat!(cur, "/theme/backgrounds/"));
        for s in starts {
            let Ok(rd) = std::fs::read_dir(p(&cat!(h, "/", s))) else {
                continue;
            };
            for e in rd.flatten() {
                let name = shell::bytes(std::path::Path::new(&e.file_name())).to_vec();
                let l = name.to_ascii_lowercase();
                let pic = PICS.iter().any(|x| l.ends_with(x.as_bytes()));
                if pic && e.file_type().is_ok_and(|f| f.is_file()) {
                    found.push(cat!(s, name));
                }
            }
        }
        found.sort();
        if let Some(f) = found.first() {
            to = cat!(sbx, "/", f);
        }
    }
    if to.is_empty() {
        return rm(ctx);
    }
    // (ln -sfn: the link replaced; at a real folder, made inside it, as ln does)
    let at = if p(&link).is_dir() && !is_link(&link) {
        cat!(link, "/", shell::basename(&to))
    } else {
        link.clone()
    };
    rm_f(&at).map_err(|e| failed(ctx, &cat!("remove ", at), &e))?;
    std::os::unix::fs::symlink(p(&to), p(&at)).map_err(|e| failed(ctx, &cat!("link ", at), &e))
}

/// `git -C DIR rev-parse ARGS...`: its answer, or None.
fn git_rev_parse(dir: &[u8], args: &[&str]) -> Option<Vec<u8>> {
    let mut c = crate::sig::command("git");
    c.arg("-C").arg(p(dir)).arg("rev-parse").args(args);
    c.stdin(Stdio::null()).stderr(Stdio::null());
    match crate::sig::run_output(c) {
        Ok((0, out)) => Some(shell::strip_nl(&out)),
        _ => None,
    }
}

/// theme_snapshot_note THEME_DIR TOP (#170): a theme seed_home copied from a link into TOP (the repo
/// up runs from) or into another checkout of it is a snapshot, made now; --theme-dir gives a live one.
pub fn theme_snapshot_note(dir: &[u8], top: &[u8]) {
    let link = dir.strip_suffix(b"/").unwrap_or(dir);
    if !is_link(link) || top.is_empty() {
        return;
    }
    let Some(t) = readlink_f(link) else { return };
    let name = shell::basename(link);
    if t.starts_with(&cat!(top, "/")) {
        say(cat!(
            "note: theme ",
            name,
            " is a copy of ",
            t,
            " made now: edits after this do not reach the box. up --theme-dir ",
            t,
            " gives a live one"
        ));
        return;
    }
    // Another checkout: git is asked only outside ai-jail (repo_top's reason: a jail's .git/config
    // runs here).
    if in_jail() {
        return;
    }
    let common = ["--path-format=absolute", "--git-common-dir"];
    let (Some(tc), Some(cc)) = (git_rev_parse(&t, &common), git_rev_parse(top, &common)) else {
        return;
    };
    if tc != cc {
        return;
    }
    let Some(ttop) = git_rev_parse(&t, &["--show-toplevel"]) else {
        return;
    };
    let inner = t.strip_prefix(cat!(ttop, "/").as_slice()).unwrap_or(&t);
    let mine = cat!(top, "/", inner);
    let own = if p(&mine).is_dir() {
        mine
    } else {
        b"DIR".to_vec()
    };
    say(cat!(
        "note: theme ",
        name,
        " is a copy of ",
        t,
        ", in another checkout (",
        ttop,
        "), not this one (",
        top,
        "), made now. up --theme-dir ",
        own,
        " gives a live one"
    ));
}

/// `sed 's/[][\.*^$+?(){}|/]/\\&/g'`: a name as a regex of itself.
fn re_escape(n: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    for &c in n {
        if b"[]\\.*^$+?(){}|/".contains(&c) {
            o.push(b'\\');
        }
        o.push(c);
    }
    o
}

/// What of the desk's extensions/ (SRC) stays out of the box by what it holds, as seed_copy's SKIP
/// (`NAME/...` from its top): the one place a content check of it goes. None yet: bash's copies it
/// whole.
fn extensions_held_back(_src: &[u8]) -> Vec<Vec<u8>> {
    Vec::new()
}

/// seed_extensions HOME: the desk's extensions/ (plugins' settings) into the box HOME, unless
/// --no-seed extensions. Each of its files a --plugin's files name ("extensions/NAME") is said (#231):
/// that plugin's settings in the box are the desk's. Not a --desk-shell plugin's (that box is the
/// desk as it is).
pub fn seed_extensions(ctx: &Ctx, look: &Look, plan: &SeedPlan, h: &[u8]) -> Res<()> {
    if plan.no_seed("extensions") {
        return Ok(());
    }
    let Some(src) = look.src(Base::Desk, b"extensions") else {
        return Ok(());
    };
    let d = Dest::of(".config/omarchy/extensions");
    seed_copy(ctx, &src, h, &d, &extensions_held_back(src.path()))?;
    let Some(s) = readlink_f(src.path()).filter(|s| p(s).is_dir()) else {
        return Ok(());
    };
    if refuse_src_look(&s).is_some() {
        return Ok(());
    }
    let home_ext = cat!(shell::env_or_empty("HOME"), "/.config/omarchy/extensions");
    let shown = if src.path() == home_ext.as_slice() {
        b"your desk".to_vec()
    } else {
        let s = src.path();
        s.strip_suffix(b"/extensions").unwrap_or(s).to_vec()
    };
    for n in shell::glob(&p(&s), false) {
        if n == b"node_modules" || n == b"api-keys.env" {
            continue; // (seed_copy leaves them out)
        }
        if !there(&cat!(d.on(h), "/", n)) {
            continue;
        }
        let pat = cat!("extensions/", re_escape(&n), "([^A-Za-z0-9_.-]|$)");
        let mut ids: Vec<&[u8]> = Vec::new();
        for (id, dir) in &plan.plugins {
            if listed(&plan.desk_ids, id) {
                continue;
            }
            let rc = grep(&[
                b"-rqsIE",
                b"--exclude-dir=.git",
                b"--exclude-dir=node_modules",
                b"-e",
                &pat,
                b"--",
                &cat!(dir, "/"),
            ]);
            if rc == 0 {
                ids.push(id);
            }
        }
        let Some(first) = ids.first() else { continue };
        let line = if ids.len() == 1 {
            cat!("plugin ", first, " reads it")
        } else {
            cat!("plugins ", ids.join(&b' '), " read it")
        };
        say(cat!(
            "up: seeded extensions/",
            n,
            " from ",
            shown,
            ": ",
            line,
            " (--no-seed extensions starts without it)"
        ));
    }
    Ok(())
}

/// seed_fonts HOME (#261, finding 380): the desk's user fonts, ~/.local/share/fonts and ~/.fonts,
/// each to be mounted read-only at the same place in the box HOME (there from the box's first
/// process, so fontconfig finds them). Not with --no-seed fonts, nor in a --clean box unless --keep
/// fonts. Said: what goes in, what does not and why, and the links in them that lead out of them
/// (nowhere in the box). DESK_FONTS: (the folder resolved, its place in the box HOME) each.
pub fn seed_fonts(ctx: &Ctx, plan: &SeedPlan, h: &[u8]) -> Res<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut fonts = Vec::new();
    if plan.no_seed("fonts") || plan.clean && !plan.clean_has("fonts") {
        return Ok(fonts);
    }
    let look = Look {
        omarchy: &plan.omarchy,
    };
    let dh = desk_home();
    let home = shell::env_or_empty("HOME");
    let mut mounted: Vec<Vec<u8>> = Vec::new();
    for rel in [".local/share/fonts", ".fonts"] {
        let Some(src) = look.src(Base::DeskHome, rel.as_bytes()) else {
            continue;
        };
        let src = src.path().to_vec();
        let shown = if dh == home {
            cat!("~/", rel)
        } else {
            src.clone()
        };
        if !there(&src) {
            continue;
        }
        let Some(r) = readlink_e(&src).filter(|r| p(r).is_dir()) else {
            say(cat!(
                "up: not mounting ",
                shown,
                ": not a folder (or a link to none)"
            ));
            continue;
        };
        if let Some(why) = refuse_src(&src) {
            say(cat!("up: not mounting ", shown, ": ", why));
            continue;
        }
        let d = Dest::of(rel);
        home_unlink(ctx, h, &d)?;
        if std::fs::read_dir(p(&d.on(h))).is_ok_and(|mut e| e.next().is_some()) {
            say(cat!(
                "up: not mounting ",
                shown,
                ": the box HOME has a ",
                rel,
                " of its own (the save's or a --seed's), which it keeps"
            ));
            continue;
        }
        mkdir_p(ctx, &d.on(h))?;
        fonts.push((r.clone(), rel.as_bytes().to_vec()));
        mounted.push(shown.clone());
        let (mut n, mut first) = (0usize, Vec::new());
        links_out(&r, &r, &mut n, &mut first);
        if n != 1 {
            first.extend_from_slice(b", ...");
        }
        if n > 0 {
            say(cat!(
                "up: ",
                n.to_string(),
                " link(s) in ",
                shown,
                " lead out of it, so they lead nowhere in the box (",
                first,
                ")"
            ));
        }
    }
    if !mounted.is_empty() {
        say(cat!(
            "up: your desk's fonts are in the box, read-only: ",
            mounted.join(&b' '),
            " (--no-seed fonts starts without them)"
        ));
    }
    Ok(fonts)
}

/// The links under DIR (find -type l, in find's order: each folder's entries as read, a folder
/// walked where it is met) that lead out of ROOT and not into /usr: how many, and the first as
/// "REL -> TARGET". A link broken on the desk too is left out.
fn links_out(root: &[u8], dir: &[u8], n: &mut usize, first: &mut Vec<u8>) {
    let Ok(rd) = std::fs::read_dir(p(dir)) else {
        return;
    };
    for e in rd.flatten() {
        let l = cat!(dir, "/", shell::bytes(std::path::Path::new(&e.file_name())));
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            let Some(t) = readlink_e(&l) else { continue };
            let t2 = cat!(t, "/");
            if t2.starts_with(&cat!(root, "/")) || t2.starts_with(b"/usr/") {
                continue;
            }
            *n += 1;
            if first.is_empty() {
                let rel = l.strip_prefix(cat!(root, "/").as_slice()).unwrap_or(&l);
                *first = cat!(rel, " -> ", t);
            }
        } else if ft.is_dir() {
            links_out(root, &l, n, first);
        }
    }
}

/// A file's lines as grep and sed read them (the last one need not end in a newline).
fn text_lines(t: &[u8]) -> Vec<&[u8]> {
    let mut v: Vec<&[u8]> = t.split(|&c| c == b'\n').collect();
    if t.ends_with(b"\n") || t.is_empty() {
        v.pop();
    }
    v
}

/// bar_seed_note DESK KEEP NOTIFY PAGERS SPLIT (#250): what up says when the box's bar is the desk's
/// and not --stock-bar's: its layout, when the desk's shell.json made fit has another `bar` than
/// Omarchy's made fit the same way, and its shell.toml, when that sets anything, by its tables.
/// Names only, never a value.
fn bar_seed_note(
    desk: &[u8],
    omarchy: &[u8],
    keep: &[u8],
    notify: bool,
    pagers: &[u8],
    split: bool,
) {
    let stock = cat!(omarchy_or(omarchy), "/config/omarchy/shell.json");
    let mut what: Vec<Vec<u8>> = Vec::new();
    let dj = cat!(desk, "/shell.json");
    if p(&dj).is_file() && p(&stock).is_file() {
        let bar = |f: &[u8]| -> Vec<u8> {
            let Ok(input) = std::fs::read(p(f)) else {
                return Vec::new();
            };
            let r = shelljson::shell_json_filter(keep, notify, pagers, split, &input);
            if !r.ok() {
                return Vec::new();
            }
            let b = crate::jq::tool(
                "jq",
                &[b"-cS".as_slice(), b".bar // null"],
                Some(&r.out),
                crate::jq::Err::Null,
            );
            if b.ok() { b.text() } else { Vec::new() }
        };
        let (mine, theirs) = (bar(&dj), bar(&stock));
        if !mine.is_empty() && mine != b"null" && mine != theirs {
            what.push(b"shell.json layout".to_vec());
        }
    }
    let toml = cat!(desk, "/shell.toml");
    if let Some(t) = std::fs::read(p(&toml)).ok().filter(|_| p(&toml).is_file()) {
        let lines = text_lines(&t);
        if lines
            .iter()
            .any(|l| !shell::re_match("^[[:space:]]*(#.*)?$", l))
        {
            let mut tables: Vec<Vec<u8>> = Vec::new();
            for l in &lines {
                if let Some(name) = shell::re_group(
                    r"^[[:space:]]*\[+[[:space:]]*([A-Za-z0-9_.-]+)[[:space:]]*\]+",
                    l,
                    1,
                ) {
                    let t = cat!("[", name, "]");
                    if !tables.contains(&t) {
                        tables.push(t);
                    }
                }
            }
            let mut w = b"shell.toml".to_vec();
            if !tables.is_empty() {
                w = cat!(w, " ", tables.join(&b", "[..]));
            }
            what.push(w);
        }
    }
    let Some(w0) = what.first() else { return };
    let shown = if desk == cat!(shell::env_or_empty("HOME"), "/.config/omarchy").as_slice() {
        b"your desk".to_vec()
    } else {
        desk.to_vec()
    };
    let more = what.get(1).map(|w| cat!(" and ", w)).unwrap_or_default();
    say(cat!(
        "up: seeded the bar from ",
        shown,
        ": its ",
        w0,
        more,
        " (--stock-bar starts from Omarchy's default)"
    ));
}

/// desk_name_refused NAME: why a desk file or folder stays out of a box by its name, or None: a
/// secret's, a git dir (its config can hold credentials), a backup. Lowered as Unicode where the
/// name is UTF-8 (`${1,,}` in a UTF-8 locale: the Kelvin sign is a `k`), else as ASCII.
pub fn desk_name_refused(name: &[u8]) -> Option<&'static str> {
    let n: Vec<u8> = match std::str::from_utf8(name) {
        Ok(s) => s.to_lowercase().into_bytes(),
        Err(_) => name.to_ascii_lowercase(),
    };
    let has = |x: &&str| crate::exit::contains(&n, x.as_bytes());
    let ends = |x: &&str| n.ends_with(x.as_bytes());
    let starts = |x: &&str| n.starts_with(x.as_bytes());
    let secret = n == b"api-keys.env"
        || ends(&".env")
        || starts(&".env.")
        || [
            "key",
            "token",
            "secret",
            "credential",
            "passw",
            "auth",
            "cookie",
            "session",
            "private",
        ]
        .iter()
        .any(has)
        || [".pem", ".p12", ".pfx", ".gpg", ".asc", ".kdbx"]
            .iter()
            .any(ends)
        || ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"]
            .iter()
            .any(starts)
        || n == b".netrc"
        || n == b".pgpass";
    if secret {
        return Some("a secret's name");
    }
    if n == b".git" {
        return Some("a git dir (its config can hold credentials)");
    }
    // (`*.bak[-._0-9]*`)
    let bak_more = n
        .windows(5)
        .any(|w| w.starts_with(b".bak") && (w[4].is_ascii_digit() || b"-._".contains(&w[4])));
    if bak_more || [".bak", "~", ".swp", ".orig", ".old"].iter().any(ends) {
        return Some("a backup");
    }
    None
}

/// What desk_content_refused greps for: a key, token, secret, password or credential given a value
/// (the key ends in the word, the value 6+ characters: finding 320), a bearer token, a private key,
/// or a token-shaped string (GitHub, GitLab, Slack, OpenAI and Anthropic, AWS).
const SECRET_ERE: &str = r#"(api[_-]?key|secret([_-]?key)?|token|passw(or)?d|credentials?|authorization)["']?[[:space:]]*[:=][[:space:]]*["']?[^"',[:space:]]{6,}|bearer[[:space:]]+[A-Za-z0-9._-]{16,}|-----BEGIN [A-Z ]*PRIVATE KEY|(^|[^A-Za-z0-9])(ghp|gho|ghs|ghu|ghr|github_pat|glpat|xox[abpr]|sk|sk-ant)[-_][A-Za-z0-9_-]{16,}|AKIA[0-9A-Z]{16}"#;

/// desk_content_refused FILE: why a desk file stays out of a box by what it holds, or None; one that
/// cannot be read stays out too. Nothing of what matched is said. grep runs it as bash's does
/// (`LC_ALL=C grep -aqiE`): the same regex engine, so the same answer.
pub fn desk_content_refused(file: &[u8]) -> Option<&'static str> {
    match grep(&[b"-aqiE", b"--", SECRET_ERE.as_bytes(), file]) {
        0 => Some("it holds what looks like a key, token or password"),
        1 => None,
        _ => Some("it could not be read"),
    }
}

/// desk_files' walk: its caps and what it says.
struct DeskWalk<'a> {
    ctx: &'a Ctx,
    root: Vec<u8>,
    h: Option<&'a [u8]>,
    total: u64,
    count: u64,
    out: Vec<u8>,
}

impl DeskWalk<'_> {
    fn skipped(&mut self, r: &[u8], why: &[u8]) {
        self.out.extend(cat!("skipped ", r, "\t", why, "\n"));
    }

    /// The entries of DESK/REL: `*`, then `.[!.]*`, then `..?*`, each in byte order (finding 446).
    fn walk(&mut self, rel: &[u8], depth: u32) -> Res<()> {
        let dir = if rel.is_empty() {
            self.root.clone()
        } else {
            cat!(self.root, "/", rel)
        };
        let mut all: Vec<Vec<u8>> = std::fs::read_dir(p(&dir))
            .map(|rd| {
                rd.flatten()
                    .map(|e| shell::bytes(std::path::Path::new(&e.file_name())).to_vec())
                    .collect()
            })
            .unwrap_or_default();
        all.sort();
        all.sort_by_key(|n| match n.as_slice() {
            [b'.', b'.', ..] => 2,
            [b'.', ..] => 1,
            _ => 0,
        });
        for name in all {
            self.entry(rel, &dir, &name, depth)?;
        }
        Ok(())
    }

    fn entry(&mut self, rel: &[u8], dir: &[u8], name: &[u8], depth: u32) -> Res<()> {
        let e = cat!(dir, "/", name);
        let mut r = if rel.is_empty() {
            name.to_vec()
        } else {
            cat!(rel, "/", name)
        };
        if rel.is_empty() {
            match name {
                b"shell.json" | b"plugins" | b"themes" | b"backgrounds" | b"branding"
                | b"extensions" => return Ok(()),
                b"hooks" => {
                    self.skipped(b"hooks/", b"your scripts (Omarchy runs them on its events)");
                    return Ok(());
                }
                _ => {}
            }
        }
        if let Some(why) = desk_name_refused(name) {
            if !is_link(&e) && p(&e).is_dir() {
                r.push(b'/');
            }
            self.skipped(&r, why.as_bytes());
            return Ok(());
        }
        let mut t = e.clone();
        if is_link(&e) {
            let Some(to) = readlink_e(&e) else {
                self.skipped(&r, b"a link that leads nowhere");
                return Ok(());
            };
            let Some(trel) = to.strip_prefix(cat!(self.root, "/").as_slice()) else {
                self.skipped(&r, b"a link out of the folder (not followed)");
                return Ok(());
            };
            if p(&to).is_dir() {
                self.skipped(&r, b"a link to a folder (not followed)");
                return Ok(());
            }
            let top = trel.split(|&c| c == b'/').next().unwrap_or_default();
            let mut why: Vec<u8> = Vec::new();
            let seeded: [&[u8]; 6] = [
                b"hooks",
                b"plugins",
                b"themes",
                b"backgrounds",
                b"branding",
                b"extensions",
            ];
            if seeded.contains(&top) {
                why = cat!("a link into ", top, "/");
            }
            // (`IFS=/ read -ra`: the parts up to a newline)
            let line = trel.split(|&c| c == b'\n').next().unwrap_or_default();
            for seg in line.split(|&c| c == b'/') {
                if !why.is_empty() {
                    break;
                }
                if let Some(w) = desk_name_refused(seg) {
                    why = cat!("a link to ", trel, ": ", w);
                }
            }
            if !why.is_empty() {
                self.skipped(&r, &why);
                return Ok(());
            }
            t = to.clone();
        }
        if p(&t).is_dir() {
            if depth >= 8 {
                self.skipped(&cat!(r, "/"), b"more than 8 folders deep");
            } else {
                self.walk(&r, depth + 1)?;
            }
            return Ok(());
        }
        let Some(m) = std::fs::metadata(p(&t)).ok().filter(|m| m.is_file()) else {
            self.skipped(&r, b"not a regular file");
            return Ok(());
        };
        let size = m.len();
        if size > 1_048_576 {
            self.skipped(&r, b"over 1 MiB");
            return Ok(());
        }
        if self.total + size > 16_777_216 || self.count >= 1000 {
            self.skipped(&r, b"over the cap of 16 MiB and 1000 files in all");
            return Ok(());
        }
        if let Some(why) = desk_content_refused(&t) {
            self.skipped(&r, why.as_bytes());
            return Ok(());
        }
        if let Some(h) = self.h {
            let src = Src::desk_file(t);
            let copied = match Dest::new(&cat!(".config/omarchy/", r)) {
                Some(d) => {
                    home_unlink(self.ctx, h, &d)?;
                    let to = d.on(h);
                    p(&to)
                        .parent()
                        .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
                        && rm_rf(&to).is_ok()
                        && cp(
                            &[b"--preserve=mode,timestamps", b"--", src.path(), &to],
                            false,
                        ) == 0
                }
                None => false,
            };
            if !copied {
                self.skipped(&r, b"it could not be copied");
                return Ok(());
            }
        }
        self.total += size;
        self.count += 1;
        self.out.extend(cat!("copied ", r, "\n"));
        Ok(())
    }
}

/// desk_files DESK [HOME] (#206): the desk's other Omarchy config files (what its plugins read beside
/// shell.json), copied into HOME's ~/.config/omarchy at the same place; without HOME only listed. A
/// deny-list: what seed_home makes is left to it, hooks/ never goes in, nor a secret by its name (a
/// folder's for all in it) or what it holds. Regular files only, each up to 1 MiB, 16 MiB and 1000
/// files in all, 8 folders deep; a link followed only to a file inside DESK, its target's path
/// checked as its own. "copied REL" and "skipped REL<TAB>WHY" lines, as `$(...)` has them.
pub fn desk_files(ctx: &Ctx, desk: &[u8], h: Option<&[u8]>) -> Res<Vec<u8>> {
    let mut w = DeskWalk {
        ctx,
        root: desk.to_vec(),
        h,
        total: 0,
        count: 0,
        out: Vec::new(),
    };
    w.walk(b"", 0)?;
    Ok(shell::strip_nl(&w.out))
}

/// A plugin's or theme's folder in the box HOME a --seed must not land on (#199): seed_blocked's.
pub struct Place {
    /// Its path in the box HOME (.config/omarchy/plugins/ID, .config/omarchy/themes/NAME).
    pub at: Vec<u8>,
    /// "plugin" or "theme".
    pub kind: &'static str,
    /// The plugin's id or the theme's name.
    pub name: Vec<u8>,
}

/// seed_blocked SRC REL PLACES (#199): the place a --seed of SRC at REL would write over, and how:
/// "in" (REL at or below it) or "over" (REL above it: a file, which replaces the folder holding it,
/// or a folder holding that path). Nothing of where SRC leads is said (a jailed caller's: finding
/// 257). JAIL_PATH (jail_path) follows a jailed caller's folders only inside its jail's.
pub fn seed_blocked<'a>(
    src: &[u8],
    rel: &[u8],
    places: &'a [Place],
    jail_path: &dyn Fn(&[u8]) -> Option<Vec<u8>>,
) -> Option<(&'a Place, &'static str)> {
    for x in places {
        if rel == x.at.as_slice() || rel.starts_with(&cat!(x.at, "/")) {
            return Some((x, "in"));
        }
        let Some(below) = x.at.strip_prefix(cat!(rel, "/").as_slice()) else {
            continue;
        };
        let mut q = cat!(src, "/", below);
        if in_jail() {
            // (`$(jail_path "${q%/*}")/${q##*/}`: the last part not followed)
            let i = q.iter().rposition(|&c| c == b'/').unwrap_or(0);
            let (dir, base) = (q[..i].to_vec(), q[i + 1..].to_vec());
            q = jail_path(&dir).map_or_else(Vec::new, |d| cat!(d, "/", base));
        }
        if !p(src).is_dir() || q.is_empty() || there(&q) {
            return Some((x, "over"));
        }
    }
    None
}

/// seed_copy_in SRC DEST: --seed's SRC into the box HOME at DEST, replacing what seed_home put there;
/// links on the way cleared first (finding 168). A folder's contents go into DEST, whose links are
/// never written through (finding 228: tar replaces a link at a folder it makes and unlinks one at a
/// file). Read from inside SRC (its folder, for a file) held open and checked to be the path checked
/// when the option was read (finding 257): a part swapped for a link since is not followed.
pub fn seed_copy_in(ctx: &Ctx, h: &[u8], src: &Src, d: &Dest) -> Res<()> {
    home_unlink(ctx, h, d)?;
    let to = d.on(h);
    if let Some(parent) = p(&to).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| failed(ctx, &cat!("make the folder of ", to), &e))?;
    }
    let s = src.path();
    let ok = if p(s).is_dir() {
        std::fs::create_dir_all(p(&to)).is_ok()
            && held_dir(s).is_some_and(|fd| {
                tar_pipe(Cwd::Held(&fd), &[b"-cf", b"-", b"."], &to, "-xpf", false) == 0
            })
    } else {
        let (dir, base) = match s.iter().rposition(|&c| c == b'/') {
            Some(0) => (b"/".to_vec(), s[1..].to_vec()),
            Some(i) => (s[..i].to_vec(), s[i + 1..].to_vec()),
            None => (b".".to_vec(), s.to_vec()),
        };
        rm_rf(&to).is_ok()
            && held_dir(&dir).is_some_and(|fd| {
                let inside = cat!("/proc/self/fd/", fd.as_raw_fd().to_string(), "/", base);
                !is_link(&inside) && cp_in(&fd, &base, &to) == 0
            })
    };
    if !ok {
        return Err(ctx.die(cat!(
            "--seed: could not copy ",
            s,
            " to ~/",
            d.rel(),
            " in the box (gone, or not what it was when checked)"
        )));
    }
    Ok(())
}

/// `cp -a --reflink=auto -- NAME TO`, run from the folder FD holds.
fn cp_in(fd: &OwnedFd, name: &[u8], to: &[u8]) -> i32 {
    let mut c = crate::sig::command("cp");
    c.args(["-a", "--reflink=auto", "--"])
        .arg(p(name))
        .arg(p(to))
        .stdin(Stdio::null());
    from_held(&mut c, fd);
    crate::sig::run_status(c).unwrap_or(127)
}

/// The plugins' ids as `printf '%s\n' ... | cut -f1 | jq -R . | jq -s .` has them: none is one empty
/// id, `[""]`.
fn ids_json(plan: &SeedPlan) -> Vec<u8> {
    if plan.plugins.is_empty() {
        return b"[\"\"]".to_vec();
    }
    let ids: Vec<String> = plan
        .plugins
        .iter()
        .map(|(id, _)| crate::json::quote(&String::from_utf8_lossy(id)))
        .collect();
    format!("[{}]", ids.join(",")).into_bytes()
}

const EMPTY_SHELL: &[u8] = b"{\"version\":1,\"idle\":{\"screensaver\":1000000,\"lock\":1000000}}\n";

/// `base64 -d`.
fn base64_decode(s: &str) -> Vec<u8> {
    let val = |c: u8| -> Option<u32> {
        Some(u32::from(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    };
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let Some(v) = val(c) else { continue };
        acc = (acc << 6 | v) & 0xffff;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).unwrap_or(0));
        }
    }
    out
}

/// seed_home: the box's fake HOME, H (`$D/home`): the user's Omarchy look, no secrets. /etc/skel
/// first (a new user's HOME but nvim's plugins and the XDG autostart entries; not over a save's),
/// then the bar (the user's shell.json with only the built-in widgets and the mounted plugins, each
/// placed where its manifest says; --stock-bar: Omarchy's; --desk-shell: the desk's as it is), the
/// theme, terminal configs, branding, extensions, themes, toggles, git's name and email. Idle held
/// off (no screensaver or lock without real input: finding 10). DESK_FILES: what --desk-shell
/// copied and skipped, for box.json.
pub fn seed_home(ctx: &Ctx, plan: &SeedPlan, h: &[u8]) -> Res<Vec<u8>> {
    let look = Look {
        omarchy: &plan.omarchy,
    };
    let home = shell::env_or_empty("HOME");
    let state = cat!(home, "/.local/state/omarchy/current");
    let omarchy = omarchy_or(&plan.omarchy);
    if !plan.from && p(b"/etc/skel/.config/omarchy").is_dir() {
        if let Some(skel) = look.src(Base::Skel, b".") {
            let rc = tar_pipe(
                Cwd::Path(skel.path()),
                &[
                    b"--exclude=./.local/share/nvim",
                    b"--exclude=./.config/autostart",
                    b"-cf",
                    b"-",
                    b".",
                ],
                h,
                "-xf",
                false,
            );
            if rc != 0 {
                return Err(status_end(rc));
            }
        }
    }
    // Every path written below is cleared of links first (a save's, finding 168).
    for rel in [
        ".config/omarchy/plugins",
        ".config/omabox",
        ".local/state/omarchy/indicators",
        ".local/share/dbus-1/services",
        ".config/omarchy/shell.toml",
        ".config/omarchy/shell.json",
        ".config/omarchy/shell.json.new",
        ".local/state/omarchy/current/theme",
        ".local/state/omarchy/current/theme.name",
        ".local/state/omarchy/current/background",
        ".local/state/omarchy/toggles",
        ".local/share/applications",
        ".gitconfig",
        ".gitconfig.lock",
    ] {
        home_unlink(ctx, h, &Dest::of(rel))?;
    }
    for rel in [
        ".config/omarchy/plugins",
        ".config/omabox",
        ".local/state/omarchy/current",
        ".local/state/omarchy/indicators",
    ] {
        mkdir_p(ctx, &cat!(h, "/", rel))?;
    }
    // Each --plugin the box sees at its own path: a link to it (finding 241). A save's links go
    // first: one to a plugin not asked for would load it, a mount onto a link would follow it.
    let plugins = Dest::of(".config/omarchy/plugins");
    if let Ok(rd) = std::fs::read_dir(p(&plugins.on(h))) {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_symlink()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    for (id, dir) in &plan.plugins {
        if listed(&plan.linked, id) {
            if let Some(d) = plugins.join(id) {
                home_link(ctx, h, &d, dir)?;
            }
        }
    }
    // (the bus watches only the service dirs there when it starts: finding 87)
    mkdir_p(ctx, &cat!(h, "/.local/share/dbus-1/services"))?;
    let oc = cat!(h, "/.config/omarchy");
    let shell_json = if plan.stock_bar {
        let sj = cat!(omarchy, "/config/omarchy/shell.json");
        if !p(&sj).is_file() {
            return Err(ctx.die(cat!("--stock-bar: no ", sj, " in this Omarchy")));
        }
        let t = cat!(oc, "/shell.toml");
        rm_f(&t).map_err(|e| failed(ctx, &cat!("remove ", t), &e))?;
        look.src(Base::Omarchy, b"config/omarchy/shell.json")
    } else {
        if let Some(toml) = look.src(Base::Desk, b"shell.toml") {
            cp_into(&toml, &oc);
        }
        look.src(Base::Desk, b"shell.json")
    };
    let ids = ids_json(plan);
    // A user who swapped the built-in notifications for a plugin has them off: back on unless a
    // mounted plugin serves them itself.
    let mut notify = true;
    for (_, dir) in &plan.plugins {
        if grep(&[b"-rqs", b"--include=*.qml", b"NotificationServer", dir]) == 0 {
            notify = false;
        }
    }
    let dirs: Vec<Vec<u8>> = plan.plugins.iter().map(|(_, d)| d.clone()).collect();
    let pw = shelljson::workspace_widgets(&dirs);
    let pagers = if pw.ok() { pw.out } else { b"[]".to_vec() };
    let split = shelljson::center_split(&plan.omarchy);
    let sjf = cat!(oc, "/shell.json");
    if plan.desk_shell {
        // (--desk-shell, #206: the desk's as it is, every plugin of it where the desk has it)
        let r = shelljson::desk_shell_json(&ids, split, &cat!(plan.desk, "/shell.json"));
        write(ctx, &sjf, &r.out)?;
        if !r.ok() {
            say(cat!(
                "--desk-shell: no shell.json in ",
                plan.desk,
                " (or not JSON): the box's shell starts from an empty one"
            ));
            write(ctx, &sjf, EMPTY_SHELL)?;
        }
    } else {
        // (one not there: bash said its redirection's own error, the Rust half nothing)
        match shell_json.and_then(|s| std::fs::read(p(s.path())).ok()) {
            Some(i) => {
                let r = shelljson::shell_json_filter(&ids, notify, &pagers, split, &i);
                write(ctx, &sjf, &r.out)?;
                if !r.ok() {
                    write(ctx, &sjf, EMPTY_SHELL)?;
                }
            }
            None => write(ctx, &sjf, EMPTY_SHELL)?,
        }
        if !plan.stock_bar {
            bar_seed_note(&desk_dir(), &plan.omarchy, &ids, notify, &pagers, split);
        }
    }
    let new = cat!(oc, "/shell.json.new");
    for (id, dir) in &plan.plugins {
        if listed(&plan.desk_ids, id) {
            continue;
        }
        let m = cat!(dir, "/manifest.json");
        let k = crate::jq::Args::new(&["-c", ".kinds // []"])
            .arg(&m)
            .run_file(crate::jq::Err::Inherit);
        if !k.ok() {
            return Err(status_end(k.rc));
        }
        let sec = crate::jq::Args::new(&["-r", ".barWidget.defaultSection // \"\""])
            .arg(&m)
            .run_file(crate::jq::Err::Inherit)
            .text();
        let section = shelljson::widget_section(&sec, split);
        let cur = std::fs::read(p(&sjf)).map_err(|e| failed(ctx, &cat!("read ", sjf), &e))?;
        let r = shelljson::place_plugin(id, &section, &k.text(), &ids, &cur);
        write(ctx, &new, &r.out)?;
        if !r.ok() {
            return Err(status_end(r.rc));
        }
        mv(ctx, &new, &sjf)?;
    }
    // --clean (#252, finding 357): the clock frozen, the sentinel wallpaper's file, no toasts.
    if plan.clean {
        if !plan.clean_has("clock") {
            let r = shelljson::clean_clock(&sjf);
            write(ctx, &new, &r.out)?;
            if !r.ok() {
                return Err(
                    ctx.die("up --clean: could not freeze the clock in the box's shell.json")
                );
            }
            mv(ctx, &new, &sjf)?;
        }
        if !plan.clean_has("wallpaper") {
            // (a save's link there cleared first, not written through: finding 168; bash wrote
            // through it)
            let d = Dest::of(".config/omabox/clean-wallpaper.png");
            home_unlink(ctx, h, &d)?;
            write(
                ctx,
                &d.on(h),
                &base64_decode(crate::upspec::CLEAN_WALLPAPER_PNG),
            )?;
        }
        // Omarchy 4.5 keeps its notifications (#293, finding 419): a clean box starts with none.
        let d = Dest::of(".local/state/omarchy/notifications");
        home_unlink(ctx, h, &d)?;
        rm_rf(&d.on(h)).map_err(|e| failed(ctx, &cat!("remove ", d.on(h)), &e))?;
    }
    // The theme: the user's current one, or with --theme NAME none of it (finding 245: the box's
    // session runs omarchy-theme-set NAME), the user's own wallpapers for it in either case.
    let cur = cat!(h, "/.local/state/omarchy/current");
    if !plan.theme.is_empty() {
        for f in ["theme", "theme.name", "background"] {
            let x = cat!(cur, "/", f);
            rm_rf(&x).map_err(|e| failed(ctx, &cat!("remove ", x), &e))?;
        }
        seed_backgrounds(ctx, &look, &plan.theme, h, b"")?;
    } else {
        seed_theme(ctx, &look, h)?;
        if let Some(n) = look.src(Base::Home, b".local/state/omarchy/current/theme.name") {
            cp_into(&n, &cur);
        }
        let id = std::fs::read(p(&cat!(state, "/theme.name")))
            .map(|t| t.split(|&c| c == b'\n').next().unwrap_or_default().to_vec())
            .unwrap_or_default();
        let bg = std::fs::read_link(p(&cat!(state, "/background")))
            .map(|b| shell::bytes(&b).to_vec())
            .unwrap_or_default();
        box_background(ctx, &look, h, &id, &bg)?;
    }
    // The rest of the look (finding 69): terminal configs and choice, branding, extensions, themes,
    // toggles.
    for t in ["foot", "alacritty", "ghostty", "kitty"] {
        let d = Dest::of(&format!(".config/{t}"));
        if let Some(s) = look.src(Base::Home, d.rel()) {
            seed_copy(ctx, &s, h, &d, &[])?;
        }
    }
    let cfg = cat!(home, "/.config");
    for n in shell::glob(&p(&cfg), false) {
        if !n.ends_with(b"xdg-terminals.list") {
            continue;
        }
        let rel = cat!(".config/", n);
        let (Some(s), Some(d)) = (look.src(Base::Home, &rel), Dest::new(&rel)) else {
            continue;
        };
        if !p(s.path()).exists() {
            continue;
        }
        home_unlink(ctx, h, &d)?;
        cp_into(&s, &cat!(h, "/.config"));
    }
    let d = Dest::of(".config/omarchy/branding");
    if let Some(s) = look.src(Base::Home, d.rel()) {
        seed_copy(ctx, &s, h, &d, &[])?;
    }
    seed_extensions(ctx, &look, plan, h)?;
    // Each theme on its own: one linked in from where it is developed is copied now (said, #170);
    // one --theme-dir names is a link to its folder at its own path in the box: live.
    let top = crate::repo::repo_top().unwrap_or_default();
    let live: Vec<Vec<u8>> = plan.theme_dirs.iter().map(|t| shell::basename(t)).collect();
    let themes = cat!(home, "/.config/omarchy/themes");
    let tdest = Dest::of(".config/omarchy/themes");
    for id in shell::glob(&p(&themes), true) {
        if live.contains(&id) {
            continue;
        }
        let (Some(s), Some(d)) = (
            look.src(Base::Home, &cat!(tdest.rel(), "/", id)),
            tdest.join(&id),
        ) else {
            continue;
        };
        seed_copy(ctx, &s, h, &d, &[])?;
        // One installed from git is someone else's code to omarchy-theme-set (a .git dir, the theme
        // no link): an empty one gives the box's Omarchy the desk's answer (#184).
        let dir = cat!(themes, "/", id);
        if !is_link(&dir) && p(&cat!(dir, "/.git")).is_dir() {
            mkdir_p(ctx, &cat!(d.on(h), "/.git"))?;
        }
        theme_snapshot_note(&cat!(dir, "/"), &top);
    }
    for td in &plan.theme_dirs {
        if let Some(d) = tdest.join(&shell::basename(td)) {
            home_link(ctx, h, &d, td)?;
        }
    }
    mkdir_p(ctx, &cat!(h, "/.local/state/omarchy/toggles"))?;
    let d = Dest::of(".local/state/omarchy/toggles/hypr");
    if let Some(s) = look.src(Base::Home, d.rel()) {
        seed_copy(ctx, &s, h, &d, &[])?;
    }
    // Without /etc/skel (an older Omarchy), at least the desktop entries: foot's X-TerminalArgAppId
    // makes xdg-terminal-exec pass --app-id, which Omarchy's window rules match.
    let apps = cat!(h, "/.local/share/applications");
    let oapps = cat!(omarchy, "/applications");
    if !p(&apps).is_dir() && p(&oapps).is_dir() {
        mkdir_p(ctx, &apps)?;
        for n in shell::glob(&p(&oapps), false) {
            if !n.ends_with(b".desktop") {
                continue;
            }
            if let Some(s) = look.src(Base::Omarchy, &cat!("applications/", n)) {
                cp_into(&s, &apps);
            }
        }
    }
    // Git's identity only (finding 48): never the rest of the user's git config (credential helpers,
    // tokens in URLs). From /, so a broken repo here cannot fail it.
    for key in ["user.name", "user.email"] {
        let mut c = crate::sig::command("git");
        c.args(["-C", "/", "config", "--global", key])
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let Ok((0, v)) = crate::sig::run_output(c) else {
            continue;
        };
        let mut c = crate::sig::command("git");
        c.args(["-C", "/", "config", "--file"])
            .arg(p(&cat!(h, "/.gitconfig")))
            .arg(key)
            .arg(p(&shell::strip_nl(&v)))
            .stdin(Stdio::null());
        let rc = crate::sig::run_status(c).unwrap_or(127);
        if rc != 0 {
            return Err(status_end(rc));
        }
    }
    // --desk-shell (#206): the desk's other config files, last, and said.
    if !plan.desk_shell {
        return Ok(Vec::new());
    }
    let out = desk_files(ctx, &plan.desk, Some(h))?;
    let mut copied: Vec<&[u8]> = Vec::new();
    for line in out.split(|&c| c == b'\n') {
        if let Some(r) = line.strip_prefix(b"copied ") {
            copied.push(r);
        } else if let Some(s) = line.strip_prefix(b"skipped ") {
            let i = s.iter().position(|&c| c == b'\t').unwrap_or(s.len());
            let why = s.get(i + 1..).unwrap_or(s);
            say(cat!(
                "--desk-shell: not copied: ",
                plan.desk_shown,
                "/",
                &s[..i],
                " (",
                why,
                ")"
            ));
        }
    }
    let list = if copied.is_empty() {
        Vec::new()
    } else {
        cat!(": ", copied.join(&b' '))
    };
    say(cat!(
        "--desk-shell: ",
        plan.desk_ids.len().to_string(),
        " plugins of ",
        plan.desk_shown,
        "/plugins, its shell.json (idle held off), and ",
        copied.len().to_string(),
        " files copied into the box's ~/.config/omarchy",
        list
    ));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_allowed_source_says_what_it_is() {
        assert!(
            SEED_ALLOW
                .iter()
                .all(|a| !a.what.is_empty() && !a.rel.is_empty())
        );
    }

    fn tmp(tag: &str) -> Vec<u8> {
        let d = std::env::temp_dir().join(format!("omabox-seed-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::create_dir_all(&d);
        shell::bytes(&d).to_vec()
    }

    fn put(f: &[u8], text: &str) {
        if let Some(par) = p(f).parent() {
            std::fs::create_dir_all(par).unwrap();
        }
        std::fs::write(p(f), text).unwrap();
    }

    #[test]
    fn secrets_are_never_a_source() {
        let look = Look { omarchy: b"" };
        for rel in [
            ".ssh",
            ".ssh/id_ed25519",
            ".config/omarchy/api-keys.env",
            ".config/omarchy/hooks",
            ".config/omarchy/hooks/post-update",
            ".config/omarchy",
            ".config/omarchy/themes/../hooks",
            ".config/omarchy/themes/.hidden",
            ".config/omarchy/themes",
            ".config/omarchy/backgrounds/x/../../hooks",
            ".gitconfig",
            ".git-credentials",
            ".local/share/keyrings",
            ".config/gh/hosts.yml",
            ".config/foot/../gh",
            ".",
            "",
        ] {
            for b in [Base::Home, Base::Desk, Base::DeskHome] {
                assert!(look.src(b, rel.as_bytes()).is_none(), "{b:?} {rel}");
            }
        }
        assert!(look.src(Base::Home, b".config/foot").is_some());
        assert!(
            look.src(Base::Home, b".config/omarchy/themes/nord")
                .is_some()
        );
        assert!(
            look.src(Base::Home, b".config/x-xdg-terminals.list")
                .is_some()
        );
        assert!(
            look.src(Base::Home, b".config/.xdg-terminals.list")
                .is_none()
        );
        assert!(look.src(Base::Desk, b"extensions").is_some());
        assert!(look.src(Base::Desk, b"api-keys.env").is_none());
        assert!(look.src(Base::Desk, b"hooks").is_none());
        assert!(look.src(Base::Skel, b".").is_some());
    }

    #[test]
    fn dests() {
        assert!(Dest::new(b"a/b").is_some());
        assert!(Dest::new(b"a/../b").is_none());
        assert!(Dest::new(b"..").is_none());
        assert!(Dest::new(b"/etc").is_none());
        assert!(Dest::new(b"").is_none());
        assert!(Dest::new(b"./.").is_none());
    }

    #[test]
    fn names_refused() {
        for n in [
            "api-keys.env",
            "x.env",
            ".env.local",
            "MyKey.json",
            "github_TOKEN",
            "a.pem",
            "id_rsa.pub",
            ".netrc",
            "monkey",
            "\u{212a}ey",
        ] {
            assert_eq!(
                desk_name_refused(n.as_bytes()),
                Some("a secret's name"),
                "{n}"
            );
        }
        assert_eq!(
            desk_name_refused(b".git"),
            Some("a git dir (its config can hold credentials)")
        );
        for n in [
            "shell.json.bak",
            "x.bak-1",
            "x.bak.2",
            "a~",
            "b.swp",
            "c.orig",
            "d.old",
        ] {
            assert_eq!(desk_name_refused(n.as_bytes()), Some("a backup"), "{n}");
        }
        for n in ["profiles", "layout.json", "x.bakery"] {
            assert_eq!(desk_name_refused(n.as_bytes()), None, "{n}");
        }
    }

    #[test]
    fn content_refused() {
        let d = tmp("content");
        let f = cat!(d, "/f");
        for (text, bad) in [
            ("{\"apiToken\": \"abcdef123\"}", true),
            ("password = hunter22", true),
            ("x ghp_aaaaaaaaaaaaaaaaaaaa", true),
            ("AKIAABCDEFGHIJKLMNOP", true),
            ("{\"maxTokens\": 4}", false),
            ("{\"tokenizer\": \"x\"}", false),
            ("plain", false),
        ] {
            put(&f, text);
            assert_eq!(desk_content_refused(&f).is_some(), bad, "{text}");
        }
        assert_eq!(
            desk_content_refused(&cat!(d, "/none")),
            Some("it could not be read")
        );
        let _ = std::fs::remove_dir_all(p(&d));
    }

    #[test]
    fn unlink_clears_links_on_the_way() {
        let d = tmp("unlink");
        let h = cat!(d, "/h");
        let out = cat!(d, "/out");
        std::fs::create_dir_all(p(&cat!(h, "/a"))).unwrap();
        std::fs::create_dir_all(p(&out)).unwrap();
        std::os::unix::fs::symlink(p(&out), p(&cat!(h, "/a/b"))).unwrap();
        let ctx = Ctx::new(b"up", b"/x".to_vec());
        assert!(home_unlink(&ctx, &h, &Dest::of("a/b/c")).is_ok());
        assert!(!there(&cat!(h, "/a/b")));
        assert!(p(&out).is_dir());
        assert!(home_link(&ctx, &h, &Dest::of("a/b/c"), b"/t").is_ok());
        assert_eq!(
            std::fs::read_link(p(&cat!(h, "/a/b/c"))).unwrap(),
            PathBuf::from("/t")
        );
        let _ = std::fs::remove_dir_all(p(&d));
    }

    #[test]
    fn blocked() {
        let places = vec![
            Place {
                at: b".config/omarchy/plugins/x".to_vec(),
                kind: "plugin",
                name: b"x".to_vec(),
            },
            Place {
                at: b".config/omarchy/themes/t".to_vec(),
                kind: "theme",
                name: b"t".to_vec(),
            },
        ];
        let none = |_: &[u8]| None;
        let how = |r: Option<(&Place, &'static str)>| r.map(|(x, h)| (x.kind, h));
        let r = seed_blocked(b"/s", b".config/omarchy/plugins/x/f", &places, &none);
        assert_eq!(how(r), Some(("plugin", "in")));
        // A file above one: over; a folder without that path: not; one with it: over.
        let d = tmp("blocked");
        put(&cat!(d, "/f"), "");
        let r = seed_blocked(&cat!(d, "/f"), b".config/omarchy", &places, &none);
        assert_eq!(how(r), Some(("plugin", "over")));
        assert!(seed_blocked(&d, b".config/omarchy", &places, &none).is_none());
        std::fs::create_dir_all(p(&cat!(d, "/themes/t"))).unwrap();
        let r = seed_blocked(&d, b".config/omarchy", &places, &none);
        assert_eq!(how(r), Some(("theme", "over")));
        assert!(seed_blocked(&d, b".config/other", &places, &none).is_none());
        let _ = std::fs::remove_dir_all(p(&d));
    }

    #[test]
    fn wallpapers_and_base64() {
        assert!(wallpaper_name(b"a.PNG"));
        assert!(!wallpaper_name(b".a.png"));
        assert!(!wallpaper_name(b".png"));
        assert!(!wallpaper_name(b"a.txt"));
        let png = base64_decode(crate::upspec::CLEAN_WALLPAPER_PNG);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(base64_decode("aGk="), b"hi");
    }

    #[test]
    fn desk_walk_lists() {
        let d = tmp("desk");
        let ctx = Ctx::new(b"up", b"/x".to_vec());
        for f in [
            "prof/a.json",
            "hooks/x",
            "shell.json",
            "api-keys.env",
            "ok.json",
            ".hid",
        ] {
            put(&cat!(d, "/", f), "{}");
        }
        put(&cat!(d, "/notes.txt"), "token: abcdefgh");
        std::os::unix::fs::symlink("/etc/passwd", p(&cat!(d, "/out"))).unwrap();
        let h = cat!(d, "-home");
        let out = desk_files(&ctx, &d, Some(&h)).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out),
            "skipped api-keys.env\ta secret's name\n\
             skipped hooks/\tyour scripts (Omarchy runs them on its events)\n\
             skipped notes.txt\tit holds what looks like a key, token or password\n\
             copied ok.json\n\
             skipped out\ta link out of the folder (not followed)\n\
             copied prof/a.json\n\
             copied .hid"
        );
        assert!(p(&cat!(h, "/.config/omarchy/prof/a.json")).is_file());
        let _ = std::fs::remove_dir_all(p(&d));
        let _ = std::fs::remove_dir_all(p(&h));
    }

    #[test]
    fn copies_keep_links_and_leave_hidden_out() {
        let d = tmp("copy");
        let src = cat!(d, "/src");
        put(&cat!(src, "/a/b.conf"), "x");
        put(&cat!(src, "/.hidden"), "x");
        put(&cat!(src, "/a/api-keys.env"), "x");
        std::os::unix::fs::symlink("/etc/passwd", p(&cat!(src, "/l"))).unwrap();
        let h = cat!(d, "/h");
        std::fs::create_dir_all(p(&h)).unwrap();
        let ctx = Ctx::new(b"up", b"/x".to_vec());
        let s = Src::asked(src.clone());
        assert!(seed_copy(&ctx, &s, &h, &Dest::of("c"), &[b"a/b.conf".to_vec()]).is_ok());
        assert!(!there(&cat!(h, "/c/a/b.conf")));
        assert!(seed_copy(&ctx, &s, &h, &Dest::of("c"), &[]).is_ok());
        assert!(p(&cat!(h, "/c/a/b.conf")).is_file());
        assert!(!there(&cat!(h, "/c/.hidden")));
        assert!(!there(&cat!(h, "/c/a/api-keys.env")));
        assert!(is_link(&cat!(h, "/c/l")));
        assert!(seed_copy_in(&ctx, &h, &s, &Dest::of("in")).is_ok());
        assert!(there(&cat!(h, "/in/.hidden")));
        let f = Src::asked(cat!(src, "/a/b.conf"));
        assert!(seed_copy_in(&ctx, &h, &f, &Dest::of("one/f")).is_ok());
        assert!(p(&cat!(h, "/one/f")).is_file());
        let _ = std::fs::remove_dir_all(p(&d));
    }
}
