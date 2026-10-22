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

/// `$(printf '%s' "$V" | cut -c1-N)`: each line's first N bytes (GNU cut counts bytes).
pub fn cut_c(v: &[u8], n: usize) -> Vec<u8> {
    let out: Vec<&[u8]> = v
        .split(|&c| c == b'\n')
        .map(|l| &l[..l.len().min(n)])
        .collect();
    strip_nl(&out.join(&b'\n'))
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
        let pat = CString::new(pattern).ok()?;
        // SAFETY: regex_t is plain data that regcomp fills in; zeroed is a valid starting value.
        let mut re: Box<libc::regex_t> = Box::new(unsafe { std::mem::zeroed() });
        // SAFETY: re points to a regex_t we own; pat is a valid C string.
        let rc = unsafe { libc::regcomp(&mut *re, pat.as_ptr(), libc::REG_EXTENDED) };
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

    #[test]
    fn cut_counts_bytes_per_line() {
        assert_eq!(cut_c(b"abcdef", 3), b"abc");
        assert_eq!(cut_c(b"abcdef\nxyzw", 2), b"ab\nxy");
        assert_eq!(cut_c("é".as_bytes(), 1), b"\xc3");
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
