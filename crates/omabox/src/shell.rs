//! What bash and coreutils do to the words bin/omabox handles, done the same way here: `$(...)`'s
//! trailing newlines, echo's options, cut, basename, printf %q, glob order, word splitting, and `=~`
//! (glibc's regex in the user's locale, where `[0-9]` and `[A-Za-z]` take more than ASCII).

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

/// `$(echo "$V")`: echo takes a word of -n, -e and -E alone as its options, and prints nothing.
pub fn echo_sub(v: &[u8]) -> Vec<u8> {
    if v.len() > 1 && v[0] == b'-' && v[1..].iter().all(|c| matches!(c, b'n' | b'e' | b'E')) {
        return Vec::new();
    }
    strip_nl(v)
}

/// `$(printf '%s' "$V" | cut -c1-N)`: each line's first N characters. coreutils 9.12's cut counts
/// characters in a multibyte locale (#324: 9.x stopped counting bytes there), as mbrtowc reads them
/// in the caller's locale, a byte that starts none (invalid, cut short) counted as one; in a
/// single-byte one (C, POSIX) bytes.
pub fn cut_c(v: &[u8], n: usize) -> Vec<u8> {
    let out: Vec<&[u8]> = v
        .split(|&c| c == b'\n')
        .map(|l| chars(l, n, true))
        .collect();
    strip_nl(&out.join(&b'\n'))
}

/// `${V:0:N}`: the first N characters, as bash reads them (glibc's mbrtowc in the locale), a byte
/// that starts none counted as one.
pub fn head_chars(v: &[u8], n: usize) -> &[u8] {
    chars(v, n, false)
}

/// `cut -c1-N` of one line (no newline in it).
pub fn cut_line(l: &[u8], n: usize) -> &[u8] {
    chars(l, n, true)
}

/// The first N characters of V. CUT: as coreutils' mcel reads one, which takes none past U+10FFFF
/// (glibc's mbrtowc takes up to 0x7FFFFFFF: bash's substring counts those as one).
fn chars(v: &[u8], n: usize, cut: bool) -> &[u8] {
    let (mut i, mut k) = (0, 0);
    while i < v.len() && k < n {
        i += mb_len(&v[i..], cut);
        k += 1;
    }
    &v[..i]
}

unsafe extern "C" {
    fn mbrtowc(
        pwc: *mut libc::wchar_t,
        s: *const libc::c_char,
        n: libc::size_t,
        ps: *mut libc::mbstate_t,
    ) -> libc::size_t;
    fn __ctype_get_mb_cur_max() -> libc::size_t;
    fn wcrtomb(s: *mut libc::c_char, wc: libc::wchar_t, ps: *mut libc::mbstate_t) -> libc::size_t;
    fn iswcntrl(wc: u32) -> libc::c_int;
    fn towlower(wc: u32) -> u32;
}

/// Whether the locale's characters may take more than a byte.
fn multibyte() -> bool {
    // SAFETY: reads the current locale's constant.
    unsafe { __ctype_get_mb_cur_max() > 1 }
}

/// `${#V}`: V's length in characters as bash counts them (a byte that starts none is one).
pub fn char_count(v: &[u8]) -> usize {
    let (mut i, mut k) = (0, 0);
    while i < v.len() {
        i += mb_len(&v[i..], false);
        k += 1;
    }
    k
}

/// Each character of V as bash's pattern matching reads it, with its bytes: Some(wide character),
/// or None for a byte that starts none; bytes alone in a single-byte locale.
fn mb_chars(v: &[u8]) -> Vec<(Option<u32>, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    let multi = multibyte();
    while i < v.len() {
        if !multi {
            out.push((None, &v[i..=i]));
            i += 1;
            continue;
        }
        // SAFETY: an mbstate_t of zeroes is the initial state; v[i..] is valid for its length.
        let (r, wc) = unsafe {
            let mut st: libc::mbstate_t = std::mem::zeroed();
            let mut wc: libc::wchar_t = 0;
            let r = mbrtowc(&mut wc, v[i..].as_ptr().cast(), v.len() - i, &mut st);
            (r, wc)
        };
        let n = match r {
            0 => 1,
            r if r > v.len() - i => {
                out.push((None, &v[i..=i]));
                i += 1;
                continue;
            }
            r => r,
        };
        out.push((Some(wc as u32), &v[i..i + n]));
        i += n;
    }
    out
}

/// A wide character in the locale's bytes (wcrtomb); None when it has none there.
fn wc_bytes(wc: u32) -> Option<Vec<u8>> {
    let mut buf = [0u8; 16];
    // SAFETY: buf holds MB_LEN_MAX (16) bytes; an mbstate_t of zeroes is the initial state.
    let n = unsafe {
        let mut st: libc::mbstate_t = std::mem::zeroed();
        wcrtomb(buf.as_mut_ptr().cast(), wc as libc::wchar_t, &mut st)
    };
    (n <= buf.len()).then(|| buf[..n].to_vec())
}

/// `${V//[[:cntrl:]]/ }`: each control character (the locale's) a space. A V with a byte that starts
/// no character is matched byte by byte, as bash falls back to then (iscntrl: none past 0x7f).
pub fn cntrl_to_space(v: &[u8]) -> Vec<u8> {
    let cs = mb_chars(v);
    let wide = multibyte() && cs.iter().all(|(c, _)| c.is_some());
    let mut out = Vec::with_capacity(v.len());
    if !wide {
        for &b in v {
            // SAFETY: iscntrl reads the locale's tables only.
            let ctl = unsafe { libc::iscntrl(libc::c_int::from(b)) != 0 };
            out.push(if ctl { b' ' } else { b });
        }
        return out;
    }
    for (c, b) in cs {
        // SAFETY: iswcntrl reads the locale's tables only.
        if c.is_some_and(|wc| unsafe { iswcntrl(wc) != 0 }) {
            out.push(b' ');
        } else {
            out.extend_from_slice(b);
        }
    }
    out
}

/// `${V,,}`: V in lower case, as the locale's towlower (tolower) has each character.
pub fn lower(v: &[u8]) -> Vec<u8> {
    let multi = multibyte();
    let mut out = Vec::with_capacity(v.len());
    for (c, b) in mb_chars(v) {
        match c {
            Some(wc) => {
                // SAFETY: towlower reads the locale's tables only.
                let l = unsafe { towlower(wc) };
                match (l != wc).then(|| wc_bytes(l)).flatten() {
                    Some(lb) => out.extend(lb),
                    None => out.extend_from_slice(b),
                }
            }
            None if multi => out.push(b[0]),
            None => {
                // SAFETY: tolower reads the locale's tables only.
                let l = unsafe { libc::tolower(libc::c_int::from(b[0])) };
                out.push(u8::try_from(l).unwrap_or(b[0]));
            }
        }
    }
    out
}

/// The length of the character V starts with, in the locale init_locale set: 1 in a single-byte
/// locale, for a NUL, and for a byte that starts no character there.
fn mb_len(v: &[u8], cut: bool) -> usize {
    // (cut's own setlocale(LC_ALL, "") failing leaves it in C: bytes, whatever bash set)
    let c_locale = cut && !LOCALE_WHOLE.load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: reads the current locale's constant.
    if v.is_empty() || c_locale || unsafe { __ctype_get_mb_cur_max() } <= 1 {
        return 1;
    }
    // SAFETY: an mbstate_t of zeroes is the initial state; v is valid for v.len() bytes; wc is ours.
    let r = unsafe {
        let mut st: libc::mbstate_t = std::mem::zeroed();
        let mut wc: libc::wchar_t = 0;
        let r = mbrtowc(&mut wc, v.as_ptr().cast(), v.len(), &mut st);
        if cut && wc as u32 > 0x10ffff {
            usize::MAX
        } else {
            r
        }
    };
    match r {
        0 => 1,
        r if r > v.len() => 1, // (size_t)-1, -2, or past U+10FFFF for cut: no character there
        r => r,
    }
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

unsafe extern "C" {
    fn iswspace(c: u32) -> libc::c_int;
}

/// `[ -z "${V//[[:space:]]/}" ]` in the user's locale: V holds nothing but spaces (iswspace).
pub fn all_space(v: &[u8]) -> bool {
    match std::str::from_utf8(v) {
        // SAFETY: iswspace only classifies the value.
        Ok(s) => s.chars().all(|c| unsafe { iswspace(c as u32) } != 0),
        Err(_) => false,
    }
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

/// Whether the whole of setlocale(LC_ALL, "") took (coreutils' own start: cut counts bytes where it
/// did not).
static LOCALE_WHOLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The categories bash sets one by one (locale.c's reset_locale_vars).
const CATEGORIES: [(libc::c_int, &str); 5] = [
    (libc::LC_CTYPE, "LC_CTYPE"),
    (libc::LC_COLLATE, "LC_COLLATE"),
    (libc::LC_MESSAGES, "LC_MESSAGES"),
    (libc::LC_NUMERIC, "LC_NUMERIC"),
    (libc::LC_TIME, "LC_TIME"),
];

/// The locale as bash sets it at its start (glob order, `=~`, `${V:0:N}` follow it): setlocale(LC_ALL,
/// "") first; where that fails as a whole (one category's name no locale), LC_ALL's value for every
/// category when it is set; else LANG's for every one, then each category's own LC_* over it, one
/// by one (one that fails keeps LANG's). LANG no locale either: C. GET reads the environment, SET is
/// setlocale's; true when the first call took.
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

/// Sets the locale from the environment, as bash does at its start (bash_locale).
pub fn init_locale() {
    let get = |k: &str| env(k);
    let mut set = |c: libc::c_int, v: &[u8]| -> bool {
        let Ok(v) = CString::new(v.to_vec()) else {
            return false;
        };
        // SAFETY: called at the start, before any other thread; v is a valid C string.
        unsafe { !libc::setlocale(c, v.as_ptr()).is_null() }
    };
    let whole = bash_locale(&get, &mut set);
    LOCALE_WHOLE.store(whole, std::sync::atomic::Ordering::Relaxed);
}

/// Bytes compared as the locale collates them (strcoll), as bash sorts a glob's matches.
pub fn coll_cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    match (CString::new(a), CString::new(b)) {
        (Ok(x), Ok(y)) => {
            // SAFETY: both are valid NUL-terminated strings that outlive the call.
            let r = unsafe { libc::strcoll(x.as_ptr(), y.as_ptr()) };
            r.cmp(&0).then_with(|| a.cmp(b))
        }
        _ => a.cmp(b),
    }
}

/// `"$DIR"/*` (or `*/` with `dirs`): the names of DIR's entries but hidden ones, in glob order.
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
    names.sort_by(|a, b| coll_cmp(a, b));
    names
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
    let o = crate::sig::command("mktemp")
        .args(args.iter().map(|a| OsStr::from_bytes(a)))
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|_| crate::exit::Exit(1))?;
    if !o.status.success() {
        return Err(crate::exit::Exit(
            u8::try_from(crate::exec::status(o.status).0).unwrap_or(1),
        ));
    }
    let p = strip_nl(&o.stdout);
    let dir = args.first().is_some_and(|a| *a == b"-d");
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(!dir)
        .open(path(&p))
        .map_err(|_| crate::exit::Exit(1))?;
    Ok((f, p))
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

/// A POSIX extended regular expression, compiled by glibc in the user's locale: bash's `=~`.
pub struct Re(Box<libc::regex_t>);

impl Re {
    pub fn new(pattern: &str) -> Option<Re> {
        Re::from_bytes(pattern.as_bytes(), false)
    }

    /// A pattern from bytes (a caller's `[[ x =~ $re ]]`); ICASE as under `shopt -s nocasematch`.
    /// None where regcomp refuses it (bash's status 2).
    pub fn from_bytes(pattern: &[u8], icase: bool) -> Option<Re> {
        let pat = CString::new(pattern).ok()?;
        // SAFETY: regex_t is plain data that regcomp fills in; zeroed is a valid starting value.
        let mut re: Box<libc::regex_t> = Box::new(unsafe { std::mem::zeroed() });
        let flags = libc::REG_EXTENDED | if icase { libc::REG_ICASE } else { 0 };
        // SAFETY: re points to a regex_t we own; pat is a valid C string.
        let rc = unsafe { libc::regcomp(&mut *re, pat.as_ptr(), flags) };
        (rc == 0).then_some(Re(re))
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
        // SAFETY: the regex is compiled; base is valid for rm_eo bytes; m has N entries.
        let rc = unsafe { libc::regexec(&*self.0, base.as_ptr().cast(), N, m.as_mut_ptr(), flags) };
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
        // SAFETY: compiled by regcomp in Re::new, freed once.
        unsafe { libc::regfree(&mut *self.0) }
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

/// Bash arithmetic's reading of a decimal word (`10#N`, `$((N))`): 64 bits, wrapping as bash does.
pub fn arith(v: &[u8]) -> Option<i64> {
    if v.is_empty() || !v.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(v.iter().fold(0i64, |n, &c| {
        n.wrapping_mul(10).wrapping_add(i64::from(c - b'0'))
    }))
}

/// Bash arithmetic's reading of a number word (`$((X + 1))` where X holds it): blanks around, a sign,
/// then decimal, octal after a 0, hex after 0x, wrapping at 64 bits. None where bash refuses it ("08:
/// value too great for base"), which bash says in its own words and ends the command list with; an
/// empty one (or blanks) is 0.
pub fn bash_int(v: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(v)
        .ok()?
        .trim_matches(|c: char| c == ' ' || c == '\t' || c == '\n');
    if s.is_empty() {
        return Some(0);
    }
    let (neg, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (base, digits) = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (16, h)
    } else if s.len() > 1 && s.starts_with('0') {
        (8, &s[1..])
    } else {
        (10, s)
    };
    if digits.is_empty() {
        return (base == 8).then_some(0);
    }
    let mut n: i64 = 0;
    for c in digits.chars() {
        let d = i64::from(c.to_digit(base)?);
        n = n.wrapping_mul(i64::from(base)).wrapping_add(d);
    }
    Some(if neg { n.wrapping_neg() } else { n })
}

/// `[ A -le B ]`'s reading of a number: optional blanks and sign, decimal digits; None for what
/// test refuses as no integer (it says so, and the test fails).
pub fn test_int(v: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(v)
        .ok()?
        .trim_matches(|c| c == ' ' || c == '\t');
    s.parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_takes_its_options() {
        assert_eq!(echo_sub(b"-n"), b"");
        assert_eq!(echo_sub(b"-neE"), b"");
        assert_eq!(echo_sub(b"-"), b"-");
        assert_eq!(echo_sub(b"-x"), b"-x");
        assert_eq!(echo_sub(b"a\n\n"), b"a");
    }

    // What coreutils 9.12's `cut -c` gave for each, in en_US.UTF-8 and in C.
    #[test]
    fn cut_counts_characters_in_utf8() {
        let _l = ThreadLocale::new(c"C.UTF-8");
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
        // (bash's substring takes glibc's word for it)
        assert_eq!(head_chars(b"\xf4\x90\x80\x80z", 1), b"\xf4\x90\x80\x80");
        assert_eq!(head_chars(b"x\xe2\x82y", 2), b"x\xe2");
        assert_eq!(cut_c("\u{1f600}z".as_bytes(), 1), "\u{1f600}".as_bytes());
        // A combining mark is a character of its own; a NUL is one.
        assert_eq!(cut_c("e\u{301}z".as_bytes(), 2), "e\u{301}".as_bytes());
        assert_eq!(cut_c(b"ab\0cd", 3), b"ab\0");
        drop(_l);
        let _c = ThreadLocale::new(c"C");
        assert_eq!(cut_c("é".as_bytes(), 1), b"\xc3");
        assert_eq!(cut_c("aéb".as_bytes(), 2), b"a\xc3");
        assert_eq!(head_chars("é".as_bytes(), 1), b"\xc3");
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

    // What bash 5.3 gave for each, in C.UTF-8 and in C.
    #[test]
    fn length_controls_and_case_as_bash() {
        let t = b"a\x01\xff\xc2\x85b\x7f\xe2\x80\xa8";
        let x = b"\xc3\x89\xc4\xb0A\xff\xe1\xba\x9e";
        {
            let _l = ThreadLocale::new(c"C.UTF-8");
            assert_eq!(char_count(t), 7);
            assert_eq!(cntrl_to_space(t), b"a \xff\xc2\x85b \xe2\x80\xa8");
            assert_eq!(cntrl_to_space(b"\xc2\x85\x01\xe2\x80\xa8"), b"   ");
            assert_eq!(
                cntrl_to_space(b"\xe2\x80\x8b\xc2\xad"),
                b"\xe2\x80\x8b\xc2\xad"
            );
            assert_eq!(lower(x), b"\xc3\xa9ia\xff\xc3\x9f");
        }
        let _l = ThreadLocale::new(c"C");
        assert_eq!(char_count(t), 10);
        assert_eq!(cntrl_to_space(t), b"a \xff\xc2\x85b \xe2\x80\xa8");
        assert_eq!(lower(x), b"\xc3\x89\xc4\xb0a\xff\xe1\xba\x9e");
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
        let _l = ThreadLocale::new(c"C.UTF-8");
        assert_eq!(head_chars("ééé".as_bytes(), 2), "éé".as_bytes());
        assert_eq!(head_chars(b"\xff\xfeab", 3), b"\xff\xfea");
        drop(_l);
        assert!(all_space(b" \t\n\x0b"));
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
    fn bash_reads_numbers() {
        assert_eq!(bash_int(b"12"), Some(12));
        assert_eq!(bash_int(b"-5"), Some(-5));
        assert_eq!(bash_int(b"010"), Some(8));
        assert_eq!(bash_int(b"08"), None);
        assert_eq!(bash_int(b"0x1f"), Some(31));
        assert_eq!(bash_int(b"0"), Some(0));
        assert_eq!(bash_int(b" 3 "), Some(3));
        assert_eq!(bash_int(b"x"), None);
        assert_eq!(bash_int(b""), Some(0));
        assert_eq!(bash_int(b" "), Some(0));
        assert_eq!(bash_int(b"-"), None);
    }

    #[test]
    fn arith_wraps() {
        assert_eq!(arith(b"0042"), Some(42));
        assert_eq!(arith(b"9999999999999999999"), Some(-8446744073709551617));
        assert_eq!(arith(b"x"), None);
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
}
