//! Typed, validated inputs for the first-run setup page (see `crate::setup`).

use super::{has_control, Invalid, MAX_ID_CHARS, MAX_PATH_CHARS};
use serde::Deserialize;
use std::collections::HashMap;

pub const MAX_SECRET_CHARS: usize = 1024;

fn folder_of(raw: &str) -> Result<std::path::PathBuf, Invalid> {
    let f = raw.trim().trim_matches('"');
    if f.is_empty() {
        return Err(Invalid::new("choose a folder for the vault"));
    }
    if f.chars().count() > MAX_PATH_CHARS || has_control(f) {
        return Err(Invalid::new("that folder path is not valid"));
    }
    Ok(std::path::PathBuf::from(f))
}

fn secret_of<'a>(raw: &'a str, what: &str) -> Result<&'a str, Invalid> {
    if raw.is_empty() {
        return Err(Invalid(format!("enter the {what}")));
    }
    if raw.chars().count() > MAX_SECRET_CHARS || raw.contains('\0') {
        return Err(Invalid(format!("the {what} is not valid")));
    }
    Ok(raw)
}

/// Create a new vault in `folder`.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct CreateReq {
    pub folder: String,
    pub passphrase: String,
    pub remember: bool,
}

impl CreateReq {
    pub fn checked(&self) -> Result<(std::path::PathBuf, &str), Invalid> {
        let folder = folder_of(&self.folder)?;
        let pass = secret_of(&self.passphrase, "passphrase")?;
        if pass.chars().count() < vault_core::crypto::MIN_PASSPHRASE {
            return Err(Invalid(format!("the passphrase needs at least {} characters", vault_core::crypto::MIN_PASSPHRASE)));
        }
        Ok((folder, pass))
    }
}

/// Open an existing vault with its passphrase (or recovery key).
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct UnlockReq {
    pub folder: String,
    pub passphrase: String,
    pub remember: bool,
}

impl UnlockReq {
    pub fn checked(&self) -> Result<(std::path::PathBuf, &str), Invalid> {
        Ok((folder_of(&self.folder)?, secret_of(&self.passphrase, "passphrase or recovery key")?))
    }
}

/// Join an existing vault with a one-time code from another device.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct JoinReq {
    pub folder: String,
    pub code: String,
}

impl JoinReq {
    pub fn checked(&self) -> Result<(std::path::PathBuf, &str), Invalid> {
        let code = self.code.trim();
        if code.is_empty() {
            return Err(Invalid::new("enter the code from your other device"));
        }
        if code.chars().count() > MAX_ID_CHARS || has_control(code) {
            return Err(Invalid::new("that code is not valid"));
        }
        Ok((folder_of(&self.folder)?, code))
    }
}

pub fn folder_param(q: &HashMap<String, String>) -> Result<std::path::PathBuf, Invalid> {
    folder_of(q.get("folder").map(String::as_str).unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::super::parse_body;
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn setup_requests_are_checked() {
        let ok = parse_body::<CreateReq>(r#"{"folder":" \"D:\\Vault\" ","passphrase":"correct horse battery"}"#).unwrap();
        assert_eq!(ok.checked().unwrap().0, std::path::PathBuf::from("D:\\Vault"));
        assert!(parse_body::<CreateReq>(r#"{"folder":"x","passphrase":"short"}"#).unwrap().checked().is_err());
        assert!(parse_body::<CreateReq>(r#"{"folder":"","passphrase":"correct horse battery"}"#).unwrap().checked().is_err());
        assert!(parse_body::<CreateReq>(r#"{"folder":"x","passphrase":"correct horse battery","nope":1}"#).is_err());
        assert!(parse_body::<UnlockReq>(r#"{"folder":"x","passphrase":""}"#).unwrap().checked().is_err());
        assert!(parse_body::<JoinReq>(r#"{"folder":"x","code":"  "}"#).unwrap().checked().is_err());
        assert_eq!(parse_body::<JoinReq>(r#"{"folder":"x","code":" AB-12 "}"#).unwrap().checked().unwrap().1, "AB-12");
        assert!(folder_param(&q(&[("folder", "a\u{0}b")])).is_err());
    }
}
