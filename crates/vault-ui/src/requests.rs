//! Typed, validated API inputs. Every request body and query parameter goes through here before
//! anything touches the vault, so bad input gets a clear 400 instead of a confusing failure later.

use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;

pub const MAX_SEARCH_CHARS: usize = 256;
pub const MAX_PATH_CHARS: usize = 1024;
pub const MAX_ID_CHARS: usize = 256;
pub const MAX_SOURCE_CHARS: usize = 64;

/// A request the caller got wrong. The message is safe to show to the user.
#[derive(Debug, PartialEq, Eq)]
pub struct Invalid(pub String);

impl Invalid {
    fn new(msg: &str) -> Invalid {
        Invalid(msg.to_string())
    }
}

/// Parse a JSON body into `T`. An empty body counts as `{}` (buttons that send nothing).
pub fn parse_body<T: DeserializeOwned>(body: &str) -> Result<T, Invalid> {
    let text = if body.trim().is_empty() { "{}" } else { body };
    serde_json::from_str(text).map_err(|e| {
        if e.is_syntax() || e.is_eof() {
            Invalid::new("the request body is not valid JSON")
        } else {
            Invalid(format!("the request is not valid: {}", short(&e.to_string())))
        }
    })
}

/// Keep serde's message short and free of internals.
fn short(msg: &str) -> String {
    msg.split(" at line ").next().unwrap_or(msg).chars().take(120).collect()
}

fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

// ---------------------------------------------------------------------------------------------
// POST bodies

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct EnrollReq {
    pub minutes: Option<u64>,
}

impl EnrollReq {
    pub fn minutes(&self) -> Result<u64, Invalid> {
        match self.minutes {
            None => Ok(15),
            Some(m) if (1..=120).contains(&m) => Ok(m),
            Some(_) => Err(Invalid::new("minutes must be between 1 and 120")),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct DeviceReq {
    pub id: String,
}

impl DeviceReq {
    /// Device ids are lowercase hex.
    pub fn id(&self) -> Result<&str, Invalid> {
        let id = self.id.trim();
        if id.is_empty() {
            return Err(Invalid::new("id is required"));
        }
        if id.len() > 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Invalid::new("id is not a valid device id"));
        }
        Ok(id)
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ExcludeReq {
    pub path: String,
    pub remove: bool,
}

impl ExcludeReq {
    /// The project path, trimmed and with forward slashes.
    pub fn path(&self) -> Result<String, Invalid> {
        let p = self.path.trim().replace('\\', "/");
        if p.is_empty() {
            return Err(Invalid::new("path is required"));
        }
        if p.chars().count() > MAX_PATH_CHARS {
            return Err(Invalid::new("path is too long"));
        }
        if has_control(&p) {
            return Err(Invalid::new("path contains invalid characters"));
        }
        Ok(p)
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct VerifyReq {
    pub deep: bool,
}

// ---------------------------------------------------------------------------------------------
// GET query strings

pub fn search_text(q: &HashMap<String, String>) -> Result<String, Invalid> {
    let text = q.get("q").map(|s| s.trim()).unwrap_or("");
    if text.chars().count() > MAX_SEARCH_CHARS {
        return Err(Invalid::new("the search is too long"));
    }
    if has_control(text) {
        return Err(Invalid::new("the search contains invalid characters"));
    }
    Ok(text.to_string())
}

/// `source` + `id` of a conversation.
pub fn conversation_ref(q: &HashMap<String, String>) -> Result<(&str, &str), Invalid> {
    let source = q.get("source").map(String::as_str).unwrap_or("");
    let id = q.get("id").map(String::as_str).unwrap_or("");
    if source.is_empty() || id.is_empty() {
        return Err(Invalid::new("source and id are required"));
    }
    if source.chars().count() > MAX_SOURCE_CHARS || !source.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Err(Invalid::new("source is not valid"));
    }
    if id.chars().count() > MAX_ID_CHARS || has_control(id) {
        return Err(Invalid::new("id is not valid"));
    }
    Ok((source, id))
}

pub fn deep_flag(q: &HashMap<String, String>) -> bool {
    q.get("deep").is_some_and(|v| v == "1" || v == "true")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn empty_body_is_an_empty_object_and_bad_json_is_rejected() {
        assert!(parse_body::<EnrollReq>("").is_ok());
        assert!(parse_body::<EnrollReq>("  ").is_ok());
        assert_eq!(parse_body::<EnrollReq>("{nope").err(), Some(Invalid::new("the request body is not valid JSON")));
    }

    #[test]
    fn unknown_fields_and_wrong_types_are_rejected() {
        assert!(parse_body::<EnrollReq>(r#"{"minutes": 5, "extra": 1}"#).is_err());
        assert!(parse_body::<EnrollReq>(r#"{"minutes": "five"}"#).is_err());
        assert!(parse_body::<ExcludeReq>(r#"{"path": 7}"#).is_err());
    }

    #[test]
    fn enrollment_minutes_are_bounded() {
        assert_eq!(parse_body::<EnrollReq>("{}").unwrap().minutes().unwrap(), 15);
        assert_eq!(parse_body::<EnrollReq>(r#"{"minutes":120}"#).unwrap().minutes().unwrap(), 120);
        assert!(parse_body::<EnrollReq>(r#"{"minutes":0}"#).unwrap().minutes().is_err());
        assert!(parse_body::<EnrollReq>(r#"{"minutes":121}"#).unwrap().minutes().is_err());
    }

    #[test]
    fn device_ids_must_be_hex() {
        assert!(parse_body::<DeviceReq>(r#"{"id":"c7800de2"}"#).unwrap().id().is_ok());
        assert!(parse_body::<DeviceReq>(r#"{"id":""}"#).unwrap().id().is_err());
        assert!(parse_body::<DeviceReq>(r#"{"id":"../../etc"}"#).unwrap().id().is_err());
        assert!(parse_body::<DeviceReq>("{}").unwrap().id().is_err());
    }

    #[test]
    fn exclude_paths_are_normalised_and_checked() {
        let r = parse_body::<ExcludeReq>(r#"{"path":"  C:\\work\\app "}"#).unwrap();
        assert_eq!(r.path().unwrap(), "C:/work/app");
        assert!(parse_body::<ExcludeReq>(r#"{"path":"   "}"#).unwrap().path().is_err());
        assert!(parse_body::<ExcludeReq>(r#"{"path":"a\u0000b"}"#).unwrap().path().is_err());
        let long = format!(r#"{{"path":"{}"}}"#, "a".repeat(MAX_PATH_CHARS + 1));
        assert!(parse_body::<ExcludeReq>(&long).unwrap().path().is_err());
    }

    #[test]
    fn query_parameters_are_checked() {
        assert_eq!(search_text(&q(&[("q", " rust ")])).unwrap(), "rust");
        assert!(search_text(&q(&[("q", &"x".repeat(MAX_SEARCH_CHARS + 1))])).is_err());
        assert!(conversation_ref(&q(&[("source", "claude-code"), ("id", "abc")])).is_ok());
        assert!(conversation_ref(&q(&[("source", "../x"), ("id", "abc")])).is_err());
        assert!(conversation_ref(&q(&[("source", "codex")])).is_err());
        assert!(deep_flag(&q(&[("deep", "1")])));
        assert!(!deep_flag(&q(&[])));
    }
}
