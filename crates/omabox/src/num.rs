//! Numbers as bin/omabox writes and reads them: the one duration grammar (ms_duration, #273), awk's
//! printf of seconds (ms_s), and awk's reading and writing of a number in a field.

use crate::shell::Re;

/// ms_duration: the one duration grammar (#273): 300ms, 2s, 1.5s, 2m, 1h, or a bare number of
/// seconds, as milliseconds (at least 1).
pub fn ms_duration(v: &[u8]) -> Option<i64> {
    let re = Re::new(r"^([0-9]{1,6}(\.[0-9]{1,6})?)(ms|s|m|h)?$")?;
    let n: f64 = String::from_utf8_lossy(&re.group(v, 1)?).parse().ok()?;
    let unit = re.group(v, 3).unwrap_or_default();
    let mult = match unit.as_slice() {
        b"ms" => 1.0,
        b"h" => 3_600_000.0,
        b"m" => 60_000.0,
        _ => 1000.0,
    };
    let ms = (n * mult + 0.5).trunc();
    (ms >= 1.0).then_some(ms as i64)
}

/// `printf "%.Nf"` of X, as awk and printf(1) write it (glibc's rounding of the exact value).
pub fn fixed(x: f64, prec: usize) -> String {
    let fmt = format!("%.{prec}f\0");
    let mut buf = [0u8; 512];
    // SAFETY: buf has its length given; fmt is NUL-ended and takes one double.
    let n = unsafe { libc::snprintf(buf.as_mut_ptr().cast(), buf.len(), fmt.as_ptr().cast(), x) };
    let n = usize::try_from(n).unwrap_or(0).min(buf.len() - 1);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// ms_s MS: milliseconds as seconds, 2 decimals (`awk 'BEGIN { printf "%.2fs", m / 1000 }'`): 430 →
/// 0.43s.
pub fn ms_s(ms: impl AsRef<[u8]>) -> String {
    format!("{}s", fixed(awk_num(ms.as_ref()) / 1000.0, 2))
}

/// awk's value of a string as a number: its leading number (blanks first), 0 for none.
pub fn awk_num(v: &[u8]) -> f64 {
    let s = String::from_utf8_lossy(v);
    let t = s.trim_start_matches([' ', '\t', '\n']);
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == ds || (i == ds + 1 && b[ds] == b'.') {
        return 0.0;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    t[..i].parse().unwrap_or(0.0)
}

/// A number as awk writes it with %s or print: an integral value as an integer, else CONVFMT's %.6g.
pub fn awk_str(x: f64) -> String {
    if x == x.trunc() && x.abs() < 1e16 {
        format!("{}", x as i64)
    } else {
        let mut buf = [0u8; 64];
        // SAFETY: buf has its length given; the format is NUL-ended and takes one double.
        let n = unsafe { libc::snprintf(buf.as_mut_ptr().cast(), buf.len(), c"%.6g".as_ptr(), x) };
        let n = usize::try_from(n).unwrap_or(0).min(buf.len() - 1);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

/// `awk -F, '{ printf "%s,%s %sx%s", $1, $2, $3, $4 }' <<<"$V"`: X,Y,W,H as "X,Y WxH" (the fields as
/// they are, missing ones empty), of V's first line.
pub fn xywh_words(v: &[u8]) -> Vec<u8> {
    let line = v.split(|&c| c == b'\n').next().unwrap_or_default();
    let f: Vec<&[u8]> = line.split(|&c| c == b',').collect();
    let g = |i: usize| f.get(i).copied().unwrap_or_default();
    crate::cat!(g(0), ",", g(1), " ", g(2), "x", g(3))
}

/// The same with the place moved back by OX OY (`$1 - ox, $2 - oy`: awk's numbers).
pub fn xywh_less(v: &[u8], ox: &[u8], oy: &[u8]) -> Vec<u8> {
    let line = v.split(|&c| c == b'\n').next().unwrap_or_default();
    let f: Vec<&[u8]> = line.split(|&c| c == b',').collect();
    let g = |i: usize| f.get(i).copied().unwrap_or_default();
    crate::cat!(
        awk_str(awk_num(g(0)) - awk_num(ox)),
        ",",
        awk_str(awk_num(g(1)) - awk_num(oy)),
        " ",
        g(2),
        "x",
        g(3)
    )
}

/// A value as `awk -v NAME=VALUE` makes it: its escapes (\\n, \\t, \\\\, octal...) read as in an awk
/// string.
pub fn awk_v(v: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len());
    let mut i = 0;
    while i < v.len() {
        if v[i] != b'\\' || i + 1 == v.len() {
            out.push(v[i]);
            i += 1;
            continue;
        }
        i += 1;
        let c = v[i];
        i += 1;
        match c {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut n = u32::from(c - b'0');
                let mut k = 1;
                while k < 3 && i < v.len() && (b'0'..=b'7').contains(&v[i]) {
                    n = n * 8 + u32::from(v[i] - b'0');
                    i += 1;
                    k += 1;
                }
                out.push(u8::try_from(n & 0xff).unwrap_or(0));
            }
            // (\\, \", \/ and any other: the character itself)
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(ms_duration(b"500ms"), Some(500));
        assert_eq!(ms_duration(b"1.5s"), Some(1500));
        assert_eq!(ms_duration(b"2m"), Some(120_000));
        assert_eq!(ms_duration(b"3"), Some(3000));
        assert_eq!(ms_duration(b"0.0004"), None);
        assert_eq!(ms_duration(b"0.0005"), Some(1));
        assert_eq!(ms_duration(b"x"), None);
    }

    // What gawk's printf "%.2fs" wrote for each.
    #[test]
    fn seconds_as_awk() {
        assert_eq!(ms_s(b"430"), "0.43s");
        assert_eq!(ms_s(b"125"), "0.12s");
        assert_eq!(ms_s(b"135"), "0.14s");
        assert_eq!(ms_s(b"5"), "0.01s");
        assert_eq!(ms_s(b"0"), "0.00s");
        assert_eq!(ms_s(b""), "0.00s");
        assert_eq!(ms_s(b"12x"), "0.01s");
        assert_eq!(ms_s(b"600000"), "600.00s");
    }

    #[test]
    fn awk_numbers() {
        assert_eq!(awk_num(b" 12abc"), 12.0);
        assert_eq!(awk_num(b"-3.5e2x"), -350.0);
        assert_eq!(awk_num(b"x"), 0.0);
        assert_eq!(awk_num(b"."), 0.0);
        assert_eq!(awk_str(5.0), "5");
        assert_eq!(awk_str(-0.5), "-0.5");
        assert_eq!(awk_str(1.0 / 3.0), "0.333333");
        assert_eq!(xywh_words(b"1,2,30,40"), b"1,2 30x40");
        assert_eq!(xywh_words(b"1,2"), b"1,2 x");
        assert_eq!(xywh_less(b"110,220,30,40", b"100", b"200"), b"10,20 30x40");
        assert_eq!(awk_v(b"a\\tb\\\\c\\101"), b"a\tb\\cA");
    }
}
