//! Health checks: the things that silently bite people who sync AI assistant state.

use crate::env::{running_tools, Env};
use crate::sync::Component;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use walkdir::WalkDir;

#[derive(Serialize, Debug, PartialEq)]
pub enum Level {
    Ok,
    Warn,
    Info,
}

#[derive(Serialize, Debug)]
pub struct Finding {
    pub level: Level,
    pub title: String,
    pub detail: String,
}

fn f(level: Level, title: &str, detail: impl Into<String>) -> Finding {
    Finding { level, title: title.into(), detail: detail.into() }
}

/// Count secret-looking strings in local session logs (never prints the secrets themselves).
pub fn scan_secrets(env: &Env) -> (usize, usize) {
    let (mut files, mut hits) = (0, 0);
    for root in [env.claude_dir.join("projects"), env.codex_dir.join("sessions")] {
        for e in WalkDir::new(root).into_iter().flatten().filter(|e| e.file_type().is_file()) {
            if e.metadata().map(|m| m.len() > 64 * 1024 * 1024).unwrap_or(true) {
                continue;
            }
            if let Ok(b) = fs::read(e.path()) {
                let n = crate::secrets::count(&b);
                if n > 0 {
                    files += 1;
                    hits += n;
                }
            }
        }
    }
    (files, hits)
}

pub fn run(env: &Env, vault_configured: bool) -> Vec<Finding> {
    let mut out = vec![];
    out.push(if env.claude_dir.is_dir() {
        f(Level::Ok, "Claude Code found", env.claude_dir.display().to_string())
    } else {
        f(Level::Info, "Claude Code not found", "nothing to sync from here")
    });
    // Claude Code deletes transcripts older than cleanupPeriodDays (default 30) at startup.
    let settings = env.claude_dir.join("settings.json");
    let days = fs::read(&settings).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v.get("cleanupPeriodDays").and_then(|d| d.as_u64()));
    match days {
        Some(d) if d >= 90 => out.push(f(Level::Ok, "Session retention", format!("Claude keeps sessions {d} days"))),
        other => out.push(f(
            Level::Warn,
            "Claude Code deletes old sessions automatically",
            format!("cleanupPeriodDays is {}; sessions older than that are purged at startup. Your vault keeps them (deletions are never synced), but run a backup at least that often, or raise the value in {}.", other.map_or("unset (default 30)".into(), |d| d.to_string()), settings.display()),
        )),
    }
    let (files, hits) = scan_secrets(env);
    out.push(if hits == 0 {
        f(Level::Ok, "No secret-looking strings in session logs", "")
    } else {
        f(Level::Warn, "Secrets found in session logs", format!("{hits} secret-looking string(s) in {files} file(s) (API keys / tokens that appeared in tool output). The vault encrypts everything, but rotate keys you pasted into chats and avoid sharing exported transcripts."))
    });
    let run = running_tools();
    if !run.is_empty() {
        out.push(f(Level::Warn, "AI tools are running", format!("{} - close them before restoring so files aren't overwritten", run.join(", "))));
    }
    if cfg!(windows) && env.desktop_cfg.to_string_lossy().contains("Packages") {
        out.push(f(Level::Info, "Claude Desktop (Microsoft Store build)", format!("using its virtualized config: {}", env.desktop_cfg.display())));
    }
    out.push(if vault_configured { f(Level::Ok, "Vault configured", "") } else { f(Level::Info, "No vault yet", "run `avault init --vault <folder>`") });
    let _ = Component::ALL;
    out
}
