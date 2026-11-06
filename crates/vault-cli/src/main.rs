use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
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
    /// Check for the common problems (retention purge, leaked secrets, running tools).
    Doctor,
    /// Change the vault passphrase (no data is re-encrypted).
    Passwd,
    /// Issue a new recovery key (the old one stops working).
    Recovery,
    /// Remove vault objects no device references any more.
    Gc,
    /// Bring a vault made by the Python version (its `data` folder) into this one.
    ImportV1 { data_dir: PathBuf },
    /// Keep syncing in the background, waiting for sessions to go idle first.
    Watch {
        #[command(flatten)]
        args: SyncArgs,
        /// Seconds between syncs.
        #[arg(long, default_value_t = 300)]
        interval: u64,
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
    std::fs::create_dir_all(&env.state_dir)?;
    std::fs::write(config_path(env), serde_json::json!({ "vault": p }).to_string())?;
    Ok(())
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
    let secret = prompt_secret("Vault passphrase (or recovery key): ")?;
    let store = Store::open(&dir, &secret)?;
    save_vault(env, &dir)?;
    Ok(store)
}

fn options(a: &SyncArgs) -> Result<Options> {
    let components = match &a.only {
        Some(s) => s.split(',').map(|c| Component::parse(c.trim())).collect::<Result<Vec<_>>>()?,
        None => Component::ALL.to_vec(),
    };
    Ok(Options { components, maps: parse_maps(&a.maps)?, dry_run: a.dry_run, redact: a.redact })
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
    let sections: [(&str, &Vec<String>); 7] = [
        ("Conflicts (kept the newer version)", &r.conflicts),
        ("Files that couldn't be written", &r.failed),
        ("In use by an app, skipped (quit it and run again)", &r.busy),
        ("MCP servers added", &r.mcp_added),
        ("Project folders missing on this device (sessions restored anyway; clone them or use --map OLD=NEW)", &r.missing_projects),
        ("Skipped, over 512 MB", &r.skipped_large),
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

/// Was any session log written in the last `secs` seconds? (A live session: don't sync mid-write.)
fn recently_active(env: &Env, secs: u64) -> bool {
    let now = std::time::SystemTime::now();
    [env.claude_dir.join("projects"), env.codex_dir.join("sessions")].iter().any(|root| {
        walkdir::WalkDir::new(root).into_iter().flatten().filter(|e| e.file_type().is_file()).any(|e| {
            e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| now.duration_since(t).ok()).map_or(false, |d| d.as_secs() < secs)
        })
    })
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
            if ch.stdin.take().map_or(false, |mut s| s.write_all(&data).is_ok()) && ch.wait().map_or(false, |s| s.success()) {
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

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let env = Env::detect(None)?;
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
                std::fs::read_dir(&inbox)?.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |e| e == "zip" || e == "json") || p.is_dir()).collect()
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
        Cmd::Watch { args, interval } => {
            let store = open(&cli, &env)?;
            let opts = options(args)?;
            let mut state = State::load(&env);
            println!("Watching. Syncing every {interval}s when sessions are idle. Ctrl+C to stop.");
            loop {
                if recently_active(&env, 15) {
                    println!("  sessions active, waiting...");
                    std::thread::sleep(std::time::Duration::from_secs(15));
                    continue;
                }
                let _lock = sync::lock(&env)?;
                match sync::sync_unlocked(&env, &store, &mut state, &opts, &log) {
                    Ok(r) => print_report(&r, cli.json),
                    Err(e) => eprintln!("  sync failed: {e:#}"),
                }
                drop(_lock);
                std::thread::sleep(std::time::Duration::from_secs(*interval));
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
