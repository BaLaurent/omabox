//! What bash and coreutils do to the words bin/omabox handles, done the same way here: `$(...)`'s
//! trailing newlines, cut, basename, printf %q, globs, word splitting, and `=~`; text read as UTF-8,
//! lists in byte order and omabox's own patterns in ASCII, whatever the locale (findings 445-447).

use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// `$(...)`: the output without its trailing newlines.
pub fn strip_nl(v: &[u8]) -> Vec<u8> {
    let mut n = v.len();
    while n > 0 && v[n - 1] == b'\n' {
        n -= 1;
    }
    v[..n].to_vec()
}

/// `date +FMT`: the local time by strftime, with no `date` run (names and stamps).
pub fn date(fmt: &str) -> Vec<u8> {
    let Ok(f) = CString::new(fmt) else {
        return Vec::new();
    };
    let mut buf = [0u8; 128];
    // SAFETY: time and localtime_r (which reads TZ the first time) fill plain data of ours; strftime writes at most the
    // buffer's length and returns how much it wrote.
    let n = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        libc::strftime(buf.as_mut_ptr().cast(), buf.len(), f.as_ptr(), &tm)
    };
    buf[..n].to_vec()
}

/// `date -Is`: 2026-10-23T15:04:05+01:00.
pub fn date_iso() -> Vec<u8> {
    let mut d = date("%Y-%m-%dT%H:%M:%S%z");
    if d.len() >= 5 {
        d.insert(d.len() - 2, b':');
    }
    d
}

/// `date +%s%N`: nanoseconds since the epoch.
pub fn epoch_ns() -> Vec<u8> {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}{:09}", t.as_secs(), t.subsec_nanos()).into_bytes()
}

// Text is read as UTF-8 whatever the locale (finding 447): a character is a valid UTF-8 sequence
// (no overlong form, no surrogate, nothing past U+10FFFF), and each byte of anything else is one.
// Bash and coreutils read text in the caller's locale: bytes under C, and glibc's own rules under a
// UTF-8 one.

/// `$(printf '%s' "$V" | cut -c1-N)`: each line's first N characters.
pub fn cut_c(v: &[u8], n: usize) -> Vec<u8> {
    let out: Vec<&[u8]> = v.split(|&c| c == b'\n').map(|l| chars(l, n)).collect();
    strip_nl(&out.join(&b'\n'))
}

/// `${V:0:N}`: the first N characters.
pub fn head_chars(v: &[u8], n: usize) -> &[u8] {
    chars(v, n)
}

/// `cut -c1-N` of one line (no newline in it).
pub fn cut_line(l: &[u8], n: usize) -> &[u8] {
    chars(l, n)
}

/// The first N characters of V.
fn chars(v: &[u8], n: usize) -> &[u8] {
    let (mut i, mut k) = (0, 0);
    while i < v.len() && k < n {
        i += char_len(&v[i..]);
        k += 1;
    }
    &v[..i]
}

/// The length of the character V starts with: its UTF-8 sequence's, or 1 for a byte that starts
/// none (and for an empty V).
fn char_len(v: &[u8]) -> usize {
    match v.first() {
        Some(0x80..) => match utf8_len(v) {
            0 => 1,
            n => n,
        },
        _ => 1,
    }
}

/// V's characters, each as its bytes and the char it is (None for a byte that starts none).
fn u8_chars(v: &[u8]) -> impl Iterator<Item = (&[u8], Option<char>)> {
    let mut i = 0;
    std::iter::from_fn(move || {
        if i >= v.len() {
            return None;
        }
        let n = char_len(&v[i..]);
        let b = &v[i..i + n];
        i += n;
        let c = std::str::from_utf8(b).ok().and_then(|s| s.chars().next());
        Some((b, c))
    })
}

/// A line `grep .` matches: one character at least (a byte that starts none is one).
pub fn grep_dot(l: &[u8]) -> bool {
    !l.is_empty()
}

/// `printf '%s\n' "$V" | grep .`'s lines, as `$(...)` keeps them: V's lines that are not empty.
pub fn grep_lines(v: &[u8]) -> Vec<u8> {
    let kept: Vec<&[u8]> = v.split(|&c| c == b'\n').filter(|l| grep_dot(l)).collect();
    kept.join(&b'\n')
}

/// `${#V}`: V's length in characters.
pub fn char_count(v: &[u8]) -> usize {
    u8_chars(v).count()
}

/// `${V//[[:cntrl:]]/ }`: each control character a space: C0, DEL and C1 (Unicode's Cc), and the
/// line and paragraph separators U+2028 and U+2029, as glibc's UTF-8 locales class them.
pub fn cntrl_to_space(v: &[u8]) -> Vec<u8> {
    chars_map(v, |b| {
        let ctl = match std::str::from_utf8(b).ok().and_then(|s| s.chars().next()) {
            Some(c) => c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'),
            None => false,
        };
        if ctl { b" ".to_vec() } else { b.to_vec() }
    })
}

/// `${V//PATTERN/...}` with a one-character PATTERN: each character of V through F, its bytes given.
pub fn chars_map(v: &[u8], f: impl Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len());
    for (b, _) in u8_chars(v) {
        out.extend(f(b));
    }
    out
}

/// `${V,,}` of an option's word: its ASCII letters in lower case, the rest as it is (bash's followed
/// the locale: a Turkish one made `SHIFT` `shıft`).
pub fn lower(v: &[u8]) -> Vec<u8> {
    v.to_ascii_lowercase()
}

/// `$(basename "$V")`.
pub fn basename(v: &[u8]) -> Vec<u8> {
    let mut end = v.len();
    while end > 1 && v[end - 1] == b'/' {
        end -= 1;
    }
    let s = &v[..end];
    if s == b"/" {
        return b"/".to_vec();
    }
    let start = s.iter().rposition(|&c| c == b'/').map_or(0, |i| i + 1);
    strip_nl(&s[start..])
}

/// printf %q, as bash 5 writes it: backslashes, or $'...' for a word with a control character.
pub fn quote_q(v: &[u8]) -> Vec<u8> {
    if v.is_empty() {
        return b"''".to_vec();
    }
    let text = String::from_utf8_lossy(v);
    let ansi = std::str::from_utf8(v).is_err() || text.chars().any(|c| c.is_control());
    let mut out = Vec::with_capacity(v.len() + 8);
    if ansi {
        out.extend_from_slice(b"$'");
        let mut i = 0;
        while i < v.len() {
            let c = v[i];
            let esc: Option<&[u8]> = match c {
                0x07 => Some(b"\\a"),
                0x08 => Some(b"\\b"),
                0x0c => Some(b"\\f"),
                b'\n' => Some(b"\\n"),
                b'\r' => Some(b"\\r"),
                b'\t' => Some(b"\\t"),
                0x0b => Some(b"\\v"),
                0x1b => Some(b"\\E"),
                b'\\' => Some(b"\\\\"),
                b'\'' => Some(b"\\'"),
                _ => None,
            };
            if let Some(e) = esc {
                out.extend_from_slice(e);
                i += 1;
                continue;
            }
            if c >= 0x80 {
                // A whole UTF-8 character passes; a byte that starts none is written in octal.
                let len = utf8_len(&v[i..]);
                if len > 0 {
                    out.extend_from_slice(&v[i..i + len]);
                    i += len;
                    continue;
                }
            }
            if !(0x20..0x7f).contains(&c) {
                out.extend_from_slice(format!("\\{c:03o}").as_bytes());
            } else {
                out.push(c);
            }
            i += 1;
        }
        out.push(b'\'');
        return out;
    }
    for (i, &c) in v.iter().enumerate() {
        let quote = match c {
            b' ' | b'\'' | b'"' | b'\\' | b'|' | b'&' | b';' | b'(' | b')' | b'<' | b'>' | b'!'
            | b'{' | b'}' | b'*' | b'[' | b'?' | b']' | b'^' | b'$' | b'`' | b',' => true,
            b'#' => i == 0,
            b'~' => i == 0 || matches!(v[i - 1], b'=' | b':'),
            _ => false,
        };
        if quote {
            out.push(b'\\');
        }
        out.push(c);
    }
    out
}

fn utf8_len(v: &[u8]) -> usize {
    let n = match v[0] {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return 0,
    };
    if v.len() >= n && std::str::from_utf8(&v[..n]).is_ok() {
        n
    } else {
        0
    }
}

/// `${V@Q}`: the word quoted for bash to read back, '...' (or $'...' for a control character).
pub fn quote_at(v: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(v);
    if std::str::from_utf8(v).is_err() || text.chars().any(char::is_control) {
        return quote_q(v);
    }
    let mut out = b"'".to_vec();
    for &c in v {
        if c == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(c);
        }
    }
    out.push(b'\'');
    out
}

/// `$(...)` of output that may hold NUL bytes: bash drops them (warning about it; that line is not
/// reproduced), then the trailing newlines.
pub fn subst(v: &[u8]) -> Vec<u8> {
    let kept: Vec<u8> = v.iter().copied().filter(|&c| c != 0).collect();
    strip_nl(&kept)
}

/// `[ -z "${V//[[:space:]]/}" ]`: V holds nothing but white space (Unicode's White_Space), as UTF-8.
pub fn all_space(v: &[u8]) -> bool {
    std::str::from_utf8(v).is_ok_and(|s| s.chars().all(char::is_whitespace))
}

/// `$PWD` as bash has it: the inherited one while it names this directory, else getcwd's.
pub fn pwd() -> Vec<u8> {
    use std::os::unix::fs::MetadataExt;
    if let Some(p) = env("PWD").filter(|p| p.first() == Some(&b'/')) {
        let same =
            |a: &std::fs::Metadata, b: &std::fs::Metadata| a.dev() == b.dev() && a.ino() == b.ino();
        let dots = p.split(|&c| c == b'/').any(|s| s == b"." || s == b"..");
        if let (Ok(a), Ok(b)) = (std::fs::metadata(path(&p)), std::fs::metadata(".")) {
            if same(&a, &b) && !dots {
                return p;
            }
        }
    }
    std::env::current_dir()
        .map(|d| bytes(&d).to_vec())
        .unwrap_or_default()
}

/// Unquoted `$(...)` in a for list: split at blanks and newlines (no glob: no name a box can have
/// holds a glob character).
pub fn words(v: &[u8]) -> Vec<Vec<u8>> {
    v.split(|c| matches!(c, b' ' | b'\t' | b'\n'))
        .filter(|w| !w.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

/// `read -r V1 ... VN <<<"$V"` with the default IFS: V's first line split at blanks, the last of N
/// taking the rest (its trailing blanks off); missing ones empty.
pub fn read_fields(v: &[u8], n: usize) -> Vec<Vec<u8>> {
    read_ifs(v, n, b" \t\n")
}

/// `IFS=CHARS read -r V1 ... VN <<<"$V"`: V's first line split as read splits it. A run of IFS
/// whitespace (space, tab, newline) is one separator, so is one other IFS character with the
/// whitespace around it; whitespace at either end goes; the last of N takes the rest.
pub fn read_ifs(v: &[u8], n: usize, ifs: &[u8]) -> Vec<Vec<u8>> {
    let ws = |c: &u8| ifs.contains(c) && matches!(c, b' ' | b'\t' | b'\n');
    let sep = |c: &u8| ifs.contains(c);
    let mut line = v.split(|&c| c == b'\n').next().unwrap_or_default();
    let skip_ws = |l: &[u8]| -> usize { l.iter().position(|c| !ws(c)).unwrap_or(l.len()) };
    line = &line[skip_ws(line)..];
    let mut out = Vec::new();
    for k in 0..n {
        if k + 1 == n {
            let end = line.iter().rposition(|c| !ws(c)).map_or(0, |i| i + 1);
            out.push(line[..end].to_vec());
            break;
        }
        let e = line.iter().position(sep).unwrap_or(line.len());
        out.push(line[..e].to_vec());
        line = &line[e..];
        // The separator: whitespace, at most one other IFS character, whitespace.
        line = &line[skip_ws(line)..];
        if line.first().is_some_and(|c| sep(c) && !ws(c)) {
            line = &line[1..];
            line = &line[skip_ws(line)..];
        }
    }
    out
}

/// The categories a caller's pattern reads (bash sets these and LC_MESSAGES, LC_NUMERIC, LC_TIME one
/// by one: locale.c's reset_locale_vars).
const CATEGORIES: [(libc::c_int, &str); 2] = [
    (libc::LC_CTYPE, "LC_CTYPE"),
    (libc::LC_COLLATE, "LC_COLLATE"),
];

/// The locale as bash sets it at its start: setlocale(LC_ALL, "") first; where that fails as a whole
/// (one category's name no locale), LC_ALL's value for every category when it is set; else LANG's
/// for every one, then each category's own LC_* over it, one by one (one that fails keeps LANG's).
/// LANG no locale either: C. GET reads the environment, SET makes the locale; true when the first
/// call took.
pub fn bash_locale(
    get: &dyn Fn(&str) -> Option<Vec<u8>>,
    set: &mut dyn FnMut(libc::c_int, &[u8]) -> bool,
) -> bool {
    if set(libc::LC_ALL, b"") {
        return true;
    }
    let val = |k: &str| get(k).filter(|v| !v.is_empty());
    if let Some(all) = val("LC_ALL") {
        set(libc::LC_ALL, &all);
        return false;
    }
    let lang = val("LANG").unwrap_or_default();
    if !set(libc::LC_ALL, &lang) {
        return false;
    }
    for (c, name) in CATEGORIES {
        let v = val(name).unwrap_or_else(|| lang.clone());
        set(c, &v);
    }
    false
}

/// The caller's locale, for a caller's own pattern only (`events --grep`, `--until`: its syntax the
/// contract), its character classes and ranges as bash's `=~` read them (bash_locale, for LC_CTYPE
/// and LC_COLLATE). Nothing else in omabox reads a locale: the process stays in C (finding 447).
fn caller_locale() -> libc::locale_t {
    static L: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *L.get_or_init(|| {
        let mut cur: libc::locale_t = std::ptr::null_mut();
        let get = |k: &str| env(k);
        let mut set = |c: libc::c_int, v: &[u8]| -> bool {
            let mask = match c {
                libc::LC_ALL => libc::LC_CTYPE_MASK | libc::LC_COLLATE_MASK,
                libc::LC_CTYPE => libc::LC_CTYPE_MASK,
                libc::LC_COLLATE => libc::LC_COLLATE_MASK,
                _ => return true,
            };
            let Ok(v) = CString::new(v.to_vec()) else {
                return false;
            };
            // SAFETY: v is a valid C string; cur is null or a locale newlocale made, which a call
            // that fails leaves as it was and one that succeeds takes over.
            let n = unsafe { libc::newlocale(mask, v.as_ptr(), cur) };
            if n.is_null() {
                return false;
            }
            cur = n;
            true
        };
        bash_locale(&get, &mut set);
        (if cur.is_null() { c_locale() } else { cur }) as usize
    }) as libc::locale_t
}

/// `"$DIR"/*` (or `*/` with `dirs`): the names of DIR's entries but hidden ones, in byte order (bash's
/// glob sorts by the locale's collation: finding 446).
pub fn glob(dir: &Path, dirs: bool) -> Vec<Vec<u8>> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<Vec<u8>> = rd
        .flatten()
        .map(|e| e.file_name().into_vec())
        .filter(|n| n.first() != Some(&b'.'))
        .filter(|n| !dirs || dir.join(OsStr::from_bytes(n)).is_dir())
        .collect();
    names.sort();
    names
}

/// `for f in "$DIR"/*EXT; do [ -f "$f" ] || continue`: DIR's regular files (links to one too) named
/// so, hidden ones aside, in byte order, each as DIR/NAME.
pub fn glob_ext(dir: &[u8], ext: &[u8]) -> Vec<Vec<u8>> {
    let Ok(rd) = std::fs::read_dir(path(dir)) else {
        return Vec::new();
    };
    let mut names: Vec<Vec<u8>> = rd
        .flatten()
        .map(|e| e.file_name().into_vec())
        .filter(|n| n.first() != Some(&b'.') && n.ends_with(ext))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|n| crate::cat!(dir, "/", n))
        .filter(|p| path(p).is_file())
        .collect()
}

/// `f=$(mktemp)`: a new temporary file, open, and its path (left there for the caller's rm); mktemp's
/// own words on stderr and its status when it cannot.
pub fn mktemp() -> Result<(std::fs::File, Vec<u8>), crate::exit::Exit> {
    mktemp_args(&[])
}

/// `f=$(mktemp TEMPLATE)` (`mktemp -d` with ["-d"] first: then the dir, opened for reading).
pub fn mktemp_in(template: &[u8]) -> Result<(std::fs::File, Vec<u8>), crate::exit::Exit> {
    mktemp_args(&[template])
}

/// `$(mktemp ARGS...)`: what it made, opened, and its path; its own words on stderr and its status
/// when it cannot.
pub fn mktemp_args(args: &[&[u8]]) -> Result<(std::fs::File, Vec<u8>), crate::exit::Exit> {
    let mut c = crate::sig::command("mktemp");
    c.args(args.iter().map(|a| OsStr::from_bytes(a)))
        .stdin(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    let (rc, out) = crate::sig::run_subst(c).map_err(|_| crate::exit::Exit(1))?;
    if rc != 0 {
        return Err(crate::exit::Exit(u8::try_from(rc).unwrap_or(1)));
    }
    let p = strip_nl(&out);
    let dir = args.first().is_some_and(|a| *a == b"-d");
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(!dir)
        .open(path(&p))
        .map_err(|_| crate::exit::Exit(1))?;
    Ok((f, p))
}

/// A temporary file, removed when this goes, on every way out.
pub struct Rm(pub Vec<u8>);

impl Drop for Rm {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(path(&self.0));
    }
}

/// A path from bytes.
pub fn path(v: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(v.to_vec()))
}

/// The bytes of a path.
pub fn bytes(p: &Path) -> &[u8] {
    p.as_os_str().as_bytes()
}

/// An environment variable as bytes; None when unset.
pub fn env(name: &str) -> Option<Vec<u8>> {
    std::env::var_os(name).map(OsString::into_vec)
}

/// `${NAME:-}`: the variable, empty when unset.
pub fn env_or_empty(name: &str) -> Vec<u8> {
    env(name).unwrap_or_default()
}

/// A POSIX extended regular expression (glibc's, as bash's `=~`). omabox's own patterns (Re::new)
/// are compiled and matched in the C locale, so `[0-9]` is the ten ASCII digits and `[A-Za-z]` the
/// ASCII letters whatever the caller's locale (finding 445: in en_US.UTF-8 bash's took `²` and
/// `é`), and `.` takes any byte; a caller's own (Re::from_bytes: `events --grep`) in the caller's
/// (caller_locale).
pub struct Re {
    re: Box<libc::regex_t>,
    loc: libc::locale_t,
}

/// The C locale, for omabox's own patterns.
fn c_locale() -> libc::locale_t {
    static C: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    // SAFETY: "C" always exists; the locale is made once and kept for the process's life.
    *C.get_or_init(|| unsafe {
        libc::newlocale(libc::LC_ALL_MASK, c"C".as_ptr(), std::ptr::null_mut()) as usize
    }) as libc::locale_t
}

/// This thread in a locale while it lives (uselocale), its own given back on drop.
struct InLocale(Option<libc::locale_t>);

impl InLocale {
    fn new(loc: libc::locale_t) -> InLocale {
        // SAFETY: loc is a valid locale (or null: then nothing is switched).
        InLocale((!loc.is_null()).then(|| unsafe { libc::uselocale(loc) }))
    }
}

impl Drop for InLocale {
    fn drop(&mut self) {
        if let Some(old) = self.0 {
            // SAFETY: the locale this thread had before new.
            unsafe { libc::uselocale(old) };
        }
    }
}

impl Re {
    /// One of omabox's own patterns, ASCII in every locale.
    pub fn new(pattern: &str) -> Option<Re> {
        Re::compile(pattern.as_bytes(), false, c_locale())
    }

    /// A pattern from bytes (a caller's `[[ x =~ $re ]]`), in the caller's locale; ICASE as under
    /// `shopt -s nocasematch`. None where regcomp refuses it (bash's status 2).
    pub fn from_bytes(pattern: &[u8], icase: bool) -> Option<Re> {
        Re::compile(pattern, icase, caller_locale())
    }

    fn compile(pattern: &[u8], icase: bool, loc: libc::locale_t) -> Option<Re> {
        let pat = CString::new(pattern).ok()?;
        // SAFETY: regex_t is plain data that regcomp fills in; zeroed is a valid starting value.
        let mut re: Box<libc::regex_t> = Box::new(unsafe { std::mem::zeroed() });
        let flags = libc::REG_EXTENDED | if icase { libc::REG_ICASE } else { 0 };
        let _in = InLocale::new(loc);
        // SAFETY: re points to a regex_t we own; pat is a valid C string.
        let rc = unsafe { libc::regcomp(&mut *re, pat.as_ptr(), flags) };
        (rc == 0).then_some(Re { re, loc })
    }

    /// The whole match and each group's, as byte ranges of `s` (None for a group that took no
    /// part), or None for no match. `s` may hold NUL bytes (REG_STARTEND).
    pub fn captures(&self, s: &[u8], notbol: bool) -> Option<Vec<Option<(usize, usize)>>> {
        const N: usize = 10;
        let empty = libc::regmatch_t { rm_so: 0, rm_eo: 0 };
        let mut m = [empty; N];
        m[0].rm_eo = libc::regoff_t::try_from(s.len()).ok()?;
        let flags = libc::REG_STARTEND | if notbol { libc::REG_NOTBOL } else { 0 };
        // A non-null pointer for an empty subject: REG_STARTEND reads nothing past rm_eo.
        let base: &[u8] = if s.is_empty() { b"\0" } else { s };
        let _in = InLocale::new(self.loc);
        // SAFETY: the regex is compiled; base is valid for rm_eo bytes; m has N entries.
        let rc =
            unsafe { libc::regexec(&*self.re, base.as_ptr().cast(), N, m.as_mut_ptr(), flags) };
        if rc != 0 {
            return None;
        }
        Some(
            m.iter()
                .map(|r| {
                    (r.rm_so >= 0)
                        .then(|| (usize::try_from(r.rm_so).ok(), usize::try_from(r.rm_eo).ok()))
                        .and_then(|(a, b)| Some((a?, b?)))
                })
                .collect(),
        )
    }

    pub fn is_match(&self, s: &[u8]) -> bool {
        self.captures(s, false).is_some()
    }

    /// Group I of a match, as bytes.
    pub fn group(&self, s: &[u8], i: usize) -> Option<Vec<u8>> {
        let m = self.captures(s, false)?;
        let (a, b) = (*m.get(i)?)?;
        Some(s[a..b].to_vec())
    }
}

impl Drop for Re {
    fn drop(&mut self) {
        // SAFETY: compiled by regcomp in Re::compile, freed once.
        unsafe { libc::regfree(&mut *self.re) }
    }
}

/// `[[ S =~ PATTERN ]]`.
pub fn re_match(pattern: &str, s: &[u8]) -> bool {
    Re::new(pattern).is_some_and(|r| r.is_match(s))
}

/// `[[ S =~ PATTERN ]]` and BASH_REMATCH[I].
pub fn re_group(pattern: &str, s: &[u8], i: usize) -> Option<Vec<u8>> {
    Re::new(pattern)?.group(s, i)
}

/// A whole number as omabox reads one (finding 448): an optional sign, then ASCII digits, decimal
/// whatever its zeros (`010` is 10, `08` is 8); None for anything else, an empty word or one past
/// 64 bits. Bash's arithmetic read a 0 in front as octal (`08` ended the command in its words),
/// 0x as hex, an empty word as 0, and wrapped past 64 bits.
pub fn dec(v: &[u8]) -> Option<i64> {
    let (neg, d) = match v.first() {
        Some(b'-') => (true, &v[1..]),
        Some(b'+') => (false, &v[1..]),
        _ => (false, v),
    };
    if d.is_empty() || !d.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut n: i64 = 0;
    for &c in d {
        let k = i64::from(c - b'0');
        n = n.checked_mul(10).and_then(|n| {
            if neg {
                n.checked_sub(k)
            } else {
                n.checked_add(k)
            }
        })?;
    }
    Some(n)
}

/// A number word omabox takes (`^[0-9]+$`): ASCII digits, decimal, no more than 64 bits hold.
pub fn is_num(v: &[u8]) -> bool {
    v.first().is_some_and(u8::is_ascii_digit) && dec(v).is_some()
}

/// `[ A -le B ]`'s reading of a number: optional blanks around, then dec's; None for what test
/// refuses as no integer (it says so, and the test fails).
pub fn test_int(v: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(v)
        .ok()?
        .trim_matches(|c| c == ' ' || c == '\t');
    dec(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_as_coreutils_prints_them() {
        let o = std::process::Command::new("date")
            .arg("+%Y%m%d")
            .output()
            .unwrap();
        assert_eq!(date("%Y%m%d"), strip_nl(&o.stdout));
        let iso = String::from_utf8(date_iso()).unwrap();
        let o = std::process::Command::new("date")
            .arg("-Is")
            .output()
            .unwrap();
        assert_eq!(iso[..10], String::from_utf8_lossy(&o.stdout)[..10]);
        assert!(re_match(
            r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}[-+][0-9]{2}:[0-9]{2}$",
            iso.as_bytes()
        ));
        let ns = epoch_ns();
        assert!(ns.len() >= 19 && ns.iter().all(u8::is_ascii_digit));
    }

    // What coreutils 9.12's `cut -c` gave for each in a UTF-8 locale, now in every locale (finding
    // 447: under C it cut bytes).
    #[test]
    fn cut_counts_characters_in_any_locale() {
        for loc in [c"C.UTF-8", c"C"] {
            let _l = ThreadLocale::new(loc);
            assert_eq!(cut_c(b"abcdef", 3), b"abc");
            assert_eq!(cut_c(b"abcdef\nxyzw", 2), b"ab\nxy");
            assert_eq!(cut_c("é".as_bytes(), 1), "é".as_bytes());
            assert_eq!(cut_c("aéb\nbéc".as_bytes(), 2), "aé\nbé".as_bytes());
            // A byte that starts no character is one; a sequence cut short, a byte each.
            assert_eq!(cut_c(b"a\xffb", 2), b"a\xff");
            assert_eq!(cut_c(b"x\xe2\x82y", 2), b"x\xe2");
            assert_eq!(cut_c(b"x\xe2\x82y", 3), b"x\xe2\x82");
            // Overlong, a surrogate, past U+10FFFF: no character; a 4-byte one is one.
            assert_eq!(cut_c(b"\xc0\x80z", 1), b"\xc0");
            assert_eq!(cut_c(b"\xed\xa0\x80z", 1), b"\xed");
            assert_eq!(cut_c(b"\xf4\x90\x80\x80z", 1), b"\xf4");
            assert_eq!(cut_c(b"\xf8\x88\x80\x80\x80z", 1), b"\xf8");
            assert_eq!(head_chars(b"\xf4\x90\x80\x80z", 1), b"\xf4");
            assert_eq!(head_chars(b"x\xe2\x82y", 2), b"x\xe2");
            assert_eq!(cut_c("\u{1f600}z".as_bytes(), 1), "\u{1f600}".as_bytes());
            // A combining mark is a character of its own; a NUL is one.
            assert_eq!(cut_c("e\u{301}z".as_bytes(), 2), "e\u{301}".as_bytes());
            assert_eq!(cut_c(b"ab\0cd", 3), b"ab\0");
            assert_eq!(head_chars("é".as_bytes(), 1), "é".as_bytes());
        }
    }

    /// A locale for this test's thread alone (uselocale), the thread's own given back on drop: the
    /// tests run in threads of one process, whose setlocale they share.
    struct ThreadLocale(libc::locale_t, libc::locale_t);

    impl ThreadLocale {
        fn new(name: &std::ffi::CStr) -> ThreadLocale {
            // SAFETY: a valid C string; the new locale is this thread's until drop frees it.
            unsafe {
                let l = libc::newlocale(libc::LC_ALL_MASK, name.as_ptr(), std::ptr::null_mut());
                assert!(!l.is_null(), "no locale {name:?}");
                ThreadLocale(l, libc::uselocale(l))
            }
        }
    }

    impl Drop for ThreadLocale {
        fn drop(&mut self) {
            // SAFETY: the thread's own locale back, then the one made in new freed once.
            unsafe {
                libc::uselocale(self.1);
                libc::freelocale(self.0);
            }
        }
    }

    // What bash 5.3 gave for each in C.UTF-8, now in every locale (finding 447); case only for ASCII
    // letters (a Turkish locale's towlower made `I` `ı`).
    #[test]
    fn length_controls_and_case_in_any_locale() {
        let t = b"a\x01\xff\xc2\x85b\x7f\xe2\x80\xa8";
        let x = b"\xc3\x89\xc4\xb0A\xff\xe1\xba\x9eSHIFT";
        for loc in [c"C.UTF-8", c"C"] {
            let _l = ThreadLocale::new(loc);
            assert_eq!(char_count(t), 7);
            // (bash, a byte that starts none in it, matched byte by byte and kept U+0085)
            assert_eq!(cntrl_to_space(t), b"a \xff b  ");
            assert_eq!(cntrl_to_space(b"\xc2\x85\x01\xe2\x80\xa8"), b"   ");
            assert_eq!(
                cntrl_to_space(b"\xe2\x80\x8b\xc2\xad"),
                b"\xe2\x80\x8b\xc2\xad"
            );
            assert_eq!(lower(x), b"\xc3\x89\xc4\xb0a\xff\xe1\xba\x9eshift");
            assert_eq!(grep_lines(b"a\n\n\xff\nb"), b"a\n\xff\nb");
        }
    }

    #[test]
    fn basename_as_coreutils() {
        assert_eq!(basename(b"/home/x/repo"), b"repo");
        assert_eq!(basename(b"/home/x/repo//"), b"repo");
        assert_eq!(basename(b"/"), b"/");
        assert_eq!(basename(b"//"), b"/");
        assert_eq!(basename(b""), b"");
        assert_eq!(basename(b"repo"), b"repo");
    }

    // What bash 5.3's printf %q wrote for each.
    #[test]
    fn quote_as_bash() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"a b", b"a\\ b"),
            (b"a,b", b"a\\,b"),
            (b"~a", b"\\~a"),
            (b"a~", b"a~"),
            (b"a:~b", b"a:\\~b"),
            (b"a=~b", b"a=\\~b"),
            (b"#a", b"\\#a"),
            (b"a#", b"a#"),
            (b"a=b", b"a=b"),
            (b"a%b+c@d-e", b"a%b+c@d-e"),
            (b"", b"''"),
            (b"a{b}", b"a\\{b\\}"),
            (b"a&;()<>b", b"a\\&\\;\\(\\)\\<\\>b"),
            (b"a*?[]", b"a\\*\\?\\[\\]"),
            (b"a\\b", b"a\\\\b"),
            (b"a'b", b"a\\'b"),
            ("é x".as_bytes(), "é\\ x".as_bytes()),
            (b"x\ny", b"$'x\\ny'"),
            (b"a\tb", b"$'a\\tb'"),
            (b"\x01", b"$'\\001'"),
            (b"a\x7fb", b"$'a\\177b'"),
            (b"\xc3x", b"$'\\303x'"),
            (
                b"a\x1bb\rc\x0bd\x0ce\x08f\x07g",
                b"$'a\\Eb\\rc\\vd\\fe\\bf\\ag'",
            ),
            (
                b"/run/user/1000/omabox/x/run/wayland-1",
                b"/run/user/1000/omabox/x/run/wayland-1",
            ),
        ];
        for (i, o) in cases {
            assert_eq!(
                String::from_utf8_lossy(&quote_q(i)),
                String::from_utf8_lossy(o)
            );
        }
    }

    #[test]
    fn quoted_for_bash() {
        assert_eq!(quote_at(b"a b"), b"'a b'");
        assert_eq!(quote_at(b"it's"), b"'it'\\''s'");
        assert_eq!(quote_at(b""), b"''");
        assert_eq!(quote_at(b"a\nb"), b"$'a\\nb'");
    }

    #[test]
    fn characters_and_spaces() {
        assert_eq!(head_chars("ééé".as_bytes(), 2), "éé".as_bytes());
        assert_eq!(head_chars(b"\xff\xfeab", 3), b"\xff\xfea");
        assert!(all_space(b" \t\n\x0b"));
        assert!(all_space("\u{a0}\u{3000}\u{2028}".as_bytes()));
        assert!(!all_space(b" x"));
        assert!(!all_space(b"\xff"));
        assert_eq!(subst(b"a\0b\n\n"), b"ab");
    }

    // What bash 5.3 counted (`x=é; ${#x}`) with each environment: setlocale(LC_ALL, "") failing makes
    // it set LC_ALL's, or LANG's then each LC_*, one by one.
    #[test]
    fn locale_as_bash_sets_it() {
        let valid = |v: &[u8]| matches!(v, b"" | b"C" | b"C.UTF-8" | b"en_US.UTF-8");
        let run = |env: &[(&str, &str)]| -> (bool, Vec<(libc::c_int, Vec<u8>)>) {
            let get = |k: &str| {
                env.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.as_bytes().to_vec())
            };
            let mut calls = Vec::new();
            let mut set = |c: libc::c_int, v: &[u8]| {
                // ("": every category's own, all of them valid or the call fails)
                let ok = if v.is_empty() {
                    env.iter().all(|(_, v)| valid(v.as_bytes()))
                } else {
                    valid(v)
                };
                if ok {
                    calls.push((c, v.to_vec()));
                }
                ok
            };
            let whole = bash_locale(&get, &mut set);
            (whole, calls)
        };
        let ctype = |calls: &[(libc::c_int, Vec<u8>)]| {
            calls
                .iter()
                .rev()
                .find(|(c, _)| *c == libc::LC_CTYPE || *c == libc::LC_ALL)
                .map(|(_, v)| v.clone())
        };
        let (w, c) = run(&[("LANG", "en_US.UTF-8")]);
        assert!(w);
        assert_eq!(ctype(&c), Some(b"".to_vec()));
        let (w, c) = run(&[("LANG", "en_US.UTF-8"), ("LC_TIME", "xx_XX.UTF-8")]);
        assert!(!w);
        assert_eq!(ctype(&c), Some(b"en_US.UTF-8".to_vec()));
        let (_, c) = run(&[("LANG", "en_US.UTF-8"), ("LC_CTYPE", "xx")]);
        assert_eq!(ctype(&c), Some(b"en_US.UTF-8".to_vec()));
        let (_, c) = run(&[("LANG", "xx"), ("LC_CTYPE", "en_US.UTF-8")]);
        assert_eq!(ctype(&c), None);
        let (_, c) = run(&[("LC_ALL", "xx"), ("LANG", "en_US.UTF-8")]);
        assert_eq!(ctype(&c), None);
        let (_, c) = run(&[
            ("LANG", "C"),
            ("LC_CTYPE", "en_US.UTF-8"),
            ("LC_TIME", "xx"),
        ]);
        assert_eq!(ctype(&c), Some(b"en_US.UTF-8".to_vec()));
        let (_, c) = run(&[("LC_ALL", "C.UTF-8"), ("LANG", "xx")]);
        assert_eq!(ctype(&c), Some(b"C.UTF-8".to_vec()));
        let (_, c) = run(&[("LC_CTYPE", "en_US.UTF-8"), ("LC_TIME", "xx")]);
        assert_eq!(ctype(&c), None);
    }

    #[test]
    fn read_splits_as_bash() {
        let f = |v: &[u8], n| read_fields(v, n);
        assert_eq!(f(b"  a  b c  \nx", 2), vec![b"a".to_vec(), b"b c".to_vec()]);
        assert_eq!(f(b"a", 3), vec![b"a".to_vec(), b"".to_vec(), b"".to_vec()]);
        assert_eq!(
            f(b"1 2 3", 3),
            vec![b"1".to_vec(), b"2".to_vec(), b"3".to_vec()]
        );
    }

    #[test]
    fn words_split_at_blanks() {
        assert_eq!(
            words(b" a\tb\n\nc "),
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
        );
    }

    #[test]
    fn numbers_are_decimal() {
        assert_eq!(dec(b"12"), Some(12));
        assert_eq!(dec(b"-5"), Some(-5));
        assert_eq!(dec(b"+5"), Some(5));
        assert_eq!(dec(b"010"), Some(10));
        assert_eq!(dec(b"08"), Some(8));
        assert_eq!(dec(b"0042"), Some(42));
        assert_eq!(dec(b"0"), Some(0));
        assert_eq!(dec(b"0x1f"), None);
        assert_eq!(dec(b" 3 "), None);
        assert_eq!(dec(b"x"), None);
        assert_eq!(dec(b""), None);
        assert_eq!(dec(b"-"), None);
        assert_eq!(dec("²".as_bytes()), None);
        assert_eq!(dec(b"9223372036854775807"), Some(i64::MAX));
        assert_eq!(dec(b"-9223372036854775808"), Some(i64::MIN));
        assert_eq!(dec(b"9223372036854775808"), None);
        assert_eq!(dec(b"9999999999999999999"), None);
        assert!(is_num(b"007") && !is_num(b"-1") && !is_num(b"99999999999999999999"));
        assert_eq!(test_int(b" 08 "), Some(8));
    }

    #[test]
    fn regex_groups() {
        assert_eq!(
            re_group(
                r"^omabox\.run-([0-9]{1,19})\.exit$",
                b"omabox.run-123.exit",
                1
            ),
            Some(b"123".to_vec())
        );
        assert!(!re_match("^[0-9]+$", b"12a"));
        assert!(re_match("^[0-9]+$", b"12"));
        assert!(Re::new("x").is_some_and(|r| r.captures(b"a\0x", false).is_some()));
    }

    // In en_US.UTF-8 glibc's `[0-9]` takes `²` and `[A-Za-z]` `é` (bash's `=~`): omabox's own
    // patterns are ASCII in every locale, and `.` takes any byte; a caller's follow the locale.
    #[test]
    fn own_patterns_ascii_in_any_locale() {
        let _l = ThreadLocale::new(c"en_US.UTF-8");
        assert!(!re_match("^[0-9]+$", "²".as_bytes()));
        assert!(!re_match("^[0-9]+$", "١".as_bytes()));
        assert!(!re_match(
            "^[A-Za-z0-9][A-Za-z0-9_.-]{0,39}$",
            "é".as_bytes()
        ));
        assert!(re_match("^[A-Za-z0-9][A-Za-z0-9_.-]{0,39}$", b"box-1.a"));
        assert!(re_match("^x.y$", b"x\xffy"));
        assert!(Re::from_bytes(b"^[A-Za-z]$", false).is_some_and(|r| r.is_match("é".as_bytes())));
    }
}
