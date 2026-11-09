//! Find the local files that belong in the vault and where they map to.

use super::*;

pub(super) struct Item {
    pub(super) logical: String,
    pub(super) local: PathBuf,
    pub(super) kind: Kind,
}

pub(super) fn kind_of(p: &Path) -> Kind {
    if p.extension().is_some_and(|e| e == "jsonl") {
        Kind::Log
    } else {
        Kind::File
    }
}

pub(super) fn rel_posix(base: &Path, p: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

pub(super) fn walk_files(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !(e.file_type().is_dir() && matches!(e.file_name().to_str(), Some(".git") | Some("node_modules"))))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let n = e.file_name().to_string_lossy();
            !EXCLUDE.contains(&n.as_ref()) && !n.ends_with(".vaulttmp") && !n.ends_with(".desktop-released.json")
        })
        .map(|e| e.into_path())
        .collect()
}

/// Expand item patterns (`name`, `dir`, `prefix_*.ext`) under `base` into files.
pub(super) fn expand_items(base: &Path, names: &[&str]) -> Vec<PathBuf> {
    let mut out = vec![];
    for n in names {
        if n.contains('*') {
            if let Ok(rd) = fs::read_dir(base) {
                let mut hits: Vec<_> = rd.flatten().filter(|e| wildcard(n, &e.file_name().to_string_lossy())).map(|e| e.path()).collect();
                hits.sort();
                for h in hits {
                    if h.is_file() {
                        out.push(h)
                    } else if h.is_dir() {
                        out.extend(walk_files(&h))
                    }
                }
            }
            continue;
        }
        let p = base.join(n);
        if p.is_file() {
            if !EXCLUDE.contains(n) {
                out.push(p)
            }
        } else if p.is_dir() {
            out.extend(walk_files(&p));
        }
    }
    out
}

/// Find a Claude Code project's working directory from its logs (prefer the one matching the dir name).
pub(super) fn detect_cwd(pdir: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let mut files: Vec<_> = fs::read_dir(pdir).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect();
    files.sort_by_key(|p| std::cmp::Reverse(fs::metadata(p).map(|m| mtime_ms(&m)).unwrap_or(0)));
    let name = pdir.file_name()?.to_string_lossy().into_owned();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for f in files.into_iter().take(25) {
        let Ok(fh) = fs::File::open(&f) else { continue };
        for line in BufReader::new(fh).split(b'\n').take(300).flatten() {
            if !line.windows(5).any(|w| w == b"\"cwd\"") {
                continue;
            }
            if let Ok(Value::Object(o)) = serde_json::from_slice::<Value>(&line) {
                if let Some(Value::String(c)) = o.get("cwd") {
                    if encode_project(c) == name {
                        return Some(c.clone());
                    }
                    *seen.entry(c.clone()).or_default() += 1;
                }
            }
        }
    }
    seen.into_iter().max_by_key(|(_, n)| *n).map(|(c, _)| c)
}

/// Reject vault-supplied relative paths that could escape their folder.
pub(super) fn safe_rel(rel: &str) -> Option<PathBuf> {
    let p = PathBuf::from(rel);
    if rel.is_empty() || rel.contains(':') || rel.starts_with('/') || rel.starts_with('\\') {
        return None;
    }
    if p.components().any(|c| !matches!(c, PathComp::Normal(_))) || rel.split(['/', '\\']).any(|s| s == "..") {
        return None;
    }
    Some(p)
}

pub(super) fn scan_local(env: &Env, comps: &[Component], state: &mut State) -> (Vec<Item>, BTreeMap<String, String>) {
    let mut items = vec![];
    let mut projects = BTreeMap::new();
    let mut push = |logical: String, local: PathBuf| {
        let kind = kind_of(&local);
        items.push(Item { logical, local, kind });
    };
    if comps.contains(&Component::ClaudeCode) && env.claude_dir.is_dir() {
        for f in expand_items(&env.claude_dir, CLAUDE_ITEMS) {
            push(format!("claude-code/config/{}", rel_posix(&env.claude_dir, &f)), f);
        }
        let proot = env.claude_dir.join("projects");
        // Read fresh each run so exclusions added from the UI/CLI apply without a restart.
        let excluded: Vec<String> =
            crate::env::read_config(&env.state_dir).get("exclude_projects").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_else(|| env.exclude_projects.clone());
        if let Ok(rd) = fs::read_dir(&proot) {
            let mut dirs: Vec<_> = rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.path()).collect();
            dirs.sort();
            for pdir in dirs {
                let name = pdir.file_name().unwrap().to_string_lossy().into_owned();
                let cached = state.project_cwd.get(&name).cloned();
                let cwd = cached.or_else(|| {
                    let d = detect_cwd(&pdir);
                    if let Some(c) = &d {
                        state.project_cwd.insert(name.clone(), c.clone());
                    }
                    d
                });
                let (key, value) = match cwd {
                    Some(cwd) => {
                        let portable = to_portable(&cwd, &env.home);
                        (project_key(&portable), portable)
                    }
                    None => (format!("raw-{name}"), format!("raw:{name}")),
                };
                if excluded.iter().any(|x| crate::paths::prefix_match(&value, x).is_some()) {
                    continue;
                }
                projects.insert(key.clone(), value);
                for f in walk_files(&pdir) {
                    push(format!("claude-code/projects/{key}/{}", rel_posix(&pdir, &f)), f);
                }
            }
        }
        if env.claude_json.is_file() {
            push(MCP.into(), env.claude_json.clone());
        }
    }
    if comps.contains(&Component::ClaudeDesktop) && env.desktop_cfg.is_file() {
        push(DESKTOP.into(), env.desktop_cfg.clone());
    }
    if comps.contains(&Component::Codex) && env.codex_dir.is_dir() {
        for f in expand_items(&env.codex_dir, CODEX_ITEMS) {
            push(format!("codex/{}", rel_posix(&env.codex_dir, &f)), f);
        }
    }
    if comps.contains(&Component::Archive) {
        let ad = env.state_dir.join("archive");
        for f in walk_files(&ad) {
            push(format!("archive/{}", rel_posix(&ad, &f)), f);
        }
    }
    (items, projects)
}

pub(super) fn local_path_for(env: &Env, logical: &str, projects: &BTreeMap<String, String>, maps: &[PathMap], missing: &mut BTreeSet<String>) -> Option<PathBuf> {
    if logical == MCP {
        return Some(env.claude_json.clone());
    }
    if logical == DESKTOP {
        return Some(env.desktop_cfg.clone());
    }
    if let Some(rel) = logical.strip_prefix("claude-code/config/") {
        return Some(env.claude_dir.join(safe_rel(rel)?));
    }
    if let Some(rest) = logical.strip_prefix("claude-code/projects/") {
        let (key, rel) = rest.split_once('/')?;
        let value = projects.get(key)?;
        let enc = if let Some(raw) = value.strip_prefix("raw:") {
            encode_project(raw)
        } else {
            let local = to_local(value, &env.home, maps);
            if !Path::new(&local).is_dir() {
                missing.insert(local.clone());
            }
            encode_project(&local)
        };
        return Some(env.claude_dir.join("projects").join(enc).join(safe_rel(rel)?));
    }
    if let Some(rel) = logical.strip_prefix("archive/") {
        return Some(env.state_dir.join("archive").join(safe_rel(rel)?));
    }
    if let Some(rel) = logical.strip_prefix("codex/") {
        return Some(env.codex_dir.join(safe_rel(rel)?));
    }
    None
}
