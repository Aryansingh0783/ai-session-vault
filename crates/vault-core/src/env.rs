//! Where the tools keep their data on *this* device, plus per-device local sync state.

use crate::store::DeviceInfo;
use crate::util::atomic_write;
use anyhow::Result;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Claude Code settings that run machine-specific commands or point at local paths.
pub const DEFAULT_LOCAL_KEYS: &[&str] = &["apiKeyHelper", "awsAuthRefresh", "awsCredentialExport", "statusLine", "otelHeadersHelper", "forceLoginOrgUUID"];

#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    pub claude_dir: PathBuf,
    pub claude_json: PathBuf,
    pub codex_dir: PathBuf,
    pub desktop_cfg: PathBuf,
    /// Local, never-synced state (device id, last-sync hashes) and safety copies.
    pub state_dir: PathBuf,
    /// Top-level `settings.json` keys that stay on each device (machine-specific commands/paths).
    pub local_keys: Vec<String>,
    /// Projects (portable path prefixes like `~/.claude-mem/observer-sessions`) that are never uploaded.
    pub exclude_projects: Vec<String>,
}

impl Env {
    /// `home_override` is used by tests so nothing real is ever touched.
    pub fn detect(home_override: Option<&Path>) -> Result<Env> {
        let test = home_override.is_some();
        let home = match home_override {
            Some(h) => h.to_path_buf(),
            None => dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot find your home folder"))?,
        };
        let var = |k: &str| if test { None } else { std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from) };
        let (claude_dir, claude_json) = match var("CLAUDE_CONFIG_DIR") {
            Some(c) => (c.clone(), c.join(".claude.json")),
            None => (home.join(".claude"), home.join(".claude.json")),
        };
        let codex_dir = var("CODEX_HOME").unwrap_or_else(|| home.join(".codex"));
        let desktop_cfg = desktop_config_path(&home, test);
        let state_dir = home.join(".ai-vault");
        let local_keys = fs::read(state_dir.join("config.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.get("local_keys")?.as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(String::from)).collect()))
            .unwrap_or_else(|| DEFAULT_LOCAL_KEYS.iter().map(|k| k.to_string()).collect());
        let exclude_projects = read_config(&state_dir)
            .get("exclude_projects")
            .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(String::from)).collect()))
            .unwrap_or_default();
        Ok(Env { state_dir, home, claude_dir, claude_json, codex_dir, desktop_cfg, local_keys, exclude_projects })
    }

    pub fn safety_root(&self) -> PathBuf {
        self.state_dir.join("safety-copies")
    }
}

/// `~/.ai-vault/config.json` as a JSON object (empty when missing or damaged).
pub fn read_config(state_dir: &Path) -> serde_json::Value {
    fs::read(state_dir.join("config.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Set one key in the config file without disturbing the others.
pub fn set_config(state_dir: &Path, key: &str, value: serde_json::Value) -> Result<()> {
    let mut cfg = read_config(state_dir);
    cfg[key] = value;
    atomic_write(&state_dir.join("config.json"), serde_json::to_string_pretty(&cfg)?.as_bytes())
}

fn desktop_config_path(home: &Path, test: bool) -> PathBuf {
    if cfg!(target_os = "macos") {
        return home.join("Library/Application Support/Claude/claude_desktop_config.json");
    }
    if cfg!(windows) {
        let appdata = if test { None } else { std::env::var_os("APPDATA").map(PathBuf::from) };
        let classic = appdata.unwrap_or_else(|| home.join("AppData/Roaming")).join("Claude/claude_desktop_config.json");
        // The Microsoft Store (MSIX) build reads a virtualized copy; edits to the classic path are ignored.
        let local = if test { None } else { std::env::var_os("LOCALAPPDATA").map(PathBuf::from) };
        let pkgs = local.unwrap_or_else(|| home.join("AppData/Local")).join("Packages");
        if let Ok(rd) = fs::read_dir(&pkgs) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_lowercase();
                if n.starts_with("claude_") || n.contains(".claude_") {
                    let p = e.path().join("LocalCache/Roaming/Claude/claude_desktop_config.json");
                    if p.is_file() {
                        return p;
                    }
                }
            }
        }
        return classic;
    }
    home.join(".config/Claude/claude_desktop_config.json")
}

/// Does `name` match a pattern with `*` wildcards (case-sensitive)?
pub fn wildcard(pat: &str, name: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == name;
    }
    let mut rest = name;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !rest.starts_with(part) {
                return false;
            }
            rest = &rest[part.len()..];
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else if let Some(pos) = rest.find(part) {
            rest = &rest[pos + part.len()..];
        } else {
            return false;
        }
    }
    true
}

// ------------------------------------------------------------------ local state
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct FileState {
    pub size: u64,
    pub mtime_ms: i64,
    /// Hash of this file's portable content as last computed.
    pub local_hash: String,
    /// For plain files: the vault version this device last uploaded or installed.
    pub synced_hash: Option<String>,
    /// For logs: vault versions already merged into this file.
    #[serde(default)]
    pub seen: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct State {
    pub device_id: String,
    #[serde(default)]
    pub files: BTreeMap<String, FileState>,
    /// Memo of each Claude Code project dir's detected working directory (avoids re-reading logs).
    #[serde(default)]
    pub project_cwd: BTreeMap<String, String>,
}

impl State {
    pub fn load(env: &Env) -> State {
        let p = env.state_dir.join("state.json");
        let mut s: State = fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if s.device_id.is_empty() {
            let mut b = [0u8; 8];
            OsRng.fill_bytes(&mut b);
            s.device_id = hex::encode(b);
        }
        s
    }

    pub fn save(&self, env: &Env) -> Result<()> {
        atomic_write(&env.state_dir.join("state.json"), &serde_json::to_vec(self)?)
    }

    pub fn device_info(&self, env: &Env) -> DeviceInfo {
        DeviceInfo {
            id: self.device_id.clone(),
            name: gethostname::gethostname().to_string_lossy().into_owned(),
            os: std::env::consts::OS.into(),
            home: env.home.to_string_lossy().into_owned(),
        }
    }
}

/// Names of the AI tools currently running (restoring under a running tool can be overwritten).
pub fn running_tools() -> Vec<String> {
    let out = if cfg!(windows) {
        std::process::Command::new("tasklist").args(["/FO", "CSV", "/NH"]).output()
    } else {
        std::process::Command::new("ps").args(["-A", "-o", "comm="]).output()
    };
    let Ok(out) = out else { return vec![] };
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    let mut found = vec![];
    for (needle, label) in [("claude", "Claude"), ("codex", "Codex")] {
        let hit = text.lines().any(|l| {
            let name = l.trim().trim_start_matches('"').split(['"', ',']).next().unwrap_or("");
            let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
            name == needle || name == format!("{needle}.exe") || name.starts_with(&format!("{needle} "))
        });
        if hit {
            found.push(label.to_string());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard("state_*.sqlite", "state_5.sqlite"));
        assert!(!wildcard("state_*.sqlite", "state_5.sqlite-wal"));
        assert!(wildcard("CLAUDE.md", "CLAUDE.md"));
        assert!(!wildcard("CLAUDE.md", "claude.md"));
    }
}
