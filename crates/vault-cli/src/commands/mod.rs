//! Command dispatch. Each command is a function in one of the modules below.

mod access;
mod app;
mod chats;
mod setup;
mod syncing;
mod vault;

use crate::cli::{Cli, Cmd};
use anyhow::Result;
use vault_core::env::Env;

/// What every command needs: the parsed command line and the detected environment.
pub(crate) struct Ctx<'a> {
    pub(crate) cli: &'a Cli,
    pub(crate) env: &'a Env,
}

pub(crate) fn dispatch(cx: &Ctx) -> Result<()> {
    let Some(cmd) = &cx.cli.cmd else { return app::launch(cx) };
    match cmd {
        Cmd::Init => vault::init(cx),
        Cmd::Sync(a) | Cmd::Backup(a) | Cmd::Restore(a) => syncing::run_sync(cx, a),
        Cmd::Status => vault::status(cx),
        Cmd::Import { paths } => chats::import_chats(cx, paths),
        Cmd::Search { query, limit } => chats::search_chats(cx, query, limit),
        Cmd::Context { query, pick, max_chars, out } => chats::context(cx, query, pick, max_chars, out),
        Cmd::Exclude { path, remove } => chats::exclude(cx, path, remove),
        Cmd::Doctor => chats::doctor(cx),
        Cmd::Passwd => vault::passwd(cx),
        Cmd::Recovery => vault::recovery(cx),
        Cmd::ImportV1 { data_dir } => chats::import_v1(cx, data_dir),
        Cmd::Ui { args, port, no_open, read_only } => app::open_ui(cx, args, port, no_open, read_only),
        Cmd::App { args, hidden } => app::open_app(cx, args, *hidden),
        Cmd::Autostart { action, print } => syncing::autostart(cx, action, print),
        Cmd::Daemon { args, quiet, full_every, no_watch, log: log_file } => syncing::daemon(cx, args, quiet, full_every, no_watch, log_file),
        Cmd::Enroll { minutes } => access::enroll(cx, minutes),
        Cmd::Join { code } => access::join(cx, code),
        Cmd::Remember => access::remember(cx),
        Cmd::Forget => access::forget(cx),
        Cmd::Overview => vault::overview(cx),
        Cmd::Verify { deep } => vault::verify(cx, deep),
        Cmd::Devices { remove, revoke } => access::devices(cx, remove, revoke),
        Cmd::Gc => vault::gc(cx),
    }
}
