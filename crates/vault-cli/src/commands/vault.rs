//! CLI commands: the vault itself (create, devices, keys, integrity).

use super::Ctx;
use crate::support::*;
use anyhow::{anyhow, bail, Result};
use vault_core::crypto::KdfParams;
use vault_core::store::Store;

pub(crate) fn init(cx: &Ctx) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let dir = cli.vault.clone().ok_or_else(|| anyhow!("pass --vault <folder>"))?;
    if Store::exists(&dir) {
        bail!("{} already contains a vault. On your other devices just run `avault sync --vault {}`.", dir.display(), dir.display());
    }
    let pass = new_passphrase()?;
    let (_, recovery) = Store::init(&dir, &pass, KdfParams::default())?;
    save_vault(env, &dir)?;
    if cli.json {
        println!("{}", serde_json::json!({ "vault": dir, "recovery_key": recovery }));
    } else {
        println!("\nVault created in {}\n\nRECOVERY KEY (shown once - store it somewhere safe, e.g. a password manager):\n\n    {recovery}\n\nIt unlocks the vault if you forget the passphrase.", dir.display());
    }
    Ok(())
}

pub(crate) fn status(cx: &Ctx) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let store = open(cli, env)?;
    let (ms, warns) = store.read_all_manifests()?;
    if cli.json {
        println!(
            "{}",
            serde_json::json!({ "vault": store.root, "devices": ms.iter().map(|m| serde_json::json!({"name": m.device.name, "os": m.device.os, "updated": m.updated, "files": m.entries.len()})).collect::<Vec<_>>(), "warnings": warns })
        );
        return Ok(());
    }
    println!("Vault: {}", store.root.display());
    for m in &ms {
        let sessions = m.entries.keys().filter(|k| k.starts_with("claude-code/projects/") && k.ends_with(".jsonl")).count();
        println!("  {} ({}): {} files, {} Claude Code session logs, last sync {}", m.device.name, m.device.os, m.entries.len(), sessions, m.updated);
    }
    for w in warns {
        println!("  ! {w}");
    }
    Ok(())
}

pub(crate) fn passwd(cx: &Ctx) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let mut store = open(cli, env)?;
    let new = new_passphrase()?;
    let keys = &store.keys;
    let mut h = store.header.clone();
    h.change_passphrase(keys, &new)?;
    store.header = h;
    store.save_header()?;
    println!("Passphrase changed. Other devices use the new one the next time they sync.");
    Ok(())
}

pub(crate) fn recovery(cx: &Ctx) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let mut store = open(cli, env)?;
    let mut h = store.header.clone();
    let rec = h.rotate_recovery(&store.keys);
    store.header = h;
    store.save_header()?;
    println!("New RECOVERY KEY (the old one no longer works):\n\n    {rec}");
    Ok(())
}

pub(crate) fn overview(cx: &Ctx) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let store = open(cli, env)?;
    let o = vault_core::overview::overview(env, &store, &[])?;
    if cli.json {
        println!("{}", serde_json::to_string(&o)?);
    } else {
        println!(
            "Vault {} ({} devices, {} projects, {} sessions, {:.1} MB)",
            o.vault_path,
            o.totals.devices,
            o.totals.projects,
            o.totals.sessions,
            o.totals.bytes as f64 / 1_048_576.0
        );
        for p in o.projects.iter().take(25) {
            println!(
                "  {:<48} {:>4} sessions  {}",
                p.portable_path.chars().take(48).collect::<String>(),
                p.sessions,
                if p.exists_locally { "" } else { "(folder not on this device)" }
            );
        }
    }
    Ok(())
}

pub(crate) fn verify(cx: &Ctx, deep: &bool) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let store = open(cli, env)?;
    let (ms, warns) = store.read_all_manifests()?;
    let r = store.verify(&ms, *deep);
    if cli.json {
        println!("{}", serde_json::to_string(&r)?);
    } else {
        for w in &warns {
            println!("  ! {w}");
        }
        println!("Checked {} object(s){}.", r.objects, if *deep { " (decrypted and verified)" } else { " (presence only; use --deep to decrypt)" });
        for m in r.missing.iter().take(20) {
            println!("  MISSING {m}");
        }
        for c in r.corrupt.iter().take(20) {
            println!("  CORRUPT {c}");
        }
        if r.missing.is_empty() && r.corrupt.is_empty() && warns.is_empty() {
            println!("Vault is healthy.");
        } else if !r.missing.is_empty() {
            println!(
                "Missing files usually mean the cloud folder is still syncing - wait, then run verify again. If they stay missing, run `avault sync` on the device that has them."
            );
        }
    }
    if !r.missing.is_empty() || !r.corrupt.is_empty() {
        std::process::exit(3);
    }
    Ok(())
}

pub(crate) fn gc(cx: &Ctx) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let store = open(cli, env)?;
    let (ms, warns) = store.read_all_manifests()?;
    if !warns.is_empty() {
        bail!("not collecting: some device manifests are unreadable ({}). Let the folder finish syncing first.", warns.join("; "));
    }
    let (n, bytes) = store.gc(&ms)?;
    println!("Removed {n} unreferenced object(s), freed {:.1} MB.", bytes as f64 / 1_048_576.0);
    Ok(())
}
