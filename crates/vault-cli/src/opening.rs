//! Finding and opening the vault: the saved folder, stored credentials, and opening without a terminal.

use crate::cli::Cli;
use crate::support::{gethostname_string, prompt_secret};
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use vault_core::env::Env;
use vault_core::store::Store;

pub(crate) fn config_path(env: &Env) -> PathBuf {
    env.state_dir.join("config.json")
}

pub(crate) fn saved_vault(env: &Env) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(config_path(env)).ok()?).ok()?;
    v.get("vault")?.as_str().map(PathBuf::from)
}

pub(crate) fn save_vault(env: &Env, p: &Path) -> Result<()> {
    vault_core::env::set_config(&env.state_dir, "vault", serde_json::json!(p))
}

pub(crate) fn keyring_entry(vault_id: &str) -> Option<keyring::Entry> {
    keyring::Entry::new("ai-session-vault", vault_id).ok()
}

pub(crate) fn device_credential_path(env: &Env, vault_id: &str) -> PathBuf {
    env.state_dir.join(format!("device-{vault_id}.key"))
}

pub(crate) fn load_device_credential(env: &Env, vault_id: &str) -> Option<String> {
    if let Ok(e) = keyring::Entry::new("ai-session-vault-device", vault_id) {
        if let Ok(p) = e.get_password() {
            return Some(p);
        }
    }
    std::fs::read_to_string(device_credential_path(env, vault_id)).ok().map(|s| s.trim().to_string())
}

pub(crate) fn vault_dir(cli: &Cli, env: &Env) -> Result<PathBuf> {
    cli.vault
        .clone()
        .or_else(|| saved_vault(env))
        .ok_or_else(|| anyhow!("no vault yet: run `avault init --vault <folder>` (a folder inside OneDrive/Google Drive/Dropbox, or on a USB stick)"))
}

pub(crate) fn open(cli: &Cli, env: &Env) -> Result<Store> {
    let dir = vault_dir(cli, env)?;
    if !Store::exists(&dir) {
        bail!("no vault in {} - if it is still syncing, wait for it to finish; otherwise run `avault init`", dir.display());
    }
    let vault_id = Store::read_header(&dir)?.vault_id;
    let env_pw_set = std::env::var("AI_VAULT_PASSPHRASE").is_ok_and(|p| !p.is_empty());
    let from_keyring = if env_pw_set { None } else { keyring_entry(&vault_id).and_then(|e| e.get_password().ok()) };
    if !env_pw_set && from_keyring.is_none() {
        if let Some(cred) = load_device_credential(env, &vault_id) {
            match Store::open_with_device(&dir, &cred) {
                Ok(s) => {
                    save_vault(env, &dir)?;
                    return Ok(s);
                }
                Err(e) => eprintln!("(this device's stored access no longer works: {e}; ask for the passphrase)"),
            }
        }
    }
    let store = match from_keyring {
        Some(pw) if !pw.is_empty() => Store::open(&dir, &pw).or_else(|_| {
            eprintln!("(the stored passphrase no longer works; ask for it again)");
            Store::open(&dir, &prompt_secret("Vault passphrase (or recovery key): ")?)
        })?,
        _ => Store::open(&dir, &prompt_secret("Vault passphrase (or recovery key): ")?)?,
    };
    save_vault(env, &dir)?;
    Ok(store)
}

/// The result of trying to open the vault without a terminal (the app has none).
pub(crate) enum Opened {
    Ready(Store),
    /// Setup is needed. `folder` is set when a vault exists there but cannot be opened yet.
    Needs {
        folder: Option<PathBuf>,
        reason: Option<String>,
    },
}

/// Like `open`, but never prompts: every way of opening it without asking is tried, and if none works
/// the caller is told what to ask the user for.
pub(crate) fn open_silent(cli: &Cli, env: &Env) -> Result<Opened> {
    let Some(dir) = cli.vault.clone().or_else(|| saved_vault(env)) else {
        return Ok(Opened::Needs { folder: None, reason: None });
    };
    if !Store::exists(&dir) {
        let reason = format!("There is no vault in {}. If it is still syncing from your cloud drive, wait for it to finish, or choose another folder.", dir.display());
        return Ok(Opened::Needs { folder: None, reason: Some(reason) });
    }
    let vault_id = Store::read_header(&dir)?.vault_id;
    let mut reason = None;
    if let Some(pw) = std::env::var("AI_VAULT_PASSPHRASE").ok().filter(|p| !p.is_empty()) {
        if let Ok(s) = Store::open(&dir, &pw) {
            return Ok(Opened::Ready(s));
        }
    }
    if let Some(cred) = load_device_credential(env, &vault_id) {
        match Store::open_with_device(&dir, &cred) {
            Ok(s) => return Ok(Opened::Ready(s)),
            Err(_) => reason = Some("This computer's saved access to the vault no longer works (it may have been revoked). Enter the passphrase to continue.".to_string()),
        }
    }
    if let Some(pw) = keyring_entry(&vault_id).and_then(|e| e.get_password().ok()).filter(|p| !p.is_empty()) {
        match Store::open(&dir, &pw) {
            Ok(s) => return Ok(Opened::Ready(s)),
            Err(_) => reason = Some("The passphrase saved on this computer no longer opens the vault. Enter the current one.".to_string()),
        }
    }
    Ok(Opened::Needs { folder: Some(dir), reason })
}

/// Store a passphrase in the OS credential store. False if there is none.
pub(crate) fn remember_passphrase(vault_id: &str, secret: &str) -> bool {
    keyring_entry(vault_id).is_some_and(|e| e.set_password(secret).is_ok())
}

/// Join a vault with a one-time code and keep this device's credential. Also says whether the
/// credential went to the OS credential store (otherwise it is a file in the state folder).
pub(crate) fn join_with_code(env: &Env, dir: &Path, code: &str) -> Result<(Store, bool)> {
    let (store, cred) = Store::join(dir, code, &gethostname_string())?;
    let id = store.header.vault_id.clone();
    let in_keyring = keyring::Entry::new("ai-session-vault-device", &id).and_then(|e| e.set_password(&cred)).is_ok();
    if !in_keyring {
        std::fs::create_dir_all(&env.state_dir)?;
        std::fs::write(device_credential_path(env, &id), &cred)?;
    }
    save_vault(env, dir)?;
    Ok((store, in_keyring))
}
