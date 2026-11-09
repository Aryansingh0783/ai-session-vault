//! Live sources: Claude Code sessions on this device.

use super::*;

// ---------------------------------------------------------------- fast, on-demand search
#[derive(Clone, Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    /// "claude-code" | "claude" | "chatgpt"
    pub source: String,
    pub id: String,
    pub mtime_ms: i64,
}

/// Every conversation file on this device (Claude Code sessions + archived chats), newest first.
pub fn list_sources(env: &Env) -> Vec<SourceFile> {
    let mut out = vec![];
    let proot = env.claude_dir.join("projects");
    for e in walkdir::WalkDir::new(&proot).max_depth(2).into_iter().flatten() {
        let p = e.path();
        if e.file_type().is_file() && p.extension().is_some_and(|x| x == "jsonl") {
            if let (Some(stem), Ok(md)) = (p.file_stem(), e.metadata()) {
                out.push(SourceFile { path: p.to_path_buf(), source: "claude-code".into(), id: stem.to_string_lossy().into_owned(), mtime_ms: crate::util::mtime_ms(&md) });
            }
        }
    }
    for e in walkdir::WalkDir::new(archive_dir(env)).max_depth(2).into_iter().flatten() {
        let p = e.path();
        if e.file_type().is_file() && p.extension().is_some_and(|x| x == "json") {
            let source = p.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let (Some(stem), Ok(md)) = (p.file_stem(), e.metadata()) {
                out.push(SourceFile { path: p.to_path_buf(), source, id: stem.to_string_lossy().into_owned(), mtime_ms: crate::util::mtime_ms(&md) });
            }
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.mtime_ms));
    out
}

pub fn load_source(f: &SourceFile) -> Option<Conversation> {
    if f.source == "claude-code" {
        parse_cc_session(&f.path)
    } else {
        serde_json::from_slice(&fs::read(&f.path).ok()?).ok()
    }
}

pub fn recent(env: &Env, limit: usize) -> Vec<Conversation> {
    list_sources(env).iter().filter_map(load_source).take(limit).collect()
}

pub fn find_conversation(env: &Env, source: &str, id: &str) -> Option<Conversation> {
    list_sources(env).into_iter().find(|f| f.source == source && f.id == id).and_then(|f| load_source(&f))
}

/// Search without building an index: scan files for the words (case-insensitive), then parse only the
/// files that contain all of them. Stops after `limit` hits.
pub fn search_fast(env: &Env, query: &str, limit: usize) -> Vec<Hit> {
    let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
    if words.is_empty() {
        return recent(env, limit).into_iter().map(|c| Hit { snippet: String::new(), score: 0, conversation: c }).collect();
    }
    let mut hits = vec![];
    for f in list_sources(env) {
        if hits.len() >= limit {
            break;
        }
        if fs::metadata(&f.path).map(|m| m.len() > 100 * 1024 * 1024).unwrap_or(true) {
            continue;
        }
        let Ok(raw) = fs::read(&f.path) else { continue };
        let lower = raw.to_ascii_lowercase();
        if !words.iter().all(|w| memchr::memmem::find(&lower, w.as_bytes()).is_some()) {
            continue;
        }
        if let Some(c) = load_source(&f) {
            hits.extend(search(vec![c], query, 1));
        }
    }
    hits
}
