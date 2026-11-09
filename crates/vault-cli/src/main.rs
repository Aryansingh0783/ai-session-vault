use anyhow::Result;
use clap::Parser;
use vault_core::env::Env;

mod autostart;
mod cli;
mod commands;
mod support;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = cli::Cli::parse();
    let env = Env::detect(cli.home.as_deref())?;
    commands::dispatch(&commands::Ctx { cli: &cli, env: &env })
}
