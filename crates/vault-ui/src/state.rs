//! Shared state of the running UI: the app handle, the engine event log and small caches.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vault_core::engine::{Event, Handle, Status};
use vault_core::env::Env;
use vault_core::overview;
use vault_core::store::Store;

use crate::http::{err, ok_json, Reply};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryItem {
    pub at: String,
    pub full: bool,
    pub uploaded: usize,
    pub downloaded: usize,
    pub merged: usize,
    pub conflicts: usize,
    pub failed: usize,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    pub notes: Vec<String>,
}

#[derive(Serialize, Clone, Default)]
pub(crate) struct Progress {
    pub(crate) phase: String,
    pub(crate) done: usize,
    pub(crate) total: usize,
}

#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) status: Option<Status>,
    pub(crate) progress: Option<Progress>,
    pub(crate) history: Vec<HistoryItem>,
    pub(crate) last_error: Option<String>,
    pub(crate) overview: Option<(Instant, Value)>,
    pub(crate) disk: Option<(Instant, u64)>,
}

pub(crate) struct App {
    pub(crate) env: Env,
    pub(crate) store: Arc<Store>,
    pub(crate) token: String,
    pub(crate) port: u16,
    pub(crate) handle: Option<Handle>,
    pub(crate) shared: Mutex<Shared>,
    pub(crate) subscribers: Mutex<Vec<Sender<String>>>,
    pub(crate) started: Instant,
}


impl App {
    pub(crate) fn broadcast(&self, line: String) {
        self.subscribers.lock().unwrap().retain(|s| s.send(line.clone()).is_ok());
    }

    pub(crate) fn on_event(&self, ev: &Event) {
        {
            let mut sh = self.shared.lock().unwrap();
            match ev {
                Event::Status(s) => sh.status = Some(s.clone()),
                Event::Progress { phase, done, total } => {
                    sh.progress = Some(Progress { phase: phase.clone(), done: *done, total: *total });
                }
                Event::SyncFinished { full, report } => {
                    let changed = report.uploaded + report.downloaded + report.merged + report.conflicts.len() + report.failed.len() > 0;
                    sh.progress = None;
                    if !changed {
                        // Routine "nothing to do" runs would bury the real activity.
                        return self.broadcast(serde_json::to_string(ev).unwrap_or_default());
                    }
                    let mut notes: Vec<String> = report.conflicts.clone();
                    notes.extend(report.warnings.iter().take(3).cloned());
                    sh.history.push(HistoryItem {
                        at: vault_core::util::now_iso(),
                        full: *full,
                        uploaded: report.uploaded,
                        downloaded: report.downloaded,
                        merged: report.merged,
                        conflicts: report.conflicts.len(),
                        failed: report.failed.len(),
                        bytes_uploaded: report.bytes_uploaded,
                        bytes_downloaded: report.bytes_downloaded,
                        notes: notes.into_iter().take(6).collect(),
                    });
                    let n = sh.history.len();
                    if n > 300 {
                        sh.history.drain(..n - 300);
                    }
                    sh.progress = None;
                    sh.overview = None; // the vault changed
                    sh.disk = None;
                    let _ = std::fs::write(self.env.state_dir.join("history.json"), serde_json::to_vec(&sh.history).unwrap_or_default());
                }
                Event::Error { message } => sh.last_error = Some(message.clone()),
                Event::Log { .. } => {}
            }
        }
        if !matches!(ev, Event::Log { .. }) {
            self.broadcast(serde_json::to_string(ev).unwrap_or_default());
        }
    }

    pub(crate) fn overview_json(&self) -> Result<Value> {
        {
            let sh = self.shared.lock().unwrap();
            if let Some((at, v)) = &sh.overview {
                if at.elapsed() < Duration::from_secs(10) {
                    return Ok(v.clone());
                }
            }
        }
        let o = overview::overview(&self.env, &self.store, &[])?;
        let disk = {
            let sh = self.shared.lock().unwrap();
            match sh.disk {
                Some((at, d)) if at.elapsed() < Duration::from_secs(60) => d,
                _ => {
                    drop(sh);
                    let d = dir_size(&self.store.root);
                    self.shared.lock().unwrap().disk = Some((Instant::now(), d));
                    d
                }
            }
        };
        let mut v = serde_json::to_value(&o)?;
        v["disk_bytes"] = json!(disk);
        self.shared.lock().unwrap().overview = Some((Instant::now(), v.clone()));
        Ok(v)
    }

    pub(crate) fn command(&self, f: impl FnOnce(&Handle)) -> Reply {
        match &self.handle {
            Some(h) => {
                f(h);
                ok_json(json!({ "ok": true }))
            }
            None => err(409, "the sync engine is not running in this session"),
        }
    }
}

fn dir_size(root: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(root).into_iter().flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()
}
