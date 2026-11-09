//! The always-on engine: a background thread that watches your AI tools' files and the vault folder,
//! waits for sessions to go quiet, then syncs only what changed. A UI (or the `avault daemon` command)
//! drives it through a [`Handle`] and listens to [`Event`]s.
//!
//! ```text
//!   file watcher ──┐
//!   vault watcher ─┼─> debounce ─> incremental backup / full sync ─> Event stream
//!   commands ──────┘                     (retry with backoff on errors)
//! ```

mod core;
mod types;
mod watch;
use core::*;
use types::*;
pub use types::{EngineConfig, Event, Handle, Phase, Status};
use watch::*;

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
