//! The always-on engine: a background thread that watches your AI tools' files and the vault folder,
//! waits for sessions to go quiet, then syncs only what changed. A UI (or the `avault daemon` command)
//! drives it through a [`Handle`] and listens to [`Event`]s.
//!
//! ```text
//!   file watcher ──┐
//!   vault watcher ─┼─> debounce ─> incremental backup / full sync ─> Event stream
//!   commands ──────┘                     (retry with backoff on errors)
//! ```

use crate::env::{Env, State};
use crate::paths::PathMap;
use crate::store::Store;
use crate::sync::{self, Component, Hooks, Options, Report};
use anyhow::Result;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub components: Vec<Component>,
    pub maps: Vec<PathMap>,
    pub redact: bool,
    pub block_secrets: bool,
    /// Wait this long after the last file change before syncing (lets a live session finish writing).
    pub quiet: Duration,
    /// Safety net: do a full sync at least this often even if no file event arrived.
    pub full_sync_every: Duration,
    /// Watch files (instant) in addition to the periodic full sync.
    pub watch: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            components: Component::ALL.to_vec(),
            maps: vec![],
            redact: false,
            block_secrets: false,
            quiet: Duration::from_secs(8),
            full_sync_every: Duration::from_secs(300),
            watch: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Idle,
    /// Changes seen; waiting for the session to go quiet.
    Waiting,
    Syncing,
    Paused,
    /// The last run failed; retrying with backoff.
    Backoff,
    Stopped,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub pending_changes: usize,
    pub last_sync: Option<String>,
    pub last_error: Option<String>,
    pub syncs_done: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Status(Status),
    Progress { phase: String, done: usize, total: usize },
    SyncFinished { full: bool, report: Report },
    Error { message: String },
    Log { message: String },
}

enum Msg {
    Fs(PathBuf),
    Remote,
    SyncNow,
    Pause,
    Resume,
    CancelRun,
    Stop,
}

/// Control surface for a running engine. Cheap to clone.
#[derive(Clone)]
pub struct Handle {
    tx: Sender<Msg>,
    cancel: Arc<AtomicBool>,
}

impl Handle {
    /// Run a full sync as soon as possible.
    pub fn sync_now(&self) {
        let _ = self.tx.send(Msg::SyncNow);
    }
    pub fn pause(&self) {
        let _ = self.tx.send(Msg::Pause);
    }
    pub fn resume(&self) {
        let _ = self.tx.send(Msg::Resume);
    }
    /// Abort the sync that is currently running (it stops at the next file).
    pub fn cancel_current(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Msg::CancelRun);
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Msg::Stop);
    }
}

pub struct Engine {
    pub handle: Handle,
    pub events: Receiver<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Engine {
    pub fn start(env: Env, store: Arc<Store>, cfg: EngineConfig) -> Result<Engine> {
        let (tx, rx) = channel::<Msg>();
        let (etx, erx) = channel::<Event>();
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = Handle { tx: tx.clone(), cancel: cancel.clone() };
        let mut watchers: Vec<RecommendedWatcher> = vec![];
        if cfg.watch {
            watchers = start_watchers(&env, &store, &tx);
        }
        let thread = std::thread::Builder::new().name("avault-engine".into()).spawn(move || {
            run(env, store, cfg, rx, etx, cancel);
            drop(watchers);
        })?;
        Ok(Engine { handle, events: erx, thread: Some(thread) })
    }

    /// Split into the control handle, the event stream and the worker thread (for servers that
    /// need to own the event stream themselves).
    pub fn into_parts(mut self) -> (Handle, Receiver<Event>, Option<std::thread::JoinHandle<()>>) {
        let t = self.thread.take();
        (self.handle, self.events, t)
    }

    /// Stop the engine and wait for it to finish.
    pub fn shutdown(mut self) {
        self.handle.stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn start_watchers(env: &Env, store: &Store, tx: &Sender<Msg>) -> Vec<RecommendedWatcher> {
    let own_state = env.state_dir.clone();
    let vault_devices = store.root.join("devices");
    let mut out = vec![];
    let mut add = |root: &Path, mode: RecursiveMode, remote: bool| {
        if !root.exists() {
            return;
        }
        let tx = tx.clone();
        let skip = own_state.clone();
        let devices = vault_devices.clone();
        let w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            for p in ev.paths {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if name.ends_with(".vaulttmp") {
                    continue;
                }
                if remote {
                    // Any change under devices/ - other devices publishing a new manifest.
                    if p.starts_with(&devices) {
                        let _ = tx.send(Msg::Remote);
                    }
                } else if !p.starts_with(&skip) || p.starts_with(skip.join("archive")) {
                    let _ = tx.send(Msg::Fs(p));
                }
            }
        });
        if let Ok(mut w) = w {
            if w.watch(root, mode).is_ok() {
                out.push(w);
            }
        }
    };
    add(&env.claude_dir, RecursiveMode::Recursive, false);
    add(&env.codex_dir, RecursiveMode::Recursive, false);
    add(&env.state_dir.join("archive"), RecursiveMode::Recursive, false);
    if let Some(d) = env.desktop_cfg.parent() {
        add(d, RecursiveMode::NonRecursive, false);
    }
    if let Some(h) = env.claude_json.parent() {
        add(h, RecursiveMode::NonRecursive, false); // for ~/.claude.json
    }
    add(&store.root.join("devices"), RecursiveMode::Recursive, true);
    out
}

struct Core {
    env: Env,
    store: Arc<Store>,
    state: State,
    cfg: EngineConfig,
    etx: Sender<Event>,
    cancel: Arc<AtomicBool>,
    status: Status,
}

impl Core {
    fn emit_status(&self) {
        let _ = self.etx.send(Event::Status(self.status.clone()));
    }

    fn set_phase(&mut self, p: Phase) {
        if self.status.phase != p {
            self.status.phase = p;
            self.emit_status();
        }
    }

    fn options(&self, only: Option<HashSet<PathBuf>>) -> Options {
        let etx = self.etx.clone();
        Options {
            components: self.cfg.components.clone(),
            maps: self.cfg.maps.clone(),
            dry_run: false,
            redact: self.cfg.redact,
            block_secrets: self.cfg.block_secrets,
            hooks: Hooks {
                progress: Some(Arc::new(move |phase, done, total| {
                    let _ = etx.send(Event::Progress { phase: phase.into(), done, total });
                })),
                cancel: Some(self.cancel.clone()),
            },
            only_paths: only.map(Arc::new),
        }
    }

    fn run_sync(&mut self, full: bool, dirty: HashSet<PathBuf>) -> Result<()> {
        self.set_phase(Phase::Syncing);
        self.cancel.store(false, Ordering::Relaxed);
        let opts = self.options(if full { None } else { Some(dirty) });
        let etx = self.etx.clone();
        let log = move |m: &str| {
            let _ = etx.send(Event::Log { message: m.to_string() });
        };
        let _lock = sync::lock(&self.env)?;
        let report = if full {
            sync::sync_unlocked(&self.env, &self.store, &mut self.state, &opts, &log)?
        } else {
            sync::backup(&self.env, &self.store, &mut self.state, &opts, &log)?
        };
        self.status.last_sync = Some(crate::util::now_iso());
        self.status.last_error = None;
        self.status.syncs_done += 1;
        let _ = self.etx.send(Event::SyncFinished { full, report });
        Ok(())
    }
}

fn run(env: Env, store: Arc<Store>, cfg: EngineConfig, rx: Receiver<Msg>, etx: Sender<Event>, cancel: Arc<AtomicBool>) {
    let state = State::load(&env);
    let status = Status { phase: Phase::Starting, pending_changes: 0, last_sync: None, last_error: None, syncs_done: 0 };
    let mut core = Core { env, store, state, cfg, etx, cancel, status };
    core.emit_status();

    let mut dirty: HashSet<PathBuf> = HashSet::new();
    let mut need_full = true; // first run is a full sync
    let mut last_event = Instant::now();
    let mut last_full = Instant::now() - Duration::from_secs(86400);
    let mut paused = false;
    let mut backoff = Duration::from_secs(5);
    let mut retry_at: Option<Instant> = None;

    loop {
        // How long can we sleep? Until the debounce expires, the retry time, or the next periodic full sync.
        let now = Instant::now();
        let mut wake = last_full + core.cfg.full_sync_every;
        if !dirty.is_empty() || need_full {
            wake = wake.min(last_event + core.cfg.quiet);
        }
        if let Some(r) = retry_at {
            wake = wake.min(r);
        }
        let timeout = wake.saturating_duration_since(now).max(Duration::from_millis(50));
        match rx.recv_timeout(timeout) {
            Ok(Msg::Fs(p)) => {
                dirty.insert(p);
                last_event = Instant::now();
                core.status.pending_changes = dirty.len();
                if !paused {
                    core.set_phase(Phase::Waiting);
                }
                continue;
            }
            Ok(Msg::Remote) => {
                need_full = true;
                last_event = Instant::now();
                if !paused {
                    core.set_phase(Phase::Waiting);
                }
                continue;
            }
            Ok(Msg::SyncNow) => {
                need_full = true;
                last_event = Instant::now() - core.cfg.quiet;
                retry_at = None;
            }
            Ok(Msg::Pause) => {
                paused = true;
                core.set_phase(Phase::Paused);
                continue;
            }
            Ok(Msg::Resume) => {
                paused = false;
                last_event = Instant::now() - core.cfg.quiet;
                core.set_phase(if dirty.is_empty() && !need_full { Phase::Idle } else { Phase::Waiting });
                continue;
            }
            Ok(Msg::CancelRun) => continue,
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if paused {
            continue;
        }
        let now = Instant::now();
        let periodic = now.duration_since(last_full) >= core.cfg.full_sync_every;
        let quiet = now.duration_since(last_event) >= core.cfg.quiet;
        let retry_due = retry_at.is_some_and(|r| now >= r);
        let due = retry_due || (quiet && (!dirty.is_empty() || need_full)) || periodic;
        if !due {
            continue;
        }
        let full = need_full || periodic || retry_due;
        let batch = std::mem::take(&mut dirty);
        core.status.pending_changes = 0;
        match core.run_sync(full, batch.clone()) {
            Ok(()) => {
                if full {
                    last_full = Instant::now();
                    need_full = false;
                }
                backoff = Duration::from_secs(5);
                retry_at = None;
                core.set_phase(Phase::Idle);
                core.emit_status();
            }
            Err(e) => {
                // Put the work back and retry later with exponential backoff (e.g. cloud folder still syncing).
                dirty.extend(batch);
                need_full |= full;
                let msg = format!("{e:#}");
                core.status.last_error = Some(msg.clone());
                let _ = core.etx.send(Event::Error { message: msg });
                retry_at = Some(Instant::now() + backoff);
                backoff = (backoff * 2).min(Duration::from_secs(300));
                core.set_phase(Phase::Backoff);
                core.emit_status();
            }
        }
    }
    core.status.phase = Phase::Stopped;
    core.emit_status();
}
