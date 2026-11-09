//! The sync engine: `backup` (device -> vault), `restore` (vault -> device) and `sync` (restore, then backup).
//!
//! Safety properties:
//! * Deletions are never propagated (Claude's own 30-day cleanup can't erase your vault).
//! * Logs (JSONL sessions/history) are merged by line union; nothing is overwritten.
//! * Other files are newest-wins, but a *conflict* (both sides changed) is reported and the
//!   loser is saved as a local safety copy.
//! * Every write is atomic (temp file + rename). Partial trailing lines of live logs are never shipped.
//! * Project folders are matched by portable path, so `C:\Users\a\app` and `/home/a/app` line up.

mod backup;
mod install;
mod merge;
mod prepare;
mod restore;
mod restore_file;
mod scan;
pub use backup::backup;
use install::*;
pub use install::{lock, RunLock};
use merge::*;
use prepare::*;
pub use restore::restore;
use restore::*;
use restore_file::*;
use scan::*;

use crate::env::{running_tools, wildcard, Env, State};

use crate::jsonl::{self, Relation};

use crate::paths::{encode_project, project_key, to_local, to_portable, PathMap};

use crate::store::{ChunkWriter, Entry, HashWriter, Kind, Store};

use crate::util::{atomic_write, mtime_ms, now_iso, stamp};

use anyhow::{anyhow, Result};

use serde::Serialize;

use serde_json::Value;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use std::io::{BufRead, BufReader, BufWriter, Write};

use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::Arc;

use std::fs;

use std::path::{Component as PathComp, Path, PathBuf};

use walkdir::WalkDir;

pub const CLAUDE_ITEMS: &[&str] = &["CLAUDE.md", "settings.json", "history.jsonl", "keybindings.json", "rules", "commands", "agents", "skills", "output-styles"];

pub const CODEX_ITEMS: &[&str] = &[
    "config.toml",
    "AGENTS.md",
    "prompts",
    "sessions",
    "archived_sessions",
    "session_index.jsonl",
    "state_*.sqlite",
    "thread_history_*.sqlite",
    "memories_*.sqlite",
    "memories",
    "skills",
    ".codex-global-state.json",
];

/// Never synced: logins, machine-local overrides, OS junk, our own temp files.
const EXCLUDE: &[&str] = &[".credentials.json", "auth.json", ".DS_Store", "Thumbs.db", "desktop.ini", "settings.local.json", "_vault_meta.json"];

/// Logs are merged in memory; plain files stream and have no size limit.
const LOG_MEM_CAP: u64 = 2 * 1024 * 1024 * 1024;

const TOL_MS: i64 = 2000;

const MCP: &str = "claude-code/mcp_servers.json";

const SETTINGS: &str = "claude-code/config/settings.json";

const DESKTOP: &str = "claude-desktop/claude_desktop_config.json";

const KEEP_SAFETY_RUNS: usize = 10;

const MAX_SAFETY_BYTES: u64 = 3 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Component {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    /// Imported claude.ai / ChatGPT conversations.
    Archive,
}

impl Component {
    pub const ALL: [Component; 4] = [Component::ClaudeCode, Component::ClaudeDesktop, Component::Codex, Component::Archive];
    pub fn name(self) -> &'static str {
        match self {
            Component::ClaudeCode => "claude-code",
            Component::ClaudeDesktop => "claude-desktop",
            Component::Codex => "codex",
            Component::Archive => "archive",
        }
    }
    pub fn parse(s: &str) -> Result<Component> {
        Self::ALL.into_iter().find(|c| c.name() == s).ok_or_else(|| anyhow!("unknown component '{s}' (use claude-code, claude-desktop, codex, archive)"))
    }
    fn of(logical: &str) -> Option<Component> {
        Self::ALL.into_iter().find(|c| logical.starts_with(&format!("{}/", c.name())))
    }
}

/// Optional callbacks for a UI or daemon: progress reporting and cooperative cancellation.
/// Progress callback: (phase, done, total).
pub type ProgressFn = Arc<dyn Fn(&str, usize, usize) + Send + Sync>;

/// What a streamed upload produced: (content hash, chunk ids, size, mtime_ms).
type Uploaded = (String, Vec<String>, u64, u64);

#[derive(Clone, Default)]
pub struct Hooks {
    pub progress: Option<ProgressFn>,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Hooks {
    pub fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
    }
    pub fn progress(&self, phase: &str, done: usize, total: usize) {
        if let Some(p) = &self.progress {
            p(phase, done, total);
        }
    }
}

#[derive(Clone)]
pub struct Options {
    pub components: Vec<Component>,
    pub maps: Vec<PathMap>,
    pub dry_run: bool,
    /// Replace API keys/tokens in session logs before they leave this device.
    pub redact: bool,
    /// Refuse to upload session logs that contain secret-looking strings (they are listed in the report).
    pub block_secrets: bool,
    pub hooks: Hooks,
    /// When set, only these local files are examined (incremental backup after file-watch events).
    pub only_paths: Option<Arc<HashSet<PathBuf>>>,
}

impl Default for Options {
    fn default() -> Self {
        Options { components: Component::ALL.to_vec(), maps: vec![], dry_run: false, redact: false, block_secrets: false, hooks: Hooks::default(), only_paths: None }
    }
}

#[derive(Default, Debug, Clone, Serialize)]
pub struct Report {
    pub uploaded: usize,
    pub downloaded: usize,
    pub merged: usize,
    pub unchanged: usize,
    pub safety_copies: usize,
    pub conflicts: Vec<String>,
    pub failed: Vec<String>,
    pub busy: Vec<String>,
    pub warnings: Vec<String>,
    pub mcp_added: Vec<String>,
    pub missing_projects: Vec<String>,
    pub skipped_large: Vec<String>,
    pub blocked: Vec<String>,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    pub cancelled: bool,
}

impl Report {
    pub fn absorb(&mut self, o: Report) {
        self.uploaded += o.uploaded;
        self.downloaded += o.downloaded;
        self.merged += o.merged;
        self.unchanged += o.unchanged;
        self.safety_copies += o.safety_copies;
        self.bytes_uploaded += o.bytes_uploaded;
        self.bytes_downloaded += o.bytes_downloaded;
        self.cancelled |= o.cancelled;
        for (a, b) in [
            (&mut self.conflicts, o.conflicts),
            (&mut self.failed, o.failed),
            (&mut self.busy, o.busy),
            (&mut self.warnings, o.warnings),
            (&mut self.mcp_added, o.mcp_added),
            (&mut self.missing_projects, o.missing_projects),
            (&mut self.skipped_large, o.skipped_large),
            (&mut self.blocked, o.blocked),
        ] {
            a.extend(b);
        }
    }
}

pub type Log<'a> = &'a dyn Fn(&str);

// ======================================================================= sync
/// Restore (merge others' changes in) and then backup (publish yours), like `git pull && git push`.
pub fn sync(env: &Env, store: &Store, state: &mut State, opts: &Options, log: Log) -> Result<Report> {
    let _lock = lock(env)?;
    sync_unlocked(env, store, state, opts, log)
}

/// Same as [`sync`] for callers that already hold the [`lock`].
pub fn sync_unlocked(env: &Env, store: &Store, state: &mut State, opts: &Options, log: Log) -> Result<Report> {
    let mut rep = restore(env, store, state, opts, log)?;
    rep.absorb(backup(env, store, state, opts, log)?);
    Ok(rep)
}
