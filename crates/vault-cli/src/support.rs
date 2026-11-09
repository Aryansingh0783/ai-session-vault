//! Shared helpers: saved settings, credentials, prompts, and output.

use crate::cli::{Cli, SyncArgs};
use anyhow::{anyhow, bail, Result};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use vault_core::crypto::MIN_PASSPHRASE;
use vault_core::env::Env;
use vault_core::paths::parse_maps;
use vault_core::store::Store;
use vault_core::sync::{Component, Options, Report};

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

pub(crate) fn prompt_secret(label: &str) -> Result<String> {
    if let Ok(p) = std::env::var("AI_VAULT_PASSPHRASE") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    if !std::io::stdin().is_terminal() {
        bail!("no terminal to ask for the passphrase: set AI_VAULT_PASSPHRASE or run in a terminal");
    }
    Ok(rpassword::prompt_password(label)?)
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

pub(crate) fn options(a: &SyncArgs) -> Result<Options> {
    let components = match &a.only {
        Some(s) => s.split(',').map(|c| Component::parse(c.trim())).collect::<Result<Vec<_>>>()?,
        None => Component::ALL.to_vec(),
    };
    Ok(Options { components, maps: parse_maps(&a.maps)?, dry_run: a.dry_run, redact: a.redact, block_secrets: a.block_secrets, ..Options::default() })
}

pub(crate) fn print_report(r: &Report, json: bool) {
    if json {
        println!("{}", serde_json::to_string(r).unwrap());
        return;
    }
    println!("  -> {} uploaded, {} downloaded, {} merged, {} already up to date", r.uploaded, r.downloaded, r.merged, r.unchanged);
    if r.safety_copies > 0 {
        println!("  {} replaced file(s) were saved first (in ~/.ai-vault/safety-copies)", r.safety_copies);
    }
    let sections: [(&str, &Vec<String>); 8] = [
        ("Conflicts (kept the newer version)", &r.conflicts),
        ("Files that couldn't be written", &r.failed),
        ("In use by an app, skipped (quit it and run again)", &r.busy),
        ("MCP servers added", &r.mcp_added),
        ("Project folders missing on this device (sessions restored anyway; clone them or use --map OLD=NEW)", &r.missing_projects),
        ("Skipped, over 2 GB", &r.skipped_large),
        ("Not uploaded: contain secrets (--block-secrets)", &r.blocked),
        ("Notes", &r.warnings),
    ];
    for (title, items) in sections {
        if !items.is_empty() {
            println!("  ! {title}:");
            for i in items.iter().take(10) {
                println!("      {i}");
            }
            if items.len() > 10 {
                println!("      ... and {} more", items.len() - 10);
            }
        }
    }
}

pub(crate) fn to_clipboard(text: &str) -> bool {
    use std::process::{Command, Stdio};
    let cmds: Vec<(&str, Vec<&str>, Vec<u8>)> = if cfg!(windows) {
        let mut b = vec![0xFF, 0xFE];
        b.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
        vec![("clip", vec![], b)]
    } else if cfg!(target_os = "macos") {
        vec![("pbcopy", vec![], text.as_bytes().to_vec())]
    } else {
        vec![("wl-copy", vec![], text.as_bytes().to_vec()), ("xclip", vec!["-selection", "clipboard"], text.as_bytes().to_vec())]
    };
    for (c, args, data) in cmds {
        if let Ok(mut ch) = Command::new(c).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            if ch.stdin.take().is_some_and(|mut s| s.write_all(&data).is_ok()) && ch.wait().is_ok_and(|s| s.success()) {
                return true;
            }
        }
    }
    false
}

pub(crate) fn new_passphrase() -> Result<String> {
    if let Ok(p) = std::env::var("AI_VAULT_PASSPHRASE") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    println!("Choose a vault passphrase ({MIN_PASSPHRASE}+ characters). Use the same one on every device.");
    loop {
        let a = prompt_secret("New passphrase: ")?;
        if a.chars().count() < MIN_PASSPHRASE {
            println!("Too short.");
            continue;
        }
        if prompt_secret("Repeat: ")? == a {
            return Ok(a);
        }
        println!("Didn't match.");
    }
}

/// Open the UI as an app-style window (Edge/Chrome `--app`) when available, else the default browser.
pub(crate) fn open_app_window(url: &str) {
    use std::process::Command;
    if cfg!(windows) {
        let candidates = [
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        ];
        let mut found: Vec<String> = candidates.iter().map(|c| c.to_string()).collect();
        for exe in ["msedge.exe", "chrome.exe"] {
            for hive in ["HKLM", "HKCU"] {
                let key = format!(r"{hive}\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}");
                if let Ok(out) = Command::new("reg").args(["query", &key, "/ve"]).output() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    if let Some(line) = text.lines().find(|l| l.contains("REG_SZ")) {
                        if let Some(p) = line.split("REG_SZ").nth(1) {
                            found.insert(0, p.trim().to_string());
                        }
                    }
                }
            }
        }
        for c in found {
            if std::path::Path::new(&c).exists() && Command::new(&c).arg(format!("--app={url}")).spawn().is_ok() {
                return;
            }
        }
        let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
    } else if cfg!(target_os = "macos") {
        let _ = Command::new("open").arg(url).spawn();
    } else {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

pub(crate) fn gethostname_string() -> String {
    std::env::var("AI_VAULT_DEVICE_NAME").or_else(|_| std::env::var("COMPUTERNAME")).or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "this device".into())
}
