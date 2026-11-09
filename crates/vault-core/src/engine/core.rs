//! The engine loop: debounce, incremental and full syncs, backoff.

use super::*;

pub(super) struct Core {
    pub(super) env: Env,
    pub(super) store: Arc<Store>,
    pub(super) state: State,
    pub(super) cfg: EngineConfig,
    pub(super) etx: Sender<Event>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) status: Status,
}

impl Core {
    pub(super) fn emit_status(&self) {
        let _ = self.etx.send(Event::Status(self.status.clone()));
    }

    pub(super) fn set_phase(&mut self, p: Phase) {
        if self.status.phase != p {
            self.status.phase = p;
            self.emit_status();
        }
    }

    pub(super) fn options(&self, only: Option<HashSet<PathBuf>>) -> Options {
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

    pub(super) fn run_sync(&mut self, full: bool, dirty: HashSet<PathBuf>) -> Result<()> {
        self.set_phase(Phase::Syncing);
        self.cancel.store(false, Ordering::Relaxed);
        let opts = self.options(if full { None } else { Some(dirty) });
        let etx = self.etx.clone();
        let log = move |m: &str| {
            let _ = etx.send(Event::Log { message: m.to_string() });
        };
        let _lock = sync::lock(&self.env)?;
        let report =
            if full { sync::sync_unlocked(&self.env, &self.store, &mut self.state, &opts, &log)? } else { sync::backup(&self.env, &self.store, &mut self.state, &opts, &log)? };
        self.status.last_sync = Some(crate::util::now_iso());
        self.status.last_error = None;
        self.status.syncs_done += 1;
        let _ = self.etx.send(Event::SyncFinished { full, report });
        Ok(())
    }
}

pub(super) fn run(env: Env, store: Arc<Store>, cfg: EngineConfig, rx: Receiver<Msg>, etx: Sender<Event>, cancel: Arc<AtomicBool>) {
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
