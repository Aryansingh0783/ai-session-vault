//! Import a vault made by the Python version (v1) into a v2 vault.
//!
//! Brings over Claude Code and Codex session logs and the archived chats. Config files that v1
//! stored with encrypted fields (settings.json, config.toml, MCP servers) are skipped: re-run a
//! normal `avault sync` on the device that has them and they are picked up fresh.

use crate::store::{Entry, Kind, Store};
use crate::util::{mtime_ms, now_iso};
use anyhow::{bail, Result};
use serde_json::Value;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

#[derive(Default, Debug)]
pub struct ImportReport {
    pub files: usize,
    pub projects: usize,
    pub skipped: Vec<String>,
}

pub fn import_v1(store: &Store, v1_data: &Path) -> Result<ImportReport> {
    if !v1_data.join("vault_key.json").is_file() && !v1_data.join("claude-code").is_dir() && !v1_data.join("codex").is_dir() {
        bail!("{} doesn't look like a v1 vault `data` folder", v1_data.display());
    }
    let dev_id = format!("v1-{}", &blake3::hash(v1_data.to_string_lossy().as_bytes()).to_hex()[..10]);
    let mut m = store.read_manifest(&dev_id)?.unwrap_or_else(|| {
        let mut m = Store::new_manifest(Default::default());
        m.device.id = dev_id.clone();
        m.device.name = "imported from Python vault".into();
        m.device.os = "legacy".into();
        m
    });
    let mut rep = ImportReport::default();
    let add = |m: &mut crate::store::Manifest, logical: String, path: &Path, rep: &mut ImportReport| -> Result<()> {
        let data = fs::read(path)?;
        let id = store.put(&data)?;
        let kind = if path.extension().is_some_and(|e| e == "jsonl") { Kind::Log } else { Kind::File };
        let mt = fs::metadata(path).map(|x| mtime_ms(&x)).unwrap_or(0);
        m.entries.insert(logical, Entry { kind, size: data.len() as u64, mtime_ms: mt, hash: id, chunks: vec![] });
        rep.files += 1;
        Ok(())
    };

    let proot = v1_data.join("claude-code/projects");
    if proot.is_dir() {
        for pd in fs::read_dir(&proot)?.flatten().filter(|e| e.path().is_dir()) {
            let meta: Option<Value> = fs::read(pd.path().join("_vault_meta.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
            let portable = meta.as_ref().and_then(|v| v.get("portable_path")).and_then(|p| p.as_str()).map(String::from);
            let (key, value) = match portable {
                Some(p) => (crate::paths::project_key(&p), p),
                None => {
                    let enc = meta.as_ref().and_then(|v| v.get("encoded_name")).and_then(|p| p.as_str()).map(String::from).unwrap_or_else(|| pd.file_name().to_string_lossy().trim_start_matches("raw-").to_string());
                    (format!("raw-{enc}"), format!("raw:{enc}"))
                }
            };
            m.projects.insert(key.clone(), value);
            rep.projects += 1;
            for f in WalkDir::new(pd.path()).into_iter().flatten().filter(|e| e.file_type().is_file()) {
                let name = f.file_name().to_string_lossy();
                if name == "_vault_meta.json" || name.ends_with(".vaulttmp") {
                    continue;
                }
                let rel = f.path().strip_prefix(pd.path())?.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
                add(&mut m, format!("claude-code/projects/{key}/{rel}"), f.path(), &mut rep)?;
            }
        }
    }
    // v1 stored these byte-for-byte (not secret-bearing): Claude config folders, codex sessions, archive DBs.
    let plain: [(&str, &str, &[&str]); 2] = [
        ("claude-code/config", "claude-code/config", &["CLAUDE.md", "rules", "commands", "agents", "skills", "output-styles"]),
        ("codex", "codex", &["AGENTS.md", "prompts", "sessions", "archived_sessions", "session_index.jsonl", "memories", "skills"]),
    ];
    for (vdir, logical_prefix, names) in plain {
        for n in names {
            let p = v1_data.join(vdir).join(n);
            let files: Vec<_> = if p.is_file() { vec![p.clone()] } else { WalkDir::new(&p).into_iter().flatten().filter(|e| e.file_type().is_file()).map(|e| e.into_path()).collect() };
            for f in files {
                let rel = f.strip_prefix(v1_data.join(vdir))?.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
                add(&mut m, format!("{logical_prefix}/{rel}"), &f, &mut rep)?;
            }
        }
    }
    for skipped in ["claude-code/config/settings.json", "codex/config.toml", "claude-code/mcp_servers.json"] {
        rep.skipped.push(skipped.into());
    }
    m.seq += 1;
    m.updated = now_iso();
    store.write_manifest(&m)?;
    Ok(rep)
}
