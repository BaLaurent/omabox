//! The JSON files bin/omabox reads and writes with jq (box.json, info.json, the jobs' records), read
//! and written as jq 1.8 does, strings escaped as jq escapes them, keys in their order; but numbers
//! in standard form (`1e3` is `1000`, where jq wrote decNumber's `1E+3`: finding 449).

use serde_json::{Map, Value};

/// Every JSON value in TEXT, in order, as jq reads a file: the ones before a syntax error, and
/// whether there was one.
pub fn values(text: &[u8]) -> (Vec<Value>, bool) {
    let mut out = Vec::new();
    for v in serde_json::Deserializer::from_slice(text).into_iter::<Value>() {
        match v {
            Ok(v) => out.push(v),
            Err(_) => return (out, false),
        }
    }
    (out, true)
}

/// `jq -r '.[$k] // empty' FILE 2>/dev/null`, as `$(...)`: each value's KEY in its raw form, a line
/// each, and whether jq would exit 0 (a missing file, a syntax error, a value that is no object:
/// not, the output before or around it kept).
pub fn field(file: &std::path::Path, key: &str) -> (Vec<u8>, bool) {
    let Ok(text) = std::fs::read(file) else {
        return (Vec::new(), false);
    };
    let (vals, mut ok) = values(&text);
    let mut out = Vec::new();
    for v in &vals {
        let f = match v {
            Value::Object(m) => m.get(key),
            Value::Null => None,
            _ => {
                ok = false;
                continue;
            }
        };
        match f {
            None | Some(Value::Null | Value::Bool(false)) => {}
            Some(x) => {
                out.extend_from_slice(raw(x).as_bytes());
                out.push(b'\n');
            }
        }
    }
    (crate::shell::strip_nl(&out), ok)
}

/// A value as `jq -r` prints it: a string as it is, anything else as jq writes it.
pub fn raw(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => pretty(v),
    }
}

/// A value as jq writes it by default: two spaces an indent.
pub fn pretty(v: &Value) -> String {
    let mut out = String::new();
    write(&mut out, v, Some(0));
    out
}

/// A value as `jq -c` writes it.
pub fn compact(v: &Value) -> String {
    let mut out = String::new();
    write(&mut out, v, None);
    out
}

fn write(out: &mut String, v: &Value, indent: Option<usize>) {
    let nl = |out: &mut String, n: usize| {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', n * 2));
    };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number(&n.to_string())),
        Value::String(s) => string(out, s),
        Value::Array(a) if a.is_empty() => out.push_str("[]"),
        Value::Object(m) if m.is_empty() => out.push_str("{}"),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if let Some(n) = indent {
                    nl(out, n + 1);
                }
                write(out, x, indent.map(|n| n + 1));
            }
            if let Some(n) = indent {
                nl(out, n);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if let Some(n) = indent {
                    nl(out, n + 1);
                }
                string(out, k);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write(out, x, indent.map(|n| n + 1));
            }
            if let Some(n) = indent {
                nl(out, n);
            }
            out.push('}');
        }
    }
}

/// A string as jq writes it, quoted and escaped.
pub fn quote(s: &str) -> String {
    let mut out = String::new();
    string(&mut out, s);
    out
}

fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A JSON number literal in standard form (finding 449): as read when it has no exponent (`1.50`,
/// `-0`, `100000000000000000000`; zeros in front of the point dropped); with one, its digits written out plain while the point stays
/// within 21 places of them (`1e3` is `1000`, `1.5e-7` `0.00000015`), else in exponent form (`1e+21`,
/// `1.5e-8`), as JavaScript writes a number. jq 1.8 wrote decNumber's (`1E+3`, `1.5E-7`).
pub fn number(lit: &str) -> String {
    let (neg, rest) = match lit.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, lit),
    };
    let Some(e) = rest.find(['e', 'E']) else {
        // (zeros in front of the point dropped: a time as events.log has it, 0001.500, is 1.500)
        let (int, frac) = rest
            .split_once('.')
            .map_or((rest, None), |(i, f)| (i, Some(f)));
        let int = match int.trim_start_matches('0') {
            "" => "0",
            i => i,
        };
        let sign = if neg { "-" } else { "" };
        return match frac {
            Some(f) => format!("{sign}{int}.{f}"),
            None => format!("{sign}{int}"),
        };
    };
    let (mant, exp) = (&rest[..e], rest[e + 1..].parse::<i64>().unwrap_or(0));
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let mut digits: String = format!("{int}{frac}");
    let exp = exp.saturating_sub(i64::try_from(frac.len()).unwrap_or(0));
    let lead = digits.len() - digits.trim_start_matches('0').len();
    digits.drain(..lead.min(digits.len().saturating_sub(1)));
    let sign = if neg { "-" } else { "" };
    if digits.chars().all(|c| c == '0') {
        return format!("{sign}0");
    }
    let n = i64::try_from(digits.len()).unwrap_or(0);
    let adjusted = exp.saturating_add(n - 1);
    if (-7..21).contains(&adjusted) {
        if exp >= 0 {
            let zeros = "0".repeat(usize::try_from(exp).unwrap_or(0));
            return format!("{sign}{digits}{zeros}");
        }
        let point = n + exp;
        if point > 0 {
            let p = usize::try_from(point).unwrap_or(0);
            return format!("{sign}{}.{}", &digits[..p], &digits[p..]);
        }
        let zeros = "0".repeat(usize::try_from(-point).unwrap_or(0));
        return format!("{sign}0.{zeros}{digits}");
    }
    let (first, tail) = digits.split_at(1);
    let dot = if tail.is_empty() { "" } else { "." };
    let esign = if adjusted >= 0 { "+" } else { "-" };
    format!(
        "{sign}{first}{dot}{tail}e{esign}{}",
        adjusted.unsigned_abs()
    )
}

/// `jq '.K1 = V1 | .K2 = V2'` on one value: an object gets the keys (in place when it has them, at
/// its end when not), null becomes an object; anything else is jq's error.
pub fn set(v: &mut Value, pairs: Vec<(&str, Value)>) -> bool {
    if v.is_null() {
        *v = Value::Object(Map::new());
    }
    let Value::Object(m) = v else {
        return false;
    };
    for (k, x) in pairs {
        m.insert(k.to_string(), x);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // Standard forms (finding 449); jq 1.8.2 printed 1E+3, 1.0E-5 as 0.000010, 1E+21, 1.5E-8.
    #[test]
    fn numbers_in_standard_form() {
        for (lit, want) in [
            ("1.50", "1.50"),
            ("1e3", "1000"),
            ("1E+3", "1000"),
            ("-2.5e2", "-250"),
            ("1.0E-5", "0.000010"),
            ("1.5e-7", "0.00000015"),
            ("1.5e-8", "1.5e-8"),
            ("1e20", "100000000000000000000"),
            ("1e21", "1e+21"),
            ("1e300", "1e+300"),
            ("100000000000000000000", "100000000000000000000"),
            ("0.0000001", "0.0000001"),
            ("0001.500", "1.500"),
            ("000", "0"),
            ("0e5", "0"),
            ("-0", "-0"),
            ("12345", "12345"),
            ("0.5", "0.5"),
            ("1.25", "1.25"),
        ] {
            assert_eq!(number(lit), want, "{lit}");
        }
    }

    #[test]
    fn pretty_as_jq() {
        let (v, ok) = values(br#"{"a":1.50,"e":[],"f":{},"h":[1,{"x":null}],"d":"\u007f\u001f<"}"#);
        assert!(ok);
        assert_eq!(
            pretty(&v[0]),
            "{\n  \"a\": 1.50,\n  \"e\": [],\n  \"f\": {},\n  \"h\": [\n    1,\n    {\n      \"x\": null\n    }\n  ],\n  \"d\": \"\\u007f\\u001f<\"\n}"
        );
        assert_eq!(
            compact(&v[0]),
            r#"{"a":1.50,"e":[],"f":{},"h":[1,{"x":null}],"d":"\u007f\u001f<"}"#
        );
    }

    #[test]
    fn set_keeps_order() {
        let (mut v, _) = values(br#"{"agent":1,"b":2}"#);
        assert!(set(
            &mut v[0],
            vec![("agent", 5.into()), ("agent_start", "9".into())]
        ));
        assert_eq!(compact(&v[0]), r#"{"agent":5,"b":2,"agent_start":"9"}"#);
    }

    #[test]
    fn stream_stops_at_an_error() {
        let (v, ok) = values(b"{} 1 x");
        assert_eq!(v.len(), 2);
        assert!(!ok);
    }
}
