//! The sync engine: `backup` (device -> vault), `restore` (vault -> device) and `sync` (restore, then backup).
//!
//! Safety properties:
//! * Deletions are never propagated (Claude's own 30-day cleanup can't erase your vault).
//! * Logs (JSONL sessions/history) are merged by line union; nothing is overwritten.
//! * Other files are newest-wins, but a *conflict* (both sides changed) is reported and the
//!   loser is saved as a local safety copy.
//! * Every write is atomic (temp file + rename). Partial trailing lines of live logs are never shipped.
//! * Project folders are matched by portable path, so `C:\Users\a\app` and `/home/a/app` line up.

use crate::env::{running_tools, wildcard, Env, FileState, State};
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

pub const CLAUDE_ITEMS: &[&str] = &[
    "CLAUDE.md", "settings.json", "history.jsonl", "keybindings.json",
    "rules", "commands", "agents", "skills", "output-styles",
];
pub const CODEX_ITEMS: &[&str] = &[
    "config.toml", "AGENTS.md", "prompts", "sessions", "archived_sessions",
    "session_index.jsonl", "state_*.sqlite", "thread_history_*.sqlite", "memories_*.sqlite",
    "memories", "skills", ".codex-global-state.json",
];
/// Never synced: logins, machine-local overrides, OS junk, our own temp files.
const EXCLUDE: &[&str] = &[
    ".credentials.json", "auth.json", ".DS_Store", "Thumbs.db", "desktop.ini",
    "settings.local.json", "_vault_meta.json",
];
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
#[derive(Clone, Default)]
pub struct Hooks {
    pub progress: Option<Arc<dyn Fn(&str, usize, usize) + Send + Sync>>,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Hooks {
    pub fn cancelled(&self) -> bool {
        self.cancel.as_ref().map_or(false, |c| c.load(Ordering::Relaxed))
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
            (&mut self.conflicts, o.conflicts), (&mut self.failed, o.failed), (&mut self.busy, o.busy),
            (&mut self.warnings, o.warnings), (&mut self.mcp_added, o.mcp_added),
            (&mut self.missing_projects, o.missing_projects), (&mut self.skipped_large, o.skipped_large),
            (&mut self.blocked, o.blocked),
        ] {
            a.extend(b);
        }
    }
}

pub type Log<'a> = &'a dyn Fn(&str);

struct Item {
    logical: String,
    local: PathBuf,
    kind: Kind,
}

fn kind_of(p: &Path) -> Kind {
    if p.extension().map_or(false, |e| e == "jsonl") { Kind::Log } else { Kind::File }
}

fn rel_posix(base: &Path, p: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !(e.file_type().is_dir() && matches!(e.file_name().to_str(), Some(".git") | Some("node_modules"))))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let n = e.file_name().to_string_lossy();
            !EXCLUDE.contains(&n.as_ref()) && !n.ends_with(".vaulttmp")
        })
        .map(|e| e.into_path())
        .collect()
}

/// Expand item patterns (`name`, `dir`, `prefix_*.ext`) under `base` into files.
fn expand_items(base: &Path, names: &[&str]) -> Vec<PathBuf> {
    let mut out = vec![];
    for n in names {
        if n.contains('*') {
            if let Ok(rd) = fs::read_dir(base) {
                let mut hits: Vec<_> = rd.flatten().filter(|e| wildcard(n, &e.file_name().to_string_lossy())).map(|e| e.path()).collect();
                hits.sort();
                for h in hits {
                    if h.is_file() { out.push(h) } else if h.is_dir() { out.extend(walk_files(&h)) }
                }
            }
            continue;
        }
        let p = base.join(n);
        if p.is_file() {
            if !EXCLUDE.contains(n) {
                out.push(p)
            }
        } else if p.is_dir() {
            out.extend(walk_files(&p));
        }
    }
    out
}

/// Find a Claude Code project's working directory from its logs (prefer the one matching the dir name).
fn detect_cwd(pdir: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let mut files: Vec<_> = fs::read_dir(pdir).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |e| e == "jsonl")).collect();
    files.sort_by_key(|p| std::cmp::Reverse(fs::metadata(p).map(|m| mtime_ms(&m)).unwrap_or(0)));
    let name = pdir.file_name()?.to_string_lossy().into_owned();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for f in files.into_iter().take(25) {
        let Ok(fh) = fs::File::open(&f) else { continue };
        for line in BufReader::new(fh).split(b'\n').take(300).flatten() {
            if !line.windows(5).any(|w| w == b"\"cwd\"") {
                continue;
            }
            if let Ok(Value::Object(o)) = serde_json::from_slice::<Value>(&line) {
                if let Some(Value::String(c)) = o.get("cwd") {
                    if encode_project(c) == name {
                        return Some(c.clone());
                    }
                    *seen.entry(c.clone()).or_default() += 1;
                }
            }
        }
    }
    seen.into_iter().max_by_key(|(_, n)| *n).map(|(c, _)| c)
}

/// Reject vault-supplied relative paths that could escape their folder.
fn safe_rel(rel: &str) -> Option<PathBuf> {
    let p = PathBuf::from(rel);
    if rel.is_empty() || rel.contains(':') || rel.starts_with('/') || rel.starts_with('\\') {
        return None;
    }
    if p.components().any(|c| !matches!(c, PathComp::Normal(_))) || rel.split(['/', '\\']).any(|s| s == "..") {
        return None;
    }
    Some(p)
}

fn scan_local(env: &Env, comps: &[Component], state: &mut State) -> (Vec<Item>, BTreeMap<String, String>) {
    let mut items = vec![];
    let mut projects = BTreeMap::new();
    let mut push = |logical: String, local: PathBuf| {
        let kind = kind_of(&local);
        items.push(Item { logical, local, kind });
    };
    if comps.contains(&Component::ClaudeCode) && env.claude_dir.is_dir() {
        for f in expand_items(&env.claude_dir, CLAUDE_ITEMS) {
            push(format!("claude-code/config/{}", rel_posix(&env.claude_dir, &f)), f);
        }
        let proot = env.claude_dir.join("projects");
        if let Ok(rd) = fs::read_dir(&proot) {
            let mut dirs: Vec<_> = rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.path()).collect();
            dirs.sort();
            for pdir in dirs {
                let name = pdir.file_name().unwrap().to_string_lossy().into_owned();
                let cached = state.project_cwd.get(&name).cloned();
                let cwd = cached.or_else(|| {
                    let d = detect_cwd(&pdir);
                    if let Some(c) = &d {
                        state.project_cwd.insert(name.clone(), c.clone());
                    }
                    d
                });
                let (key, value) = match cwd {
                    Some(cwd) => {
                        let portable = to_portable(&cwd, &env.home);
                        (project_key(&portable), portable)
                    }
                    None => (format!("raw-{name}"), format!("raw:{name}")),
                };
                if env.exclude_projects.iter().any(|x| crate::paths::prefix_match(&value, x).is_some()) {
                    continue;
                }
                projects.insert(key.clone(), value);
                for f in walk_files(&pdir) {
                    push(format!("claude-code/projects/{key}/{}", rel_posix(&pdir, &f)), f);
                }
            }
        }
        if env.claude_json.is_file() {
            push(MCP.into(), env.claude_json.clone());
        }
    }
    if comps.contains(&Component::ClaudeDesktop) && env.desktop_cfg.is_file() {
        push(DESKTOP.into(), env.desktop_cfg.clone());
    }
    if comps.contains(&Component::Codex) && env.codex_dir.is_dir() {
        for f in expand_items(&env.codex_dir, CODEX_ITEMS) {
            push(format!("codex/{}", rel_posix(&env.codex_dir, &f)), f);
        }
    }
    if comps.contains(&Component::Archive) {
        let ad = env.state_dir.join("archive");
        for f in walk_files(&ad) {
            push(format!("archive/{}", rel_posix(&ad, &f)), f);
        }
    }
    (items, projects)
}

fn local_path_for(env: &Env, logical: &str, projects: &BTreeMap<String, String>, maps: &[PathMap], missing: &mut BTreeSet<String>) -> Option<PathBuf> {
    if logical == MCP {
        return Some(env.claude_json.clone());
    }
    if logical == DESKTOP {
        return Some(env.desktop_cfg.clone());
    }
    if let Some(rel) = logical.strip_prefix("claude-code/config/") {
        return Some(env.claude_dir.join(safe_rel(rel)?));
    }
    if let Some(rest) = logical.strip_prefix("claude-code/projects/") {
        let (key, rel) = rest.split_once('/')?;
        let value = projects.get(key)?;
        let enc = if let Some(raw) = value.strip_prefix("raw:") {
            encode_project(raw)
        } else {
            let local = to_local(value, &env.home, maps);
            if !Path::new(&local).is_dir() {
                missing.insert(local.clone());
            }
            encode_project(&local)
        };
        return Some(env.claude_dir.join("projects").join(enc).join(safe_rel(rel)?));
    }
    if let Some(rel) = logical.strip_prefix("archive/") {
        return Some(env.state_dir.join("archive").join(safe_rel(rel)?));
    }
    if let Some(rel) = logical.strip_prefix("codex/") {
        return Some(env.codex_dir.join(safe_rel(rel)?));
    }
    None
}

fn sqlite_busy(p: &Path) -> bool {
    if !p.extension().map_or(false, |e| e == "sqlite") {
        return false;
    }
    let mut w = p.as_os_str().to_owned();
    w.push("-wal");
    fs::metadata(PathBuf::from(w)).map(|m| m.len() > 0).unwrap_or(false)
}

/// Serialize just the `mcpServers` of `.claude.json` (the rest is account state, never synced).
fn mcp_extract(raw: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw)).ok()?;
    let m = v.get("mcpServers")?.as_object()?;
    if m.is_empty() {
        return None;
    }
    serde_json::to_vec_pretty(&Value::Object(m.clone())).ok()
}

/// Stream a local file's *portable* form (what goes into the vault) into `out`.
/// Returns false when there is nothing to store. Live logs: a trailing partial line is never shipped.
fn prepare_to(item: &Item, env: &Env, redact: bool, out: &mut dyn Write) -> Result<bool> {
    if item.logical == MCP {
        return match mcp_extract(&fs::read(&item.local)?) {
            Some(b) => {
                out.write_all(&b)?;
                Ok(true)
            }
            None => Ok(false),
        };
    }
    if item.logical == SETTINGS {
        let raw = fs::read(&item.local)?;
        out.write_all(&filter_settings(&raw, &env.local_keys))?;
        return Ok(true);
    }
    if crate::sqlite::is_sqlite(&item.local) {
        // Live database: take a consistent snapshot with SQLite's backup API, then stream that.
        let tmp = env.state_dir.join("tmp").join(format!("snap-{}.sqlite", std::process::id()));
        if let Some(d) = tmp.parent() {
            fs::create_dir_all(d)?;
        }
        let r = crate::sqlite::snapshot(&item.local, &tmp)
            .and_then(|_| Ok(std::io::copy(&mut fs::File::open(&tmp)?, out).map(|_| ())?));
        let _ = fs::remove_file(&tmp);
        r?;
        return Ok(true);
    }
    let f = fs::File::open(&item.local)?;
    if item.kind == Kind::Log {
        let mut r = BufReader::with_capacity(256 * 1024, f);
        let mut line = Vec::new();
        loop {
            line.clear();
            if r.read_until(b'\n', &mut line)? == 0 || !line.ends_with(b"\n") {
                break;
            }
            let t = jsonl::transform(&line, &|p| to_portable(p, &env.home));
            out.write_all(&if redact { crate::secrets::redact(&t) } else { t })?;
        }
    } else {
        std::io::copy(&mut BufReader::with_capacity(256 * 1024, f), out)?;
    }
    Ok(true)
}

fn prepare_mem(item: &Item, env: &Env, redact: bool) -> Result<Option<Vec<u8>>> {
    let mut v = Vec::new();
    Ok(prepare_to(item, env, redact, &mut v)?.then_some(v))
}

/// `settings.json` minus the machine-specific keys, in a canonical form. Unparseable files pass through.
fn filter_settings(raw: &[u8], local_keys: &[String]) -> Vec<u8> {
    let parsed = serde_json::from_slice::<Value>(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw));
    match parsed {
        Ok(Value::Object(mut o)) => {
            for k in local_keys {
                o.shift_remove(k);
            }
            let mut out = serde_json::to_vec_pretty(&Value::Object(o)).unwrap_or_else(|_| raw.to_vec());
            out.push(b'\n');
            out
        }
        _ => raw.to_vec(),
    }
}

/// Install shared settings but keep this device's own values for the machine-specific keys.
fn install_settings(env: &Env, store: &Store, e: &Entry, path: &Path, safety: &mut SafetyCopies) -> Result<()> {
    let remote = store.read_all(e)?;
    let mut merged = match serde_json::from_slice::<Value>(&remote) {
        Ok(Value::Object(o)) => o,
        _ => return install_stream(store, e, path, safety),
    };
    if let Ok(local_raw) = fs::read(path) {
        if let Ok(Value::Object(local)) = serde_json::from_slice::<Value>(local_raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&local_raw)) {
            for k in &env.local_keys {
                if let Some(v) = local.get(k) {
                    merged.insert(k.clone(), v.clone());
                }
            }
        }
    }
    let mut out = serde_json::to_vec_pretty(&Value::Object(merged))?;
    out.push(b'\n');
    safety.save(path)?;
    atomic_write(path, &out)?;
    set_mtime(path, e.mtime_ms);
    Ok(())
}

/// `settings.json` is merged key by key instead of replaced as a whole file:
/// keys only one side has are kept, machine-specific keys never leave their device, and when both
/// sides set a key differently the newer file's value wins (and the difference is reported).
#[allow(clippy::too_many_arguments)]
fn restore_settings(env: &Env, store: &Store, state: &mut State, item: &Item, winner: &Entry, local: Option<&(String, i64)>, opts: &Options, safety: &mut SafetyCopies, rep: &mut Report) -> Result<()> {
    let Ok(Value::Object(remote)) = serde_json::from_slice::<Value>(&store.read_all(winner)?) else {
        return Ok(());
    };
    let local_obj = fs::read(&item.local).ok().and_then(|b| match serde_json::from_slice::<Value>(b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&b)) {
        Ok(Value::Object(o)) => Some(o),
        _ => None,
    });
    let lmt = local.map(|l| l.1).unwrap_or(0);
    let remote_newer = winner.mtime_ms > lmt + TOL_MS;
    let mut merged = local_obj.clone().unwrap_or_default();
    let mut differing = vec![];
    for (k, rv) in &remote {
        if env.local_keys.contains(k) {
            continue;
        }
        match merged.get(k) {
            None => {
                merged.insert(k.clone(), rv.clone());
            }
            Some(lv) if lv == rv => {}
            Some(_) => {
                differing.push(k.clone());
                if remote_newer {
                    merged.insert(k.clone(), rv.clone());
                }
            }
        }
    }
    if local_obj.is_some() && !differing.is_empty() {
        rep.conflicts.push(format!("settings.json: {} set differently on two devices; kept the newer value (replaced values are in the safety copy)", differing.join(", ")));
    }
    if local_obj.as_ref() == Some(&merged) {
        rep.unchanged += 1;
        return Ok(());
    }
    rep.downloaded += 1;
    if opts.dry_run {
        return Ok(());
    }
    let mut out = serde_json::to_vec_pretty(&Value::Object(merged))?;
    out.push(b'\n');
    safety.save(&item.local)?;
    atomic_write(&item.local, &out)?;
    let md = fs::metadata(&item.local)?;
    let f = state.files.entry(item.logical.clone()).or_default();
    f.size = md.len();
    f.mtime_ms = mtime_ms(&md);
    f.local_hash = store.keys.object_id(&filter_settings(&out, &env.local_keys));
    Ok(())
}

fn file_has_secrets(path: &Path) -> bool {
    let Ok(f) = fs::File::open(path) else { return false };
    let mut r = BufReader::with_capacity(256 * 1024, f);
    let mut line = Vec::new();
    while r.read_until(b'\n', &mut line).map_or(false, |n| n > 0) {
        if crate::secrets::count(&line) > 0 {
            return true;
        }
        line.clear();
    }
    false
}

/// Write an entry's content to `path` through a temp file, verifying it before it replaces anything.
fn install_stream(store: &Store, e: &Entry, path: &Path, safety: &mut SafetyCopies) -> Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".vaulttmp");
    let tmp = PathBuf::from(tmp);
    let r = (|| -> Result<()> {
        let mut w = BufWriter::new(fs::File::create(&tmp)?);
        store.read_to(e, &mut w)?;
        w.flush()?;
        Ok(())
    })();
    if let Err(err) = r {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    safety.save(path)?;
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    set_mtime(path, e.mtime_ms);
    Ok(())
}

fn set_mtime(p: &Path, ms: i64) {
    let ft = filetime::FileTime::from_unix_time(ms.div_euclid(1000), (ms.rem_euclid(1000) as u32) * 1_000_000);
    let _ = filetime::set_file_mtime(p, ft);
}

struct SafetyCopies<'a> {
    env: &'a Env,
    stamp: String,
    count: usize,
}

impl SafetyCopies<'_> {
    fn save(&mut self, p: &Path) -> Result<()> {
        if !p.is_file() {
            return Ok(());
        }
        let rel = p.strip_prefix(&self.env.home).map(|r| r.to_path_buf()).unwrap_or_else(|_| {
            PathBuf::from(p.to_string_lossy().replace([':', '\\', '/'], "_"))
        });
        let dst = self.env.safety_root().join(&self.stamp).join(rel);
        if let Some(d) = dst.parent() {
            fs::create_dir_all(d)?;
        }
        fs::copy(p, dst)?;
        self.count += 1;
        Ok(())
    }
}

fn dir_size(d: &Path) -> u64 {
    WalkDir::new(d).into_iter().flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()
}

fn prune_safety(env: &Env) {
    let root = env.safety_root();
    let Ok(rd) = fs::read_dir(&root) else { return };
    let mut runs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    runs.sort();
    if runs.len() > KEEP_SAFETY_RUNS {
        for d in runs.drain(..runs.len() - KEEP_SAFETY_RUNS) {
            let _ = fs::remove_dir_all(d);
        }
    }
    let mut total = 0u64;
    for (i, d) in runs.iter().rev().enumerate() {
        total += dir_size(d);
        if i > 0 && total > MAX_SAFETY_BYTES {
            let _ = fs::remove_dir_all(d);
        }
    }
}

/// Take an exclusive per-device lock so two runs (e.g. a scheduled one and a manual one) don't collide.
pub struct RunLock(#[allow(dead_code)] fs::File);

pub fn lock(env: &Env) -> Result<RunLock> {
    fs::create_dir_all(&env.state_dir)?;
    let f = fs::OpenOptions::new().create(true).write(true).truncate(false).open(env.state_dir.join("lock"))?;
    f.try_lock().map_err(|_| anyhow!("another AI Session Vault run is already in progress on this device"))?;
    Ok(RunLock(f))
}

// ===================================================================== backup
pub fn backup(env: &Env, store: &Store, state: &mut State, opts: &Options, log: Log) -> Result<Report> {
    let mut rep = Report::default();
    let (mut items, projects) = scan_local(env, &opts.components, state);
    if let Some(only) = &opts.only_paths {
        items.retain(|i| only.contains(&i.local));
    }
    let mut manifest = store.read_manifest(&state.device_id)?.unwrap_or_else(|| Store::new_manifest(state.device_info(env)));
    manifest.device = state.device_info(env);
    let mut changed = false;
    for (k, v) in &projects {
        if manifest.projects.get(k) != Some(v) {
            manifest.projects.insert(k.clone(), v.clone());
            changed = true;
        }
    }
    let total = items.len();
    let mut since_checkpoint = 0;
    for (i, item) in items.iter().enumerate() {
        if opts.hooks.cancelled() {
            rep.cancelled = true;
            break;
        }
        opts.hooks.progress("backup", i, total);
        let md = match fs::metadata(&item.local) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if item.kind == Kind::Log && md.len() > LOG_MEM_CAP {
            rep.skipped_large.push(item.local.display().to_string());
            continue;
        }
        if opts.block_secrets && item.kind == Kind::Log && file_has_secrets(&item.local) {
            rep.blocked.push(item.local.display().to_string());
            continue;
        }
        let (size, mt) = if crate::sqlite::is_sqlite(&item.local) {
            crate::sqlite::fingerprint(&item.local).unwrap_or((md.len(), mtime_ms(&md)))
        } else {
            (md.len(), mtime_ms(&md))
        };
        let have = manifest.entries.get(&item.logical).map(|e| e.hash.clone());
        if let Some(c) = state.files.get(&item.logical) {
            if c.size == size && c.mtime_ms == mt && Some(&c.local_hash) == have.as_ref() {
                rep.unchanged += 1;
                continue;
            }
        }
        // Stream: read -> portable form -> 1 MiB chunks -> encrypt. Memory stays bounded; chunks the
        // vault already has (e.g. the unchanged head of a growing log) are not uploaded again.
        let res: Result<Option<(String, Vec<String>, u64, u64)>> = (|| {
            if opts.dry_run {
                let mut hw = HashWriter::new(store);
                if !prepare_to(item, env, opts.redact, &mut hw)? {
                    return Ok(None);
                }
                let (h, sz) = hw.finish();
                Ok(Some((h, vec![], sz, 0)))
            } else {
                let mut cw = ChunkWriter::new(store);
                if !prepare_to(item, env, opts.redact, &mut cw)? {
                    return Ok(None);
                }
                let st = cw.finish()?;
                Ok(Some((st.hash, st.chunks, st.size, st.new_bytes)))
            }
        })();
        let (id, chunks, out_size, new_bytes) = match res {
            Ok(Some(x)) => x,
            Ok(None) => continue,
            Err(e) => {
                rep.failed.push(format!("{}: {e}", item.local.display()));
                continue;
            }
        };
        let fs_entry = state.files.entry(item.logical.clone()).or_default();
        fs_entry.size = size;
        fs_entry.mtime_ms = mt;
        fs_entry.local_hash = id.clone();
        if have.as_ref() == Some(&id) {
            rep.unchanged += 1;
            continue;
        }
        if opts.dry_run {
            log(&format!("  would upload {}", item.logical));
            rep.uploaded += 1;
            continue;
        }
        if item.kind == Kind::File {
            fs_entry.synced_hash = Some(id.clone());
        } else if !fs_entry.seen.contains(&id) {
            fs_entry.seen.push(id.clone());
        }
        manifest.entries.insert(item.logical.clone(), Entry { kind: item.kind, size: out_size, mtime_ms: mt, hash: id, chunks });
        changed = true;
        rep.uploaded += 1;
        rep.bytes_uploaded += new_bytes;
        since_checkpoint += 1;
        if since_checkpoint >= 100 {
            // Save progress so an interrupted run resumes instead of starting over.
            manifest.seq += 1;
            manifest.updated = now_iso();
            store.write_manifest(&manifest)?;
            state.save(env)?;
            since_checkpoint = 0;
        }
    }
    opts.hooks.progress("backup", total, total);
    if changed && !opts.dry_run {
        manifest.seq += 1;
        manifest.updated = now_iso();
        store.write_manifest(&manifest)?;
    }
    if !opts.dry_run {
        state.save(env)?;
    }
    Ok(rep)
}

// ==================================================================== restore
fn local_hash_of(item: &Item, env: &Env, store: &Store, state: &mut State, redact: bool) -> Option<(String, i64)> {
    let md = fs::metadata(&item.local).ok()?;
    let (size, mt) = if crate::sqlite::is_sqlite(&item.local) {
        crate::sqlite::fingerprint(&item.local).unwrap_or((md.len(), mtime_ms(&md)))
    } else {
        (md.len(), mtime_ms(&md))
    };
    if let Some(c) = state.files.get(&item.logical) {
        if c.size == size && c.mtime_ms == mt && !c.local_hash.is_empty() {
            return Some((c.local_hash.clone(), mt));
        }
    }
    let mut hw = HashWriter::new(store);
    if !prepare_to(item, env, redact, &mut hw).ok()? {
        return None;
    }
    let (id, _) = hw.finish();
    let e = state.files.entry(item.logical.clone()).or_insert_with(FileState::default);
    e.size = size;
    e.mtime_ms = mt;
    e.local_hash = id.clone();
    Some((id, mt))
}

fn write_local(path: &Path, data: &[u8], mtime: Option<i64>, safety: &mut SafetyCopies) -> Result<()> {
    safety.save(path)?;
    atomic_write(path, data)?;
    if let Some(ms) = mtime {
        set_mtime(path, ms);
    }
    Ok(())
}

pub fn restore(env: &Env, store: &Store, state: &mut State, opts: &Options, log: Log) -> Result<Report> {
    let mut rep = Report::default();
    let (manifests, warns) = store.read_all_manifests()?;
    rep.warnings.extend(warns);
    let running = running_tools();
    if !running.is_empty() && !opts.dry_run {
        rep.warnings.push(format!("{} appears to be running; restored files may be overwritten by it. Close it and restore again to be safe.", running.join(" and ")));
    }
    let mut projects: BTreeMap<String, String> = BTreeMap::new();
    let mut cands: BTreeMap<String, Vec<(&str, &Entry)>> = BTreeMap::new();
    for m in &manifests {
        projects.extend(m.projects.clone());
        for (l, e) in &m.entries {
            if Component::of(l).map_or(false, |c| opts.components.contains(&c)) {
                cands.entry(l.clone()).or_default().push((m.device.id.as_str(), e));
            }
        }
    }
    let mut safety = SafetyCopies { env, stamp: stamp(), count: 0 };
    let mut missing = BTreeSet::new();
    let mut mcp_blobs: Vec<&Entry> = vec![];

    let total = cands.len();
    for (idx, (logical, entries)) in cands.iter().enumerate() {
        if opts.hooks.cancelled() {
            rep.cancelled = true;
            break;
        }
        opts.hooks.progress("restore", idx, total);
        let all: Vec<&Entry> = entries.iter().map(|(_, e)| *e).collect();
        // This device's own manifest entry is only what it last uploaded; prefer what other devices have.
        let others: Vec<&Entry> = entries.iter().filter(|(d, _)| *d != state.device_id).map(|(_, e)| *e).collect();
        let entries: Vec<&Entry> = if others.is_empty() { all } else { others };
        if logical == MCP {
            mcp_blobs = entries.clone();
            continue;
        }
        let Some(local) = local_path_for(env, logical, &projects, &opts.maps, &mut missing) else {
            rep.warnings.push(format!("ignored unrecognised vault entry {logical}"));
            continue;
        };
        if sqlite_busy(&local) {
            rep.busy.push(local.file_name().unwrap().to_string_lossy().into_owned());
            continue;
        }
        let kind = entries[0].kind;
        let item = Item { logical: logical.clone(), local: local.clone(), kind };
        let local_state = local.is_file().then(|| local_hash_of(&item, env, store, state, opts.redact)).flatten();
        let res = match kind {
            Kind::Log => restore_log(env, store, state, &item, &entries, local_state.as_ref().map(|x| x.0.as_str()), opts, &mut safety, &mut rep),
            Kind::File => restore_file(env, store, state, &item, &entries, local_state.as_ref(), opts, &mut safety, &mut rep, log),
        };
        if let Err(e) = res {
            rep.failed.push(format!("{}: {e}", local.display()));
        }
    }
    if !mcp_blobs.is_empty() {
        if let Err(e) = restore_mcp(env, store, &mcp_blobs, opts, &mut safety, &mut rep, log) {
            rep.failed.push(format!("MCP servers: {e}"));
        }
    }
    rep.safety_copies = safety.count;
    rep.missing_projects = missing.into_iter().collect();
    if !opts.dry_run {
        state.save(env)?;
        prune_safety(env);
    }
    Ok(rep)
}

#[allow(clippy::too_many_arguments)]
fn restore_log(env: &Env, store: &Store, state: &mut State, item: &Item, entries: &[&Entry], local_hash: Option<&str>, opts: &Options, safety: &mut SafetyCopies, rep: &mut Report) -> Result<()> {
    let mut remote: Vec<&Entry> = entries.to_vec();
    remote.sort_by_key(|e| e.mtime_ms);
    let seen: Vec<String> = state.files.get(&item.logical).map(|f| f.seen.clone()).unwrap_or_default();
    let mut distinct: Vec<&Entry> = vec![];
    for e in &remote {
        if !distinct.iter().any(|d| d.hash == e.hash) {
            distinct.push(e);
        }
    }
    if local_hash.is_some() && distinct.iter().all(|e| Some(e.hash.as_str()) == local_hash || seen.iter().any(|s| *s == e.hash)) {
        rep.unchanged += 1;
        return Ok(());
    }
    if distinct.iter().any(|e| e.size > LOG_MEM_CAP) {
        rep.skipped_large.push(item.logical.clone());
        return Ok(());
    }
    let mut cur: Vec<u8> = if item.local.is_file() { prepare_mem(item, env, opts.redact)?.unwrap_or_default() } else { vec![] };
    let before = cur.clone();
    let mut merged = false;
    for e in &distinct {
        if local_hash == Some(e.hash.as_str()) {
            continue;
        }
        let r = store.read_all(e)?;
        rep.bytes_downloaded += r.len() as u64;
        match jsonl::relation(&cur, &r) {
            Relation::Same | Relation::BBehind => {}
            Relation::ABehind => cur = r,
            Relation::Diverged => {
                cur = jsonl::merge(&cur, &r);
                merged = true;
            }
        }
    }
    if cur != before || !item.local.is_file() {
        if opts.dry_run {
            rep.downloaded += 1;
            return Ok(());
        }
        let out = jsonl::transform(&cur, &|p| to_local(p, &env.home, &opts.maps));
        write_local(&item.local, &out, None, safety)?;
        if merged { rep.merged += 1 } else { rep.downloaded += 1 }
    } else {
        rep.unchanged += 1;
    }
    if !opts.dry_run {
        let md = fs::metadata(&item.local).ok();
        let f = state.files.entry(item.logical.clone()).or_default();
        f.local_hash = store.keys.object_id(&cur);
        if let Some(md) = md {
            f.size = md.len();
            f.mtime_ms = mtime_ms(&md);
        }
        f.seen = distinct.iter().map(|e| e.hash.clone()).chain(seen).take(16).collect();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn restore_file(env: &Env, store: &Store, state: &mut State, item: &Item, entries: &[&Entry], local: Option<&(String, i64)>, opts: &Options, safety: &mut SafetyCopies, rep: &mut Report, log: Log) -> Result<()> {
    let winner = entries.iter().max_by(|a, b| a.mtime_ms.cmp(&b.mtime_ms).then(a.hash.cmp(&b.hash))).unwrap();
    if item.logical == SETTINGS {
        return restore_settings(env, store, state, item, winner, local, opts, safety, rep);
    }
    let base = state.files.get(&item.logical).and_then(|f| f.synced_hash.clone());
    let install = |state: &mut State, safety: &mut SafetyCopies, rep: &mut Report| -> Result<()> {
        if item.logical == SETTINGS {
            install_settings(env, store, winner, &item.local, safety)?;
        } else {
            install_stream(store, winner, &item.local, safety)?;
        }
        rep.bytes_downloaded += winner.size;
        let md = fs::metadata(&item.local)?;
        let f = state.files.entry(item.logical.clone()).or_default();
        f.size = md.len();
        f.mtime_ms = mtime_ms(&md);
        f.local_hash = winner.hash.clone();
        f.synced_hash = Some(winner.hash.clone());
        Ok(())
    };
    match local {
        None => {
            if opts.dry_run {
                log(&format!("  would create {}", item.local.display()));
            } else {
                install(state, safety, rep)?;
            }
            rep.downloaded += 1;
        }
        Some((lh, lmt)) => {
            if *lh == winner.hash {
                state.files.entry(item.logical.clone()).or_default().synced_hash = Some(lh.clone());
                rep.unchanged += 1;
                return Ok(());
            }
            let local_changed = base.as_deref() != Some(lh.as_str());
            let remote_changed = base.as_deref() != Some(winner.hash.as_str());
            if !remote_changed {
                rep.unchanged += 1; // local edit waiting to be uploaded
            } else if winner.mtime_ms > *lmt + TOL_MS {
                if local_changed {
                    rep.conflicts.push(format!("{} changed on two devices; kept the newer version (yours is saved in {})", item.logical, env_safety_hint(safety)));
                }
                if opts.dry_run {
                    log(&format!("  would update {}", item.local.display()));
                } else {
                    install(state, safety, rep)?;
                }
                rep.downloaded += 1;
            } else {
                if local_changed {
                    rep.conflicts.push(format!("{} changed on two devices; kept this device's newer version", item.logical));
                }
                rep.unchanged += 1;
            }
        }
    }
    Ok(())
}

fn env_safety_hint(s: &SafetyCopies) -> String {
    s.env.safety_root().join(&s.stamp).display().to_string()
}

/// MCP servers run commands, so they are only *added* (never overwritten), and each is listed.
fn restore_mcp(env: &Env, store: &Store, entries: &[&Entry], opts: &Options, safety: &mut SafetyCopies, rep: &mut Report, log: Log) -> Result<()> {
    let mut sorted: Vec<&Entry> = entries.to_vec();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.mtime_ms));
    let mut servers = serde_json::Map::new();
    for e in sorted {
        if let Value::Object(o) = serde_json::from_slice::<Value>(&store.read_all(e)?)? {
            for (k, v) in o {
                servers.entry(k).or_insert(v);
            }
        }
    }
    if !env.claude_json.is_file() {
        rep.warnings.push("run `claude` once on this device, then restore again to add your MCP servers".into());
        return Ok(());
    }
    let raw = fs::read(&env.claude_json)?;
    let mut doc: Value = serde_json::from_slice(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw))?;
    let obj = doc.as_object_mut().ok_or_else(|| anyhow!(".claude.json is not an object"))?;
    let cur = obj.entry("mcpServers").or_insert_with(|| Value::Object(Default::default()));
    let cur = cur.as_object_mut().ok_or_else(|| anyhow!("mcpServers is not an object"))?;
    let mut added = vec![];
    for (k, v) in servers {
        if !cur.contains_key(&k) {
            let cmd = v.get("command").or_else(|| v.get("url")).and_then(|c| c.as_str()).unwrap_or("?");
            log(&format!("  + MCP server '{k}': {cmd}"));
            added.push(k.clone());
            cur.insert(k, v);
        }
    }
    if added.is_empty() {
        return Ok(());
    }
    if !opts.dry_run {
        safety.save(&env.claude_json)?;
        atomic_write(&env.claude_json, serde_json::to_string_pretty(&doc)?.as_bytes())?;
    }
    rep.mcp_added = added;
    Ok(())
}

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
