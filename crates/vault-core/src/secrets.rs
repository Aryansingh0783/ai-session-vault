//! Secret detection/redaction for session logs (API keys, tokens and private keys that showed up in tool output).

use regex::bytes::Regex;
use std::sync::OnceLock;

pub const PLACEHOLDER: &[u8] = b"[REDACTED-by-vault]";

pub fn regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?-u)(sk-[A-Za-z0-9_\-]{20,}|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_\w{30,}|xox[abprs]-[A-Za-z0-9\-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_\-]{35}|eyJ[\w\-]{10,}\.[\w\-]{10,}\.[\w\-]{10,})").unwrap()
    })
}

pub fn count(data: &[u8]) -> usize {
    regex().find_iter(data).count()
}

/// Replace secret-looking strings. The placeholder has no quotes or backslashes, so JSON stays valid.
pub fn redact(data: &[u8]) -> Vec<u8> {
    regex().replace_all(data, PLACEHOLDER).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_keys_but_keeps_json_valid() {
        let line = br#"{"out":"token ghp_abcdefghijklmnopqrstuvwxyz0123456789 done"}"#;
        assert_eq!(count(line), 1);
        let r = redact(line);
        assert!(serde_json::from_slice::<serde_json::Value>(&r).is_ok());
        assert!(!String::from_utf8_lossy(&r).contains("ghp_"));
        assert_eq!(count(&r), 0);
    }
}
