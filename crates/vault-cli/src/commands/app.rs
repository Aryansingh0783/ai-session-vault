//! CLI commands: the app (browser window and native window).

use super::Ctx;
use crate::cli::*;
use crate::support::*;
use anyhow::{bail, Context, Result};

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

pub(crate) fn open_app(cx: &Ctx, args: &SyncArgs) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let store = std::sync::Arc::new(open(cli, env)?);
    let o = options(args)?;
    let engine = vault_core::engine::EngineConfig { components: o.components, maps: o.maps, redact: o.redact, block_secrets: o.block_secrets, ..Default::default() };
    let ui = vault_ui::Ui::start(env.clone(), store, vault_ui::UiOptions { port: 0, engine: Some(engine) })?;
    let exe = std::env::current_exe()?.with_file_name(if cfg!(windows) { "avault-window.exe" } else { "avault-window" });
    if !exe.exists() {
        eprintln!("avault-window was not found next to avault ({}). Opening in your browser instead.", exe.display());
        println!(
            "AI Session Vault is running at:

    {}

(Keep this window open. Ctrl+C quits.)",
            ui.url
        );
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
    Ok(())
}
