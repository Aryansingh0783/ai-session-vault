//! CLI commands: who can open the vault (enrollment codes, joined devices, remembered passphrase).

use super::Ctx;
use crate::support::*;
use anyhow::{anyhow, bail, Result};
use vault_core::env::State;
use vault_core::store::Store;

pub(crate) fn enroll(cx: &Ctx, minutes: &u64) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let store = open(cli, env)?;
    let code = store.create_enrollment(minutes * 60)?;
    if cli.json {
        println!("{}", serde_json::json!({ "code": code, "expires_in_minutes": minutes }));
    } else {
        println!("Enrollment code (valid {minutes} minutes, works once):\n\n    {code}\n\nOn the other device: avault join --vault \"{}\" --code {code}", store.root.display());
    }
    Ok(())
}

pub(crate) fn join(cx: &Ctx, code: &str) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let dir = vault_dir(cli, env)?;
    if !Store::exists(&dir) {
        bail!("no vault in {} - let the folder finish syncing first", dir.display());
    }
    let (_, stored_in_keyring) = join_with_code(env, &dir, code)?;
    println!(
        "Joined. This device can now open the vault without the passphrase{}.",
        if stored_in_keyring { " (credential in the OS credential store)" } else { " (credential in ~/.ai-vault; protect that folder)" }
    );
    println!("Run `avault sync` to bring your sessions over.");
    Ok(())
}

pub(crate) fn remember(cx: &Ctx) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let store = open(cli, env)?;
    let pw = prompt_secret("Passphrase to remember (re-enter; not the recovery key): ")?;
    Store::open(&store.root, &pw)?;
    keyring_entry(&store.header.vault_id)
        .ok_or_else(|| anyhow!("no OS credential store available"))?
        .set_password(&pw)
        .map_err(|e| anyhow!("could not store the passphrase: {e}"))?;
    println!("Passphrase stored in your OS credential store for this vault. `avault forget` removes it.");
    Ok(())
}

pub(crate) fn forget(cx: &Ctx) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let dir = vault_dir(cli, env)?;
    let id = Store::read_header(&dir)?.vault_id;
    if let Some(e) = keyring_entry(&id) {
        let _ = e.delete_credential();
    }
    println!("Stored passphrase removed.");
    Ok(())
}

pub(crate) fn devices(cx: &Ctx, remove: &Option<String>, revoke: &Option<String>) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let store = open(cli, env)?;
    if let Some(id) = revoke {
        let slots = store.device_slots();
        let hits: Vec<_> = slots.iter().filter(|s| s.id.starts_with(id.as_str())).collect();
        match hits.as_slice() {
            [s] => {
                store.revoke_device_slot(&s.id)?;
                println!("Revoked access for '{}'.", s.name);
            }
            [] => bail!("no joined device with id starting '{id}'"),
            _ => bail!("several match; use more characters"),
        }
        return Ok(());
    }
    if let Some(prefix) = remove {
        let (ms, _) = store.read_all_manifests()?;
        let hits: Vec<_> = ms.iter().filter(|m| m.device.id.starts_with(prefix.as_str())).collect();
        match hits.as_slice() {
            [m] => {
                store.remove_device(&m.device.id)?;
                println!("Forgot {} ({}). Run `avault gc` to free space.", m.device.name, m.device.id);
            }
            [] => bail!("no device id starts with '{prefix}'"),
            _ => bail!("several devices match; use more characters"),
        }
        return Ok(());
    }
    let me = State::load(env).device_id;
    let (ms, _) = store.read_all_manifests()?;
    if cli.json {
        println!("{}", serde_json::to_string(&ms.iter().map(|m| serde_json::json!({"id": m.device.id, "name": m.device.name, "os": m.device.os, "updated": m.updated, "files": m.entries.len(), "this_device": m.device.id == me})).collect::<Vec<_>>())?);
    }
    let slots = store.device_slots();
    if !cli.json && !slots.is_empty() {
        println!("Joined devices (passphrase-free access):");
        for sl in &slots {
            println!("  {}  {}  joined {}", sl.id, sl.name, sl.created);
        }
        println!();
    }
    for m in ms.iter().filter(|_| !cli.json) {
        println!(
            "{}  {} ({}){}  last sync {}  {} files",
            m.device.id,
            m.device.name,
            m.device.os,
            if m.device.id == me { "  <- this device" } else { "" },
            m.updated,
            m.entries.len()
        );
    }
    Ok(())
}
