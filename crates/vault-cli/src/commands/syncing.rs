//! CLI commands: syncing (sync, backup, restore, daemon, autostart).

use super::Ctx;
use crate::cli::*;
use crate::support::*;
use anyhow::{bail, Result};
use std::io::Write;
use std::path::PathBuf;
use vault_core::env::State;
use vault_core::store::Store;
use vault_core::sync::{self};

pub(crate) fn run_sync(cx: &Ctx, a: &SyncArgs) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let log = |m: &str| println!("{m}");
    let store = open(cli, env)?;
    let opts = options(a)?;
    let mut state = State::load(env);
    let _lock = sync::lock(env)?;
    let rep = match &cli.cmd {
        Cmd::Sync(_) => {
            let r = sync::restore(env, &store, &mut state, &opts, &log)?;
            let mut r = r;
            r.absorb(sync::backup(env, &store, &mut state, &opts, &log)?);
            r
        }
        Cmd::Backup(_) => sync::backup(env, &store, &mut state, &opts, &log)?,
        _ => sync::restore(env, &store, &mut state, &opts, &log)?,
    };
    print_report(&rep, cli.json);
    if !rep.failed.is_empty() {
        std::process::exit(2);
    }
    Ok(())
}

pub(crate) fn autostart(cx: &Ctx, action: &str, print: &bool) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    use crate::autostart::{self, Action};
    let act = match action {
        "enable" => Action::Enable,
        "disable" => Action::Disable,
        "status" => Action::Status,
        _ => bail!("use: avault autostart enable | disable | status"),
    };
    let dir = vault_dir(cli, env)?;
    let vid = Store::read_header(&dir)?.vault_id;
    if act == Action::Enable {
        let unattended = keyring_entry(&vid).and_then(|e| e.get_password().ok()).is_some() || load_device_credential(env, &vid).is_some();
        if !unattended && !*print {
            bail!("the engine can't ask for a passphrase at login. Run `avault remember` (or `avault join` on a new device) first.");
        }
    }
    std::fs::create_dir_all(&env.state_dir)?;
    let log = env.state_dir.join("daemon.log");
    println!("{}", autostart::apply(act, &dir, &log, *print)?);
    Ok(())
}

pub(crate) fn daemon(cx: &Ctx, args: &SyncArgs, quiet: &u64, full_every: &u64, no_watch: &bool, log_file: &Option<PathBuf>) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    use vault_core::engine::{Engine, EngineConfig, Event};
    let store = open(cli, env)?;
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
    Ok(())
}
