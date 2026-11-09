//! CLI commands: the app (native window with a tray icon, or the browser).

use super::setup::CliSetup;
use super::Ctx;
use crate::cli::*;
use crate::support::*;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vault_core::env::Env;
use vault_core::store::Store;

pub(crate) fn open_ui(cx: &Ctx, args: &SyncArgs, port: &u16, no_open: &bool, read_only: &bool) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let store = std::sync::Arc::new(open(cli, env)?);
    let o = options(args)?;
    let engine =
        (!*read_only).then(|| vault_core::engine::EngineConfig { components: o.components, maps: o.maps, redact: o.redact, block_secrets: o.block_secrets, ..Default::default() });
    let ui = vault_ui::Ui::start(env.clone(), store, vault_ui::UiOptions { port: *port, engine })?;
    println!(
        "AI Session Vault is running at:

    {}

(Keep this window open. Ctrl+C quits.)",
        ui.url
    );
    if !*no_open {
        open_app_window(&ui.url);
    }
    ui.wait();
    Ok(())
}

fn window_exe() -> Result<PathBuf> {
    Ok(std::env::current_exe()?.with_file_name(if cfg!(windows) { "avault-window.exe" } else { "avault-window" }))
}

/// `avault` with no command (a double-click): hand over to the windowed app so no console stays open.
pub(crate) fn launch(cx: &Ctx) -> Result<()> {
    if let Ok(exe) = window_exe() {
        if exe.exists() {
            Command::new(exe).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().context("could not start the app window")?;
            return Ok(());
        }
    }
    open_app(cx, &SyncArgs::default(), false)
}

/// Only one copy of the app runs per user. The lock is held for the life of the process (and
/// released by the OS if it crashes). A second launch asks the running one to show its window.
fn single_instance(env: &Env) -> Result<Option<std::fs::File>> {
    std::fs::create_dir_all(&env.state_dir)?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(env.state_dir.join("app.lock"))?;
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

fn show_flag(env: &Env) -> PathBuf {
    env.state_dir.join("show.flag")
}

/// Start the window process on `url`. `None` if it is not installed or cannot start here (for
/// example the system webview is missing); the caller then uses the browser instead.
fn spawn_window(env: &Env, url: &str, hidden: bool, setup: bool) -> Option<Child> {
    let exe = window_exe().ok().filter(|e| e.exists())?;
    let mut c = Command::new(exe);
    c.arg(url).arg("--signal").arg(show_flag(env));
    if hidden {
        c.arg("--hidden");
    }
    if setup {
        c.arg("--quit-on-close");
    }
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = c.spawn().ok()?;
    // A window that cannot start (no webview) exits within moments; then the browser takes over.
    let until = Instant::now() + Duration::from_secs(4);
    while Instant::now() < until {
        if let Ok(Some(st)) = child.try_wait() {
            return if st.success() { Some(child) } else { None };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Some(child)
}

/// First run (or a vault that needs its passphrase): show the setup page until it is done.
fn run_setup(cx: &Ctx, folder: Option<PathBuf>, reason: Option<String>) -> Result<Option<Store>> {
    let env = cx.env;
    let backend = Arc::new(CliSetup::new(env.clone(), folder, reason));
    let setup = vault_ui::Setup::start(backend.clone(), 0)?;
    let mut child = spawn_window(env, &setup.url, false, true);
    if child.is_none() {
        open_app_window(&setup.url);
    }
    let finished = loop {
        if setup.wait_finished(Duration::from_millis(300)) {
            break true;
        }
        if let Some(c) = child.as_mut() {
            if c.try_wait()?.is_some() {
                break false; // the window was closed: the user quit
            }
        }
    };
    if let Some(mut c) = child {
        let _ = c.kill();
        let _ = c.wait();
    }
    setup.shutdown();
    if !finished {
        return Ok(None);
    }
    backend.take_store().map(Some).context("setup finished without a vault")
}

pub(crate) fn open_app(cx: &Ctx, args: &SyncArgs, hidden: bool) -> Result<()> {
    let (cli, env) = (cx.cli, cx.env);
    let Some(_running) = single_instance(env)? else {
        // Already running: ask it to come to the front.
        let _ = std::fs::write(show_flag(env), "show");
        return Ok(());
    };
    let store = match open_silent(cli, env)? {
        Opened::Ready(s) => s,
        Opened::Needs { folder, reason } => match run_setup(cx, folder, reason)? {
            Some(s) => s,
            None => return Ok(()),
        },
    };
    let o = options(args)?;
    let engine = vault_core::engine::EngineConfig { components: o.components, maps: o.maps, redact: o.redact, block_secrets: o.block_secrets, ..Default::default() };
    let ui = vault_ui::Ui::start(env.clone(), Arc::new(store), vault_ui::UiOptions { port: 0, engine: Some(engine) })?;
    // The window process shows the UI and the tray icon; closing the window hides it to the tray.
    // It exits on "Quit", and then the engine stops too.
    match spawn_window(env, &ui.url, hidden, false) {
        Some(mut child) => {
            let status = child.wait().context("the window stopped unexpectedly")?;
            ui.shutdown();
            if !status.success() {
                bail!("the window exited with {status}");
            }
        }
        None => {
            if !hidden {
                open_app_window(&ui.url);
            }
            ui.wait();
        }
    }
    Ok(())
}
