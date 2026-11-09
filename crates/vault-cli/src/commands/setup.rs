//! The vault operations behind the app's first-run setup page.

use crate::support::*;
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use vault_core::crypto::KdfParams;
use vault_core::env::Env;
use vault_core::store::Store;
use vault_ui::{SetupBackend, SetupStatus, Suggestion};

pub(crate) struct CliSetup {
    pub(crate) env: Env,
    pub(crate) folder: Option<PathBuf>,
    pub(crate) reason: Option<String>,
    /// The vault the user just created or opened, handed to the app so it need not be opened twice
    /// (and so it works even when the passphrase was not remembered).
    opened: Mutex<Option<Store>>,
}

/// Folders a cloud drive already syncs, each with a vault subfolder name we would use.
fn cloud_folders(home: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = vec![];
    if let Ok(rd) = std::fs::read_dir(home) {
        let mut names: Vec<String> = rd.flatten().filter(|e| e.path().is_dir()).filter_map(|e| e.file_name().into_string().ok()).collect();
        names.sort();
        for n in names {
            let label = if n.starts_with("OneDrive") {
                n.clone()
            } else if n.starts_with("Dropbox") {
                "Dropbox".to_string()
            } else if n.starts_with("Google Drive") {
                "Google Drive".to_string()
            } else {
                continue;
            };
            out.push((label, home.join(&n)));
        }
    }
    let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
    if icloud.is_dir() {
        out.push(("iCloud Drive".into(), icloud));
    }
    out.push(("Documents".into(), home.join("Documents")));
    out
}

pub(crate) fn suggestions(home: &Path) -> Vec<Suggestion> {
    cloud_folders(home)
        .into_iter()
        .filter(|(_, p)| p.is_dir())
        .map(|(label, base)| {
            let path = base.join("AI Session Vault");
            Suggestion { label, has_vault: Store::exists(&path), path: path.display().to_string() }
        })
        .collect()
}

impl CliSetup {
    pub(crate) fn new(env: Env, folder: Option<PathBuf>, reason: Option<String>) -> CliSetup {
        CliSetup { env, folder, reason, opened: Mutex::new(None) }
    }

    pub(crate) fn take_store(&self) -> Option<Store> {
        self.opened.lock().unwrap().take()
    }
}

impl SetupBackend for CliSetup {
    fn status(&self) -> SetupStatus {
        SetupStatus {
            folder: self.folder.as_ref().map(|p| p.display().to_string()),
            reason: self.reason.clone(),
            suggestions: suggestions(&self.env.home),
            device_name: gethostname_string(),
        }
    }

    fn has_vault(&self, folder: &Path) -> bool {
        Store::exists(folder)
    }

    fn create(&self, folder: &Path, passphrase: &str, remember: bool) -> Result<String> {
        if Store::exists(folder) {
            bail!("There is already a vault in that folder. Go back and choose \"I already have a vault\" to connect to it.");
        }
        std::fs::create_dir_all(folder).map_err(|e| anyhow!("Can't create that folder: {e}"))?;
        let (store, recovery) = Store::init(folder, passphrase, KdfParams::default())?;
        save_vault(&self.env, folder)?;
        if remember {
            remember_passphrase(&store.header.vault_id, passphrase);
        }
        *self.opened.lock().unwrap() = Some(store);
        Ok(recovery)
    }

    fn unlock(&self, folder: &Path, secret: &str, remember: bool) -> Result<()> {
        if !Store::exists(folder) {
            bail!("There is no vault in that folder. If it is still syncing from your cloud drive, wait for it to finish.");
        }
        let store = Store::open(folder, secret).map_err(|_| anyhow!("That passphrase or recovery key doesn't open this vault."))?;
        save_vault(&self.env, folder)?;
        if remember {
            remember_passphrase(&store.header.vault_id, secret);
        }
        *self.opened.lock().unwrap() = Some(store);
        Ok(())
    }

    fn join(&self, folder: &Path, code: &str) -> Result<()> {
        if !Store::exists(folder) {
            bail!("There is no vault in that folder. If it is still syncing from your cloud drive, wait for it to finish.");
        }
        let (store, _) = join_with_code(&self.env, folder, code).map_err(|e| anyhow!("That code didn't work ({e:#}). Make a new one on your other computer."))?;
        *self.opened.lock().unwrap() = Some(store);
        Ok(())
    }
}
