//! Command-line definition (clap).

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "avault", version, about = "Carry Claude Code / Codex / Claude Desktop sessions between your devices - encrypted, conflict-free.")]
pub(crate) struct Cli {
    /// Vault folder (remembered after `init`). Can also be set with AI_VAULT_DIR.
    #[arg(long, global = true, env = "AI_VAULT_DIR")]
    pub(crate) vault: Option<PathBuf>,
    /// Use this folder as the home directory (testing and demos; never touches your real one).
    #[arg(long, global = true, hide = true)]
    pub(crate) home: Option<PathBuf>,
    /// Machine-readable output (for the desktop app and scripts).
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// With no command, AI Session Vault opens as an app (what a double-click does).
    #[command(subcommand)]
    pub(crate) cmd: Option<Cmd>,
}

#[derive(clap::Args, Clone, Default)]
pub(crate) struct SyncArgs {
    /// Comma list: claude-code,claude-desktop,codex,archive
    #[arg(long)]
    pub(crate) only: Option<String>,
    /// Remap a project path, e.g. --map "D:\work=~/work" (repeatable)
    #[arg(long = "map", value_name = "OLD=NEW")]
    pub(crate) maps: Vec<String>,
    /// Show what would change without writing anything.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Replace API keys/tokens found in session logs before uploading.
    #[arg(long)]
    pub(crate) redact: bool,
    /// Refuse to upload session logs that contain API keys/tokens (listed in the report).
    #[arg(long)]
    pub(crate) block_secrets: bool,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Create a new vault (asks for a passphrase and shows a one-time recovery key).
    Init,
    /// Pull other devices' changes in, then publish yours (recommended).
    Sync(SyncArgs),
    /// Publish this device's sessions to the vault.
    Backup(SyncArgs),
    /// Apply the vault's sessions to this device.
    Restore(SyncArgs),
    /// Show what is in the vault and which devices use it.
    Status,
    /// Import claude.ai / ChatGPT data-export zips into the searchable archive.
    Import { paths: Vec<PathBuf> },
    /// Search every archived chat and Claude Code session on this device.
    Search {
        query: String,
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// Turn a chat/session into a paste-ready context pack for a new conversation.
    Context {
        query: String,
        #[arg(long)]
        pick: Option<usize>,
        #[arg(long, default_value_t = 60000)]
        max_chars: usize,
        /// Write to this file instead of the default location.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Never upload a project (portable path or prefix, e.g. `~/.claude-mem/observer-sessions`). No argument lists them.
    Exclude {
        path: Option<String>,
        /// Stop excluding it.
        #[arg(long)]
        remove: bool,
    },
    /// Check for the common problems (retention purge, leaked secrets, running tools).
    Doctor,
    /// Change the vault passphrase (no data is re-encrypted).
    Passwd,
    /// Issue a new recovery key (the old one stops working).
    Recovery,
    /// Remove vault objects no device references any more.
    Gc,
    /// Check the vault's integrity (every file the manifests list is present; --deep also decrypts them).
    Verify {
        #[arg(long)]
        deep: bool,
    },
    /// Store the vault passphrase in the OS credential store (Windows Credential Manager / macOS Keychain / Secret Service), so the daemon and later commands don't ask.
    Remember,
    /// Remove the stored passphrase from the OS credential store.
    Forget,
    /// Summary of the vault for a UI: devices, projects (with local paths), totals. Use with --json.
    Overview,
    /// Print a one-time code (valid 15 minutes) that lets another device join without the passphrase.
    Enroll {
        #[arg(long, default_value_t = 15)]
        minutes: u64,
    },
    /// Join an existing vault with a code from `avault enroll` (no passphrase needed).
    Join {
        #[arg(long)]
        code: String,
    },
    /// List the devices using this vault, or forget one.
    Devices {
        /// Forget the device whose id starts with this (then run `gc`).
        #[arg(long)]
        remove: Option<String>,
        /// Revoke a joined device's passphrase-free access (id from the "Joined devices" list).
        #[arg(long)]
        revoke: Option<String>,
    },
    /// Bring a vault made by the Python version (its `data` folder) into this one.
    ImportV1 { data_dir: PathBuf },
    /// Run the always-on engine: watches your sessions and the vault, syncs only what changed.
    #[command(alias = "watch")]
    Daemon {
        #[command(flatten)]
        args: SyncArgs,
        /// Seconds a session must be quiet before it is synced.
        #[arg(long, default_value_t = 8)]
        quiet: u64,
        /// Do a full sync at least this often (seconds), even without file events.
        #[arg(long, default_value_t = 300)]
        full_every: u64,
        /// Poll only (no file watching).
        #[arg(long)]
        no_watch: bool,
        /// Append events (JSON lines) to this file instead of printing; for unattended runs.
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Open the app in its own window with a tray icon (closing the window keeps syncing in the tray).
    App {
        #[command(flatten)]
        args: SyncArgs,
        /// Start in the tray without showing the window (used when starting at login).
        #[arg(long)]
        hidden: bool,
    },
    /// Open the app: a local web UI over the sync engine (read the link it prints; it carries a one-time token).
    Ui {
        #[command(flatten)]
        args: SyncArgs,
        /// Port to listen on (127.0.0.1 only). 0 = pick a free one.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Don't open a window; just print the link.
        #[arg(long)]
        no_open: bool,
        /// Show the vault without running the sync engine (nothing is uploaded or restored).
        #[arg(long)]
        read_only: bool,
    },
    /// Start the engine automatically at login (Windows scheduled task / macOS launchd / Linux systemd user unit).
    Autostart {
        /// enable | disable | status
        action: String,
        /// Show what would be installed without changing anything.
        #[arg(long)]
        print: bool,
    },
}
