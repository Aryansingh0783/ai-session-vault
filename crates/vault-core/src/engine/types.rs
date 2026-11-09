//! Engine configuration, state, events and the control handle.

use super::*;

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

pub(super) enum Msg {
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
    pub(super) tx: Sender<Msg>,
    pub(super) cancel: Arc<AtomicBool>,
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
