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
}

/// The length of the character V starts with, in the locale init_locale set: 1 in a single-byte
/// locale, for a NUL, and for a byte that starts no character there.
fn mb_len(v: &[u8], cut: bool) -> usize {
    // SAFETY: reads the current locale's constant.
    if v.is_empty() || unsafe { __ctype_get_mb_cur_max() } <= 1 {
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

/// Sets the locale from the environment, as bash does at its start: glob order and `=~` follow it.
pub fn init_locale() {
    // SAFETY: called once at the start, before any other thread; the argument is a valid C string.
    unsafe {
        libc::setlocale(libc::LC_ALL, c"".as_ptr());
    }
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

    #[test]
    fn words_split_at_blanks() {
        assert_eq!(
            words(b" a\tb\n\nc "),
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
        );
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
