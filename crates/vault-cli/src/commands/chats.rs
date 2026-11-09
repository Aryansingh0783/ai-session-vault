//! CLI commands: chats and the archive (import, search, context packs, exclusions, doctor).

use super::Ctx;
use crate::support::*;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use vault_core::archive;
use vault_core::doctor::{self, Level};

pub(crate) fn import_chats(cx: &Ctx, paths: &[PathBuf]) -> Result<()> {
    let env = cx.env;
    let inbox = env.state_dir.join("inbox");
    let list: Vec<PathBuf> = if paths.is_empty() {
        std::fs::create_dir_all(&inbox)?;
        std::fs::read_dir(&inbox)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "zip" || e == "json") || p.is_dir()).collect()
    } else {
        paths.to_vec()
    };
    if list.is_empty() {
        println!("Nothing to import. Pass export .zip files, or drop them in {}", inbox.display());
        return Ok(());
    }
    for p in list {
        match archive::load_export(&p).and_then(|c| archive::save_all(env, &c).map(|r| (c.len(), r))) {
            Ok((n, (added, updated))) => println!("  + {}: {n} conversations ({added} new, {updated} updated)", p.display()),
            Err(e) => println!("  x {}: {e}", p.display()),
        }
    }
    println!("Run `avault sync` to carry the archive to your other devices.");
    Ok(())
}

pub(crate) fn search_chats(cx: &Ctx, query: &str, limit: &usize) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let hits = archive::search(archive::load_all(env, true), query, *limit);
    if cli.json {
        println!("{}", serde_json::to_string(&hits.iter().map(|h| serde_json::json!({"id": h.conversation.id, "source": h.conversation.source, "title": h.conversation.title, "updated": h.conversation.updated, "project": h.conversation.project, "snippet": h.snippet})).collect::<Vec<_>>())?);
    } else if hits.is_empty() {
        println!("No matches.");
    }
    for h in hits.iter().filter(|_| !cli.json) {
        let c = &h.conversation;
        println!("[{}] {}  ({}, id {})\n    {}", c.source, c.title.chars().take(70).collect::<String>(), c.updated, c.id.chars().take(8).collect::<String>(), h.snippet);
    }
    Ok(())
}

pub(crate) fn context(cx: &Ctx, query: &str, pick: &Option<usize>, max_chars: &usize, out: &Option<PathBuf>) -> Result<()> {
    let env = cx.env;
    let hits = archive::search(archive::load_all(env, true), query, 25);
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
    Ok(())
}

pub(crate) fn exclude(cx: &Ctx, path: &Option<String>, remove: &bool) -> Result<()> {
    let env = cx.env;
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
    Ok(())
}

pub(crate) fn doctor(cx: &Ctx) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let f = doctor::run(env, vault_dir(cli, env).is_ok());
    if cli.json {
        println!("{}", serde_json::to_string(&f)?);
    }
    for x in f.iter().filter(|_| !cli.json) {
        let mark = match x.level {
            Level::Ok => "ok  ",
            Level::Warn => "WARN",
            Level::Info => "info",
        };
        println!("[{mark}] {}{}", x.title, if x.detail.is_empty() { String::new() } else { format!(" - {}", x.detail) });
    }
    Ok(())
}

pub(crate) fn import_v1(cx: &Ctx, data_dir: &Path) -> Result<()> {
    let cli = cx.cli;
    let env = cx.env;
    let store = open(cli, env)?;
    let r = vault_core::legacy::import_v1(&store, data_dir)?;
    println!("Imported {} file(s) from {} project(s). Run `avault sync` to distribute them.", r.files, r.projects);
    println!("Not imported (v1 encrypted fields inside them): {}. Sync those fresh from the device that has them.", r.skipped.join(", "));
    Ok(())
}
