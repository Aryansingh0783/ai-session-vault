use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
mod autostart;
use vault_core::archive;
use vault_core::crypto::{KdfParams, MIN_PASSPHRASE};
use vault_core::doctor::{self, Level};
use vault_core::env::{Env, State};
use vault_core::paths::parse_maps;
use vault_core::store::Store;
use vault_core::sync::{self, Component, Options, Report};

#[derive(Parser)]
#[command(name = "avault", version, about = "Carry Claude Code / Codex / Claude Desktop sessions between your devices - encrypted, conflict-free.")]
struct Cli {
    /// Vault folder (remembered after `init`). Can also be set with AI_VAULT_DIR.
    #[arg(long, global = true, env = "AI_VAULT_DIR")]
    vault: Option<PathBuf>,
    /// Use this folder as the home directory (testing and demos; never touches your real one).
    #[arg(long, global = true, hide = true)]
    home: Option<PathBuf>,
    /// Machine-readable output (for the desktop app and scripts).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args, Clone)]
struct SyncArgs {
    /// Comma list: claude-code,claude-desktop,codex,archive
    #[arg(long)]
    only: Option<String>,
    /// Remap a project path, e.g. --map "D:\work=~/work" (repeatable)
    #[arg(long = "map", value_name = "OLD=NEW")]
    maps: Vec<String>,
    /// Show what would change without writing anything.
    #[arg(long)]
    dry_run: bool,
    /// Replace API keys/tokens found in session logs before uploading.
    #[arg(long)]
    redact: bool,
    /// Refuse to upload session logs that contain API keys/tokens (listed in the report).
    #[arg(long)]
    block_secrets: bool,
}

#[derive(Subcommand)]
enum Cmd {
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
    Search { query: String, #[arg(long, default_value_t = 15)] limit: usize },
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

fn config_path(env: &Env) -> PathBuf {
    env.state_dir.join("config.json")
}

fn saved_vault(env: &Env) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(config_path(env)).ok()?).ok()?;
    v.get("vault")?.as_str().map(PathBuf::from)
}

fn save_vault(env: &Env, p: &Path) -> Result<()> {
    vault_core::env::set_config(&env.state_dir, "vault", serde_json::json!(p))
}

fn keyring_entry(vault_id: &str) -> Option<keyring::Entry> {
    keyring::Entry::new("ai-session-vault", vault_id).ok()
}

fn device_credential_path(env: &Env, vault_id: &str) -> PathBuf {
    env.state_dir.join(format!("device-{vault_id}.key"))
}

fn load_device_credential(env: &Env, vault_id: &str) -> Option<String> {
    if let Ok(e) = keyring::Entry::new("ai-session-vault-device", vault_id) {
        if let Ok(p) = e.get_password() {
            return Some(p);
        }
    }
    std::fs::read_to_string(device_credential_path(env, vault_id)).ok().map(|s| s.trim().to_string())
}

fn prompt_secret(label: &str) -> Result<String> {
    if let Ok(p) = std::env::var("AI_VAULT_PASSPHRASE") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    if !std::io::stdin().is_terminal() {
        bail!("no terminal to ask for the passphrase: set AI_VAULT_PASSPHRASE or run in a terminal");
    }
    Ok(rpassword::prompt_password(label)?)
}

fn vault_dir(cli: &Cli, env: &Env) -> Result<PathBuf> {
    cli.vault.clone().or_else(|| saved_vault(env)).ok_or_else(|| anyhow!("no vault yet: run `avault init --vault <folder>` (a folder inside OneDrive/Google Drive/Dropbox, or on a USB stick)"))
}

fn open(cli: &Cli, env: &Env) -> Result<Store> {
    let dir = vault_dir(cli, env)?;
    if !Store::exists(&dir) {
        bail!("no vault in {} - if it is still syncing, wait for it to finish; otherwise run `avault init`", dir.display());
    }
    let vault_id = Store::read_header(&dir)?.vault_id;
    let env_pw_set = std::env::var("AI_VAULT_PASSPHRASE").is_ok_and(|p| !p.is_empty());
    let from_keyring = if env_pw_set { None } else { keyring_entry(&vault_id).and_then(|e| e.get_password().ok()) };
    if !env_pw_set && from_keyring.is_none() {
        if let Some(cred) = load_device_credential(env, &vault_id) {
            match Store::open_with_device(&dir, &cred) {
                Ok(s) => {
                    save_vault(env, &dir)?;
                    return Ok(s);
                }
                Err(e) => eprintln!("(this device's stored access no longer works: {e}; ask for the passphrase)"),
            }
        }
    }
    let store = match from_keyring {
        Some(pw) if !pw.is_empty() => Store::open(&dir, &pw).or_else(|_| {
            eprintln!("(the stored passphrase no longer works; ask for it again)");
            Store::open(&dir, &prompt_secret("Vault passphrase (or recovery key): ")?)
        })?,
        _ => Store::open(&dir, &prompt_secret("Vault passphrase (or recovery key): ")?)?,
    };
    save_vault(env, &dir)?;
    Ok(store)
}

fn options(a: &SyncArgs) -> Result<Options> {
    let components = match &a.only {
        Some(s) => s.split(',').map(|c| Component::parse(c.trim())).collect::<Result<Vec<_>>>()?,
        None => Component::ALL.to_vec(),
    };
    Ok(Options { components, maps: parse_maps(&a.maps)?, dry_run: a.dry_run, redact: a.redact, block_secrets: a.block_secrets, ..Options::default() })
}

fn print_report(r: &Report, json: bool) {
    if json {
        println!("{}", serde_json::to_string(r).unwrap());
        return;
    }
    println!(
        "  -> {} uploaded, {} downloaded, {} merged, {} already up to date",
        r.uploaded, r.downloaded, r.merged, r.unchanged
    );
    if r.safety_copies > 0 {
        println!("  {} replaced file(s) were saved first (in ~/.ai-vault/safety-copies)", r.safety_copies);
    }
    let sections: [(&str, &Vec<String>); 8] = [
        ("Conflicts (kept the newer version)", &r.conflicts),
        ("Files that couldn't be written", &r.failed),
        ("In use by an app, skipped (quit it and run again)", &r.busy),
        ("MCP servers added", &r.mcp_added),
        ("Project folders missing on this device (sessions restored anyway; clone them or use --map OLD=NEW)", &r.missing_projects),
        ("Skipped, over 2 GB", &r.skipped_large),
        ("Not uploaded: contain secrets (--block-secrets)", &r.blocked),
        ("Notes", &r.warnings),
    ];
    for (title, items) in sections {
        if !items.is_empty() {
            println!("  ! {title}:");
            for i in items.iter().take(10) {
                println!("      {i}");
            }
            if items.len() > 10 {
                println!("      ... and {} more", items.len() - 10);
            }
        }
    }
}

fn to_clipboard(text: &str) -> bool {
    use std::process::{Command, Stdio};
    let cmds: Vec<(&str, Vec<&str>, Vec<u8>)> = if cfg!(windows) {
        let mut b = vec![0xFF, 0xFE];
        b.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
        vec![("clip", vec![], b)]
    } else if cfg!(target_os = "macos") {
        vec![("pbcopy", vec![], text.as_bytes().to_vec())]
    } else {
        vec![("wl-copy", vec![], text.as_bytes().to_vec()), ("xclip", vec!["-selection", "clipboard"], text.as_bytes().to_vec())]
    };
    for (c, args, data) in cmds {
        if let Ok(mut ch) = Command::new(c).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            if ch.stdin.take().is_some_and(|mut s| s.write_all(&data).is_ok()) && ch.wait().is_ok_and(|s| s.success()) {
                return true;
            }
        }
    }
    false
}

fn new_passphrase() -> Result<String> {
    if let Ok(p) = std::env::var("AI_VAULT_PASSPHRASE") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    println!("Choose a vault passphrase ({MIN_PASSPHRASE}+ characters). Use the same one on every device.");
    loop {
        let a = prompt_secret("New passphrase: ")?;
        if a.chars().count() < MIN_PASSPHRASE {
            println!("Too short.");
            continue;
        }
        if prompt_secret("Repeat: ")? == a {
            return Ok(a);
        }
        println!("Didn't match.");
    }
}

/// Open the UI as an app-style window (Edge/Chrome `--app`) when available, else the default browser.
fn open_app_window(url: &str) {
    use std::process::Command;
    if cfg!(windows) {
        let candidates = [
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        ];
        let mut found: Vec<String> = candidates.iter().map(|c| c.to_string()).collect();
        for exe in ["msedge.exe", "chrome.exe"] {
            for hive in ["HKLM", "HKCU"] {
                let key = format!(r"{hive}\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}");
                if let Ok(out) = Command::new("reg").args(["query", &key, "/ve"]).output() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    if let Some(line) = text.lines().find(|l| l.contains("REG_SZ")) {
                        if let Some(p) = line.split("REG_SZ").nth(1) {
                            found.insert(0, p.trim().to_string());
                        }
                    }
                }
            }
        }
        for c in found {
            if std::path::Path::new(&c).exists() && Command::new(&c).arg(format!("--app={url}")).spawn().is_ok() {
                return;
            }
        }
        let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
    } else if cfg!(target_os = "macos") {
        let _ = Command::new("open").arg(url).spawn();
    } else {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

fn gethostname_string() -> String {
    std::env::var("AI_VAULT_DEVICE_NAME").or_else(|_| std::env::var("COMPUTERNAME")).or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "this device".into())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let env = Env::detect(cli.home.as_deref())?;
    let log = |m: &str| println!("{m}");
    match &cli.cmd {
        Cmd::Init => {
            let dir = cli.vault.clone().ok_or_else(|| anyhow!("pass --vault <folder>"))?;
            if Store::exists(&dir) {
                bail!("{} already contains a vault. On your other devices just run `avault sync --vault {}`.", dir.display(), dir.display());
            }
            let pass = new_passphrase()?;
            let (_, recovery) = Store::init(&dir, &pass, KdfParams::default())?;
            save_vault(&env, &dir)?;
            if cli.json {
                println!("{}", serde_json::json!({ "vault": dir, "recovery_key": recovery }));
            } else {
                println!("\nVault created in {}\n\nRECOVERY KEY (shown once - store it somewhere safe, e.g. a password manager):\n\n    {recovery}\n\nIt unlocks the vault if you forget the passphrase.", dir.display());
            }
        }
        Cmd::Sync(a) | Cmd::Backup(a) | Cmd::Restore(a) => {
            let store = open(&cli, &env)?;
            let opts = options(a)?;
            let mut state = State::load(&env);
            let _lock = sync::lock(&env)?;
            let rep = match &cli.cmd {
                Cmd::Sync(_) => {
                    let r = sync::restore(&env, &store, &mut state, &opts, &log)?;
                    let mut r = r;
                    r.absorb(sync::backup(&env, &store, &mut state, &opts, &log)?);
                    r
                }
                Cmd::Backup(_) => sync::backup(&env, &store, &mut state, &opts, &log)?,
                _ => sync::restore(&env, &store, &mut state, &opts, &log)?,
            };
            print_report(&rep, cli.json);
            if !rep.failed.is_empty() {
                std::process::exit(2);
            }
        }
        Cmd::Status => {
            let store = open(&cli, &env)?;
            let (ms, warns) = store.read_all_manifests()?;
            if cli.json {
                println!("{}", serde_json::json!({ "vault": store.root, "devices": ms.iter().map(|m| serde_json::json!({"name": m.device.name, "os": m.device.os, "updated": m.updated, "files": m.entries.len()})).collect::<Vec<_>>(), "warnings": warns }));
                return Ok(());
            }
            println!("Vault: {}", store.root.display());
            for m in &ms {
                let sessions = m.entries.keys().filter(|k| k.starts_with("claude-code/projects/") && k.ends_with(".jsonl")).count();
                println!("  {} ({}): {} files, {} Claude Code session logs, last sync {}", m.device.name, m.device.os, m.entries.len(), sessions, m.updated);
            }
            for w in warns {
                println!("  ! {w}");
            }
        }
        Cmd::Import { paths } => {
            let inbox = env.state_dir.join("inbox");
            let list: Vec<PathBuf> = if paths.is_empty() {
                std::fs::create_dir_all(&inbox)?;
                std::fs::read_dir(&inbox)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "zip" || e == "json") || p.is_dir()).collect()
            } else {
                paths.clone()
            };
            if list.is_empty() {
                println!("Nothing to import. Pass export .zip files, or drop them in {}", inbox.display());
                return Ok(());
            }
            for p in list {
                match archive::load_export(&p).and_then(|c| archive::save_all(&env, &c).map(|r| (c.len(), r))) {
                    Ok((n, (added, updated))) => println!("  + {}: {n} conversations ({added} new, {updated} updated)", p.display()),
                    Err(e) => println!("  x {}: {e}", p.display()),
                }
            }
            println!("Run `avault sync` to carry the archive to your other devices.");
        }
        Cmd::Search { query, limit } => {
            let hits = archive::search(archive::load_all(&env, true), query, *limit);
            if cli.json {
                println!("{}", serde_json::to_string(&hits.iter().map(|h| serde_json::json!({"id": h.conversation.id, "source": h.conversation.source, "title": h.conversation.title, "updated": h.conversation.updated, "project": h.conversation.project, "snippet": h.snippet})).collect::<Vec<_>>())?);
            } else if hits.is_empty() {
                println!("No matches.");
            }
            for h in hits.iter().filter(|_| !cli.json) {
                let c = &h.conversation;
                println!("[{}] {}  ({}, id {})\n    {}", c.source, c.title.chars().take(70).collect::<String>(), c.updated, c.id.chars().take(8).collect::<String>(), h.snippet);
            }
        }
        Cmd::Context { query, pick, max_chars, out } => {
            let hits = archive::search(archive::load_all(&env, true), query, 25);
            if hits.is_empty() {
                bail!("no match - try a word from the title, the project folder, or a session id");
            }
            let idx = match (pick, hits.len()) {
                (Some(n), _) if *n >= 1 && *n <= hits.len() => n - 1,
                (Some(_), _) => bail!("--pick must be between 1 and {}", hits.len()),
                (None, 1) => 0,
                (None, _) => {
                    for (i, h) in hits.iter().enumerate() {
                        println!("{:>3}. [{}] {}  ({})", i + 1, h.conversation.source, h.conversation.title.chars().take(70).collect::<String>(), h.conversation.updated);
                    }
                    bail!("several matches - re-run with --pick N");
                }
            };
            let c = &hits[idx].conversation;
            let pack = archive::build_pack(c, *max_chars);
            let path = out.clone().unwrap_or_else(|| env.state_dir.join("context-packs").join(format!("{}.md", c.id.chars().take(12).collect::<String>())));
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&path, &pack).with_context(|| path.display().to_string())?;
            println!("Context pack saved: {} ({} chars)", path.display(), pack.chars().count());
            if to_clipboard(&pack) {
                println!("Copied to the clipboard - paste it as the first message of a new chat.");
            }
        }
        Cmd::Exclude { path, remove } => {
            let mut list = env.exclude_projects.clone();
            if let Some(p) = path {
                let p = p.trim().replace('\\', "/");
                if *remove {
                    list.retain(|x| *x != p);
                } else if !list.contains(&p) {
                    list.push(p);
                }
                vault_core::env::set_config(&env.state_dir, "exclude_projects", serde_json::json!(list))?;
            }
            if list.is_empty() {
                println!("No projects excluded.");
            }
            for x in &list {
                println!("{x}");
            }
        }
        Cmd::Doctor => {
            let f = doctor::run(&env, vault_dir(&cli, &env).is_ok());
            if cli.json {
                println!("{}", serde_json::to_string(&f)?);
            }
            for x in f.iter().filter(|_| !cli.json) {
                let mark = match x.level { Level::Ok => "ok  ", Level::Warn => "WARN", Level::Info => "info" };
                println!("[{mark}] {}{}", x.title, if x.detail.is_empty() { String::new() } else { format!(" - {}", x.detail) });
            }
        }
        Cmd::Passwd => {
            let mut store = open(&cli, &env)?;
            let new = new_passphrase()?;
            let keys = &store.keys;
            let mut h = store.header.clone();
            h.change_passphrase(keys, &new)?;
            store.header = h;
            store.save_header()?;
            println!("Passphrase changed. Other devices use the new one the next time they sync.");
        }
        Cmd::Recovery => {
            let mut store = open(&cli, &env)?;
            let mut h = store.header.clone();
            let rec = h.rotate_recovery(&store.keys);
            store.header = h;
            store.save_header()?;
            println!("New RECOVERY KEY (the old one no longer works):\n\n    {rec}");
        }
        Cmd::ImportV1 { data_dir } => {
            let store = open(&cli, &env)?;
            let r = vault_core::legacy::import_v1(&store, data_dir)?;
            println!("Imported {} file(s) from {} project(s). Run `avault sync` to distribute them.", r.files, r.projects);
            println!("Not imported (v1 encrypted fields inside them): {}. Sync those fresh from the device that has them.", r.skipped.join(", "));
        }
        Cmd::Ui { args, port, no_open, read_only } => {
            let store = std::sync::Arc::new(open(&cli, &env)?);
            let o = options(args)?;
            let engine = (!*read_only).then(|| vault_core::engine::EngineConfig { components: o.components, maps: o.maps, redact: o.redact, block_secrets: o.block_secrets, ..Default::default() });
            let ui = vault_ui::Ui::start(env.clone(), store, vault_ui::UiOptions { port: *port, engine })?;
            println!("AI Session Vault is running at:

    {}

(Keep this window open. Ctrl+C quits.)", ui.url);
            if !*no_open {
                open_app_window(&ui.url);
            }
            ui.wait();
        }
        Cmd::App { args } => {
            let store = std::sync::Arc::new(open(&cli, &env)?);
            let o = options(args)?;
            let engine = vault_core::engine::EngineConfig { components: o.components, maps: o.maps, redact: o.redact, block_secrets: o.block_secrets, ..Default::default() };
            let ui = vault_ui::Ui::start(env.clone(), store, vault_ui::UiOptions { port: 0, engine: Some(engine) })?;
            let exe = std::env::current_exe()?.with_file_name(if cfg!(windows) { "avault-window.exe" } else { "avault-window" });
            if !exe.exists() {
                eprintln!("avault-window was not found next to avault ({}). Opening in your browser instead.", exe.display());
                println!("AI Session Vault is running at:

    {}

(Keep this window open. Ctrl+C quits.)", ui.url);
                open_app_window(&ui.url);
                ui.wait();
                return Ok(());
            }
            // The window process shows the UI and the tray icon; closing the window hides it to the tray.
            // It exits on "Quit", and then the engine stops too.
            let status = std::process::Command::new(&exe).arg(&ui.url).status().with_context(|| format!("cannot start {}", exe.display()))?;
            ui.shutdown();
            if !status.success() {
                bail!("the window exited with {status}");
            }
        }
        Cmd::Autostart { action, print } => {
            use autostart::Action;
            let act = match action.as_str() {
                "enable" => Action::Enable,
                "disable" => Action::Disable,
                "status" => Action::Status,
                _ => bail!("use: avault autostart enable | disable | status"),
            };
            let dir = vault_dir(&cli, &env)?;
            let vid = Store::read_header(&dir)?.vault_id;
            if act == Action::Enable {
                let unattended = keyring_entry(&vid).and_then(|e| e.get_password().ok()).is_some() || load_device_credential(&env, &vid).is_some();
                if !unattended && !*print {
                    bail!("the engine can't ask for a passphrase at login. Run `avault remember` (or `avault join` on a new device) first.");
                }
            }
            std::fs::create_dir_all(&env.state_dir)?;
            let log = env.state_dir.join("daemon.log");
            println!("{}", autostart::apply(act, &dir, &log, *print)?);
        }
        Cmd::Daemon { args, quiet, full_every, no_watch, log: log_file } => {
            use vault_core::engine::{Engine, EngineConfig, Event};
            let store = open(&cli, &env)?;
            let o = options(args)?;
            let cfg = EngineConfig {
                components: o.components,
                maps: o.maps,
                redact: o.redact,
                block_secrets: o.block_secrets,
                quiet: std::time::Duration::from_secs(*quiet),
                full_sync_every: std::time::Duration::from_secs(*full_every),
                watch: !*no_watch,
            };
            let engine = Engine::start(env.clone(), std::sync::Arc::new(store), cfg)?;
            if !cli.json {
                println!("Engine running (Ctrl+C to stop). Syncs {}s after a session goes quiet.", quiet);
            }
            let mut last_progress = 0usize;
            let mut logger = log_file.as_ref().map(|p| {
                // Keep the unattended log from growing without bound.
                if std::fs::metadata(p).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
                    let _ = std::fs::remove_file(p);
                }
                std::fs::OpenOptions::new().create(true).append(true).open(p)
            });
            while let Ok(ev) = engine.events.recv() {
                if let Some(Ok(f)) = logger.as_mut() {
                    if !matches!(ev, Event::Progress { .. }) {
                        let _ = writeln!(f, "{}", serde_json::to_string(&ev)?);
                    }
                    continue;
                }
                if cli.json {
                    println!("{}", serde_json::to_string(&ev)?);
                    continue;
                }
                match ev {
                    Event::Status(s) => println!("[{:?}] {} pending", s.phase, s.pending_changes),
                    Event::Progress { phase, done, total } => {
                        if total > 0 && (done == total || done >= last_progress + 200 || done == 0) {
                            println!("  {phase}: {done}/{total}");
                        }
                        last_progress = if done == total { 0 } else { done };
                    }
                    Event::SyncFinished { full, report } => {
                        println!("{} sync done:", if full { "full" } else { "incremental" });
                        print_report(&report, false);
                    }
                    Event::Error { message } => eprintln!("  error (will retry): {message}"),
                    Event::Log { message } => println!("{message}"),
                }
            }
        }
        Cmd::Enroll { minutes } => {
            let store = open(&cli, &env)?;
            let code = store.create_enrollment(minutes * 60)?;
            if cli.json {
                println!("{}", serde_json::json!({ "code": code, "expires_in_minutes": minutes }));
            } else {
                println!("Enrollment code (valid {minutes} minutes, works once):\n\n    {code}\n\nOn the other device: avault join --vault \"{}\" --code {code}", store.root.display());
            }
        }
        Cmd::Join { code } => {
            let dir = vault_dir(&cli, &env)?;
            if !Store::exists(&dir) {
                bail!("no vault in {} - let the folder finish syncing first", dir.display());
            }
            let name = gethostname_string();
            let (store, cred) = Store::join(&dir, code, &name)?;
            let id = store.header.vault_id.clone();
            let stored_in_keyring = keyring::Entry::new("ai-session-vault-device", &id).and_then(|e| e.set_password(&cred)).is_ok();
            if !stored_in_keyring {
                std::fs::create_dir_all(&env.state_dir)?;
                std::fs::write(device_credential_path(&env, &id), &cred)?;
            }
            save_vault(&env, &dir)?;
            println!("Joined. This device can now open the vault without the passphrase{}.", if stored_in_keyring { " (credential in the OS credential store)" } else { " (credential in ~/.ai-vault; protect that folder)" });
            println!("Run `avault sync` to bring your sessions over.");
        }
        Cmd::Remember => {
            let store = open(&cli, &env)?;
            let pw = prompt_secret("Passphrase to remember (re-enter; not the recovery key): ")?;
            Store::open(&store.root, &pw)?;
            keyring_entry(&store.header.vault_id)
                .ok_or_else(|| anyhow!("no OS credential store available"))?
                .set_password(&pw)
                .map_err(|e| anyhow!("could not store the passphrase: {e}"))?;
            println!("Passphrase stored in your OS credential store for this vault. `avault forget` removes it.");
        }
        Cmd::Forget => {
            let dir = vault_dir(&cli, &env)?;
            let id = Store::read_header(&dir)?.vault_id;
            if let Some(e) = keyring_entry(&id) {
                let _ = e.delete_credential();
            }
            println!("Stored passphrase removed.");
        }
        Cmd::Overview => {
            let store = open(&cli, &env)?;
            let o = vault_core::overview::overview(&env, &store, &[])?;
            if cli.json {
                println!("{}", serde_json::to_string(&o)?);
            } else {
                println!("Vault {} ({} devices, {} projects, {} sessions, {:.1} MB)", o.vault_path, o.totals.devices, o.totals.projects, o.totals.sessions, o.totals.bytes as f64 / 1_048_576.0);
                for p in o.projects.iter().take(25) {
                    println!("  {:<48} {:>4} sessions  {}", p.portable_path.chars().take(48).collect::<String>(), p.sessions, if p.exists_locally { "" } else { "(folder not on this device)" });
                }
            }
        }
        Cmd::Verify { deep } => {
            let store = open(&cli, &env)?;
            let (ms, warns) = store.read_all_manifests()?;
            let r = store.verify(&ms, *deep);
            if cli.json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                for w in &warns {
                    println!("  ! {w}");
                }
                println!("Checked {} object(s){}.", r.objects, if *deep { " (decrypted and verified)" } else { " (presence only; use --deep to decrypt)" });
                for m in r.missing.iter().take(20) {
                    println!("  MISSING {m}");
                }
                for c in r.corrupt.iter().take(20) {
                    println!("  CORRUPT {c}");
                }
                if r.missing.is_empty() && r.corrupt.is_empty() && warns.is_empty() {
                    println!("Vault is healthy.");
                } else if !r.missing.is_empty() {
                    println!("Missing files usually mean the cloud folder is still syncing - wait, then run verify again. If they stay missing, run `avault sync` on the device that has them.");
                }
            }
            if !r.missing.is_empty() || !r.corrupt.is_empty() {
                std::process::exit(3);
            }
        }
        Cmd::Devices { remove, revoke } => {
            let store = open(&cli, &env)?;
            if let Some(id) = revoke {
                let slots = store.device_slots();
                let hits: Vec<_> = slots.iter().filter(|s| s.id.starts_with(id.as_str())).collect();
                match hits.as_slice() {
                    [s] => {
                        store.revoke_device_slot(&s.id)?;
                        println!("Revoked access for '{}'.", s.name);
                    }
                    [] => bail!("no joined device with id starting '{id}'"),
                    _ => bail!("several match; use more characters"),
                }
                return Ok(());
            }
            if let Some(prefix) = remove {
                let (ms, _) = store.read_all_manifests()?;
                let hits: Vec<_> = ms.iter().filter(|m| m.device.id.starts_with(prefix.as_str())).collect();
                match hits.as_slice() {
                    [m] => {
                        store.remove_device(&m.device.id)?;
                        println!("Forgot {} ({}). Run `avault gc` to free space.", m.device.name, m.device.id);
                    }
                    [] => bail!("no device id starts with '{prefix}'"),
                    _ => bail!("several devices match; use more characters"),
                }
                return Ok(());
            }
            let me = State::load(&env).device_id;
            let (ms, _) = store.read_all_manifests()?;
            if cli.json {
                println!("{}", serde_json::to_string(&ms.iter().map(|m| serde_json::json!({"id": m.device.id, "name": m.device.name, "os": m.device.os, "updated": m.updated, "files": m.entries.len(), "this_device": m.device.id == me})).collect::<Vec<_>>())?);
            }
            let slots = store.device_slots();
            if !cli.json && !slots.is_empty() {
                println!("Joined devices (passphrase-free access):");
                for sl in &slots {
                    println!("  {}  {}  joined {}", sl.id, sl.name, sl.created);
                }
                println!();
            }
            for m in ms.iter().filter(|_| !cli.json) {
                println!("{}  {} ({}){}  last sync {}  {} files", m.device.id, m.device.name, m.device.os, if m.device.id == me { "  <- this device" } else { "" }, m.updated, m.entries.len());
            }
        }
        Cmd::Gc => {
            let store = open(&cli, &env)?;
            let (ms, warns) = store.read_all_manifests()?;
            if !warns.is_empty() {
                bail!("not collecting: some device manifests are unreadable ({}). Let the folder finish syncing first.", warns.join("; "));
            }
            let (n, bytes) = store.gc(&ms)?;
            println!("Removed {n} unreferenced object(s), freed {:.1} MB.", bytes as f64 / 1_048_576.0);
        }
    }
    Ok(())
}
