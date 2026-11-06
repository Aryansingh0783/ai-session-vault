//! JSONL helpers: `cwd` rewriting (portable <-> local) and conflict-free line-union merging.

use serde_json::Value;
use std::collections::HashSet;

fn rewrite_cwd(v: &mut Value, f: &dyn Fn(&str) -> String) -> bool {
    let mut changed = false;
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if k == "cwd" {
                    if let Value::String(s) = val {
                        let n = f(s);
                        if n != *s {
                            *s = n;
                            changed = true;
                        }
                        continue;
                    }
                }
                changed |= rewrite_cwd(val, f);
            }
        }
        Value::Array(a) => {
            for x in a {
                changed |= rewrite_cwd(x, f);
            }
        }
        _ => {}
    }
    changed
}

/// Rewrite every structured `"cwd"` field (message text is never touched).
/// Lines that don't change are kept byte-for-byte.
pub fn transform(data: &[u8], f: &dyn Fn(&str) -> String) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for line in data.split_inclusive(|b| *b == b'\n') {
        let t = trim_ascii(line);
        if t.first() == Some(&b'{') && memfind(t, b"\"cwd\"") {
            if let Ok(mut v) = serde_json::from_slice::<Value>(t) {
                if rewrite_cwd(&mut v, f) {
                    out.extend_from_slice(serde_json::to_string(&v).unwrap().as_bytes());
                    if line.ends_with(b"\r\n") {
                        out.extend_from_slice(b"\r\n");
                    } else if line.ends_with(b"\n") {
                        out.push(b'\n');
                    }
                    continue;
                }
            }
        }
        out.extend_from_slice(line);
    }
    out
}

fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while let Some((f, r)) = s.split_first() {
        if f.is_ascii_whitespace() {
            s = r
        } else {
            break;
        }
    }
    while let Some((l, r)) = s.split_last() {
        if l.is_ascii_whitespace() {
            s = r
        } else {
            break;
        }
    }
    s
}

fn memfind(h: &[u8], n: &[u8]) -> bool {
    h.windows(n.len()).any(|w| w == n)
}

/// Drop a trailing partial line (no newline): a session being written right now can be caught
/// mid-write, and shipping half a JSON line would corrupt the copy.
pub fn complete_lines(data: &[u8]) -> &[u8] {
    match data.iter().rposition(|b| *b == b'\n') {
        Some(i) => &data[..=i],
        None => if data.is_empty() { data } else { &data[..0] },
    }
}

/// How two versions of an append-only log relate.
#[derive(Debug, PartialEq, Eq)]
pub enum Relation {
    Same,
    /// `a` is a strict prefix of `b` (b is simply newer).
    ABehind,
    /// `b` is a strict prefix of `a`.
    BBehind,
    /// Both gained lines the other lacks.
    Diverged,
}

pub fn relation(a: &[u8], b: &[u8]) -> Relation {
    if a == b {
        Relation::Same
    } else if b.starts_with(a) {
        Relation::ABehind
    } else if a.starts_with(b) {
        Relation::BBehind
    } else {
        Relation::Diverged
    }
}

/// Union of lines: all of `a`, then lines of `b` that `a` lacks (exact line bytes).
/// Session logs are append-only event streams linked by ids, so a union keeps every message
/// from both devices instead of discarding one side like "newest wins".
pub fn merge(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut seen: HashSet<&[u8]> = HashSet::new();
    let mut out = Vec::with_capacity(a.len() + b.len());
    for line in a.split_inclusive(|c| *c == b'\n') {
        seen.insert(trim_ascii(line));
        out.extend_from_slice(line);
    }
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    for line in b.split_inclusive(|c| *c == b'\n') {
        let t = trim_ascii(line);
        if t.is_empty() || !seen.insert(t) {
            continue;
        }
        out.extend_from_slice(line);
        if !line.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_only_changed_lines() {
        let src = b"{\"cwd\":\"C:\\\\u\\\\p\",\"a\":1}\n  not json \n{\"x\":2}\n";
        let out = transform(src, &|s| s.replace("C:\\u\\p", "~/p"));
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("{\"cwd\":\"~/p\",\"a\":1}\n"));
        assert!(s.ends_with("  not json \n{\"x\":2}\n"));
    }

    #[test]
    fn merge_unions_divergent_logs() {
        let a = b"1\n2\n3\n";
        let b = b"1\n2\n4\n";
        assert_eq!(relation(a, b), Relation::Diverged);
        assert_eq!(merge(a, b), b"1\n2\n3\n4\n");
        assert_eq!(relation(b"1\n", b"1\n2\n"), Relation::ABehind);
    }

    #[test]
    fn partial_line_is_dropped() {
        assert_eq!(complete_lines(b"a\nb\nhal"), b"a\nb\n");
        assert_eq!(complete_lines(b"half"), b"");
    }
}
