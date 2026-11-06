//! Offline archive of claude.ai / ChatGPT data exports, cross-source search, and context packs.
//! Each conversation is stored as its own small file under `~/.ai-vault/archive/<source>/<id>.json`,
//! which the sync engine carries (encrypted) to your other devices like any other file.

use crate::env::Env;
use crate::util::atomic_write;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Message {
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub time: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Conversation {
    pub id: String,
    /// "claude" | "chatgpt" | "claude-code"
    pub source: String,
    pub title: String,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub project: String,
    pub messages: Vec<Message>,
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str())
}

/// Tolerant timestamp formatter: accepts unix seconds (int/float) or RFC 3339; anything else -> "".
pub fn fmt_ts(v: Option<&Value>) -> String {
    let Some(v) = v else { return String::new() };
    let dt = if let Some(f) = v.as_f64() {
        OffsetDateTime::from_unix_timestamp(f as i64).ok()
    } else if let Some(t) = v.as_str() {
        OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339).ok()
    } else {
        None
    };
    match dt {
        Some(d) => format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year(), d.month() as u8, d.day(), d.hour(), d.minute()),
        None => v.as_str().map(|t| t.chars().take(16).collect()).unwrap_or_default(),
    }
}

fn gpt_text(content: &Value) -> String {
    let ctype = s(content, "content_type").unwrap_or("");
    if matches!(ctype, "thoughts" | "reasoning_recap" | "model_editable_context" | "user_editable_context") {
        return String::new();
    }
    let mut out: Vec<String> = vec![];
    if let Some(parts) = content.get("parts").and_then(|p| p.as_array()) {
        for p in parts {
            if let Some(t) = p.as_str() {
                out.push(t.to_string());
            } else if let Some(t) = s(p, "text") {
                out.push(t.to_string());
            } else if s(p, "content_type").unwrap_or("").contains("image") {
                out.push("[image]".into());
            }
        }
    }
    if out.is_empty() {
        if let Some(t) = s(content, "text") {
            out.push(t.to_string());
        }
    }
    out.join("\n").trim().to_string()
}

pub fn parse_chatgpt(c: &Value) -> Conversation {
    let empty = serde_json::Map::new();
    let mapping = c.get("mapping").and_then(|m| m.as_object()).unwrap_or(&empty);
    let mut chain: Vec<&Value> = vec![];
    let mut node = s(c, "current_node").map(String::from);
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = node {
        if !seen.insert(id.clone()) {
            break;
        }
        let Some(n) = mapping.get(&id) else { break };
        chain.push(n);
        node = s(n, "parent").map(String::from);
    }
    chain.reverse();
    if chain.is_empty() {
        let mut all: Vec<&Value> = mapping.values().collect();
        all.sort_by(|a, b| {
            let t = |n: &Value| n.get("message").and_then(|m| m.get("create_time")).and_then(|t| t.as_f64()).unwrap_or(0.0);
            t(a).partial_cmp(&t(b)).unwrap_or(std::cmp::Ordering::Equal)
        });
        chain = all;
    }
    let mut messages = vec![];
    for n in chain {
        let Some(m) = n.get("message").filter(|m| m.is_object()) else { continue };
        let role = m.get("author").and_then(|a| s(a, "role")).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        if m.get("metadata").and_then(|x| x.get("is_visually_hidden_from_conversation")).and_then(|b| b.as_bool()) == Some(true) {
            continue;
        }
        if let Some(r) = m.get("recipient").and_then(|r| r.as_str()) {
            if r != "all" {
                continue;
            }
        }
        let text = gpt_text(m.get("content").unwrap_or(&Value::Null));
        if !text.is_empty() {
            messages.push(Message { role: role.into(), text, time: fmt_ts(m.get("create_time")) });
        }
    }
    let id = s(c, "conversation_id").or_else(|| s(c, "id")).map(String::from).unwrap_or_else(|| blake3::hash(c.to_string().as_bytes()).to_hex()[..16].to_string());
    Conversation {
        id,
        source: "chatgpt".into(),
        title: s(c, "title").unwrap_or("Untitled").into(),
        created: fmt_ts(c.get("create_time")),
        updated: fmt_ts(c.get("update_time").or_else(|| c.get("create_time"))),
        project: String::new(),
        messages,
    }
}

pub fn parse_claude(c: &Value) -> Conversation {
    let mut messages = vec![];
    for m in c.get("chat_messages").and_then(|m| m.as_array()).into_iter().flatten() {
        let role = if s(m, "sender") == Some("human") { "user" } else { "assistant" };
        let parts: Vec<&str> = m.get("content").and_then(|c| c.as_array()).into_iter().flatten()
            .filter(|b| s(b, "type") == Some("text")).filter_map(|b| s(b, "text")).collect();
        let mut text = parts.join("\n\n").trim().to_string();
        if text.is_empty() {
            text = s(m, "text").unwrap_or("").trim().to_string();
        }
        let names: Vec<&str> = ["attachments", "files"].iter()
            .flat_map(|k| m.get(*k).and_then(|a| a.as_array()).into_iter().flatten())
            .filter_map(|a| s(a, "file_name")).collect();
        if !names.is_empty() {
            if !text.is_empty() { text.push_str("\n\n"); }
            text.push_str(&format!("[attached: {}]", names.join(", ")));
        }
        if !text.is_empty() {
            messages.push(Message { role: role.into(), text, time: fmt_ts(m.get("created_at")) });
        }
    }
    Conversation {
        id: s(c, "uuid").or_else(|| s(c, "id")).unwrap_or("unknown").into(),
        source: "claude".into(),
        title: s(c, "name").filter(|n| !n.is_empty()).unwrap_or("Untitled").into(),
        created: fmt_ts(c.get("created_at")),
        updated: fmt_ts(c.get("updated_at").or_else(|| c.get("created_at"))),
        project: String::new(),
        messages,
    }
}

fn is_conv_file(name: &str) -> bool {
    let n = name.rsplit(['/', '\\']).next().unwrap_or(name);
    n.starts_with("conversations") && n.ends_with(".json")
}

/// Load a ChatGPT/claude.ai export (zip, folder or conversations.json). Handles sharded
/// `conversations-000.json` files and null fields.
pub fn load_export(p: &Path) -> Result<Vec<Conversation>> {
    let mut raws: Vec<Vec<u8>> = vec![];
    if p.is_dir() {
        for e in walkdir::WalkDir::new(p).into_iter().flatten().filter(|e| e.file_type().is_file()) {
            if is_conv_file(&e.path().to_string_lossy()) {
                raws.push(fs::read(e.path())?);
            }
        }
    } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        let mut z = zip::ZipArchive::new(fs::File::open(p)?)?;
        for i in 0..z.len() {
            let mut f = z.by_index(i)?;
            if is_conv_file(f.name()) {
                let mut b = vec![];
                f.read_to_end(&mut b)?;
                raws.push(b);
            }
        }
    } else {
        raws.push(fs::read(p)?);
    }
    if raws.is_empty() {
        bail!("no conversations.json found (is this a Claude or ChatGPT data export?)");
    }
    let mut convs = vec![];
    for raw in raws {
        let v: Value = serde_json::from_slice(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw))?;
        let list = match v {
            Value::Array(a) => a,
            Value::Object(ref o) => o.get("conversations").and_then(|c| c.as_array()).cloned().unwrap_or_else(|| vec![v.clone()]),
            _ => vec![],
        };
        for c in list.iter().filter(|c| c.is_object()) {
            if c.get("mapping").is_some() {
                convs.push(parse_chatgpt(c));
            } else if c.get("chat_messages").is_some() {
                convs.push(parse_claude(c));
            }
        }
    }
    if convs.is_empty() {
        bail!("unrecognized export format");
    }
    Ok(convs)
}

pub fn archive_dir(env: &Env) -> PathBuf {
    env.state_dir.join("archive")
}

fn conv_path(env: &Env, c: &Conversation) -> PathBuf {
    let safe: String = c.id.chars().map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '_' }).take(80).collect();
    archive_dir(env).join(&c.source).join(format!("{safe}.json"))
}

/// Add conversations to the archive. Returns `(added, updated)`.
pub fn save_all(env: &Env, convs: &[Conversation]) -> Result<(usize, usize)> {
    let (mut added, mut updated) = (0, 0);
    for c in convs {
        let p = conv_path(env, c);
        match fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Conversation>(&b).ok()) {
            None => added += 1,
            Some(old) if old == *c || old.updated > c.updated => continue,
            Some(_) => updated += 1,
        }
        atomic_write(&p, &serde_json::to_vec(c)?)?;
    }
    Ok((added, updated))
}

/// Archive conversations + Claude Code sessions found on this device.
pub fn load_all(env: &Env, with_sessions: bool) -> Vec<Conversation> {
    let mut out: Vec<Conversation> = vec![];
    for e in walkdir::WalkDir::new(archive_dir(env)).into_iter().flatten().filter(|e| e.file_type().is_file()) {
        if let Some(c) = fs::read(e.path()).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            out.push(c);
        }
    }
    if with_sessions {
        let proot = env.claude_dir.join("projects");
        for e in walkdir::WalkDir::new(&proot).max_depth(2).into_iter().flatten() {
            if e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl") {
                if let Some(c) = parse_cc_session(e.path()) {
                    out.push(c);
                }
            }
        }
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    out
}

/// Parse a Claude Code session log into a readable conversation.
pub fn parse_cc_session(path: &Path) -> Option<Conversation> {
    let raw = fs::read(path).ok()?;
    let md = fs::metadata(path).ok()?;
    let mut msgs: Vec<Message> = vec![];
    let mut project = String::new();
    for line in raw.split(|b| *b == b'\n') {
        let Ok(o) = serde_json::from_slice::<Value>(line) else { continue };
        let ty = s(&o, "type").unwrap_or("");
        if ty != "user" && ty != "assistant" {
            continue;
        }
        if o.get("isSidechain").and_then(|b| b.as_bool()) == Some(true) || o.get("isMeta").and_then(|b| b.as_bool()) == Some(true) {
            continue;
        }
        if project.is_empty() {
            project = s(&o, "cwd").unwrap_or("").to_string();
        }
        let m = o.get("message").unwrap_or(&Value::Null);
        let role = s(m, "role").unwrap_or(ty).to_string();
        let mut parts: Vec<String> = vec![];
        match m.get("content") {
            Some(Value::String(t)) => parts.push(t.clone()),
            Some(Value::Array(a)) => {
                for b in a {
                    match s(b, "type") {
                        Some("text") => parts.extend(s(b, "text").map(String::from)),
                        Some("tool_use") => parts.push(format!("[used tool: {}]", s(b, "name").unwrap_or("?"))),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        let text = parts.join("\n").trim().to_string();
        if text.is_empty() || text.starts_with("<command-") || text.starts_with("<local-command-") || text.starts_with("<system-reminder>") {
            continue;
        }
        match msgs.last_mut() {
            Some(last) if last.role == role => {
                last.text.push('\n');
                last.text.push_str(&text);
            }
            _ => msgs.push(Message { role, text, time: fmt_ts(o.get("timestamp")) }),
        }
    }
    if msgs.is_empty() {
        return None;
    }
    let title: String = msgs.iter().find(|m| m.role == "user").map(|m| m.text.lines().next().unwrap_or("").chars().take(80).collect()).unwrap_or_default();
    let updated = md.modified().ok().map(OffsetDateTime::from).map(|d| format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year(), d.month() as u8, d.day(), d.hour(), d.minute())).unwrap_or_default();
    Some(Conversation {
        id: path.file_stem()?.to_string_lossy().into_owned(),
        source: "claude-code".into(),
        title,
        created: msgs.first().map(|m| m.time.clone()).unwrap_or_default(),
        updated,
        project,
        messages: msgs,
    })
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub conversation: Conversation,
    pub score: u32,
    pub snippet: String,
}

/// All words must appear (case-insensitive). Title hits rank above body hits.
pub fn search(convs: Vec<Conversation>, query: &str, limit: usize) -> Vec<Hit> {
    let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
    let mut hits: Vec<Hit> = vec![];
    for c in convs {
        let title = c.title.to_lowercase();
        let project = c.project.to_lowercase();
        let body: Vec<String> = c.messages.iter().map(|m| m.text.to_lowercase()).collect();
        let ok = words.iter().all(|w| title.contains(w) || project.contains(w) || body.iter().any(|b| b.contains(w)));
        if !ok {
            continue;
        }
        let mut score = 1;
        for w in &words {
            if title.contains(w) { score += 10 }
            if project.contains(w) { score += 4 }
        }
        let snippet = words.first().and_then(|w| {
            body.iter().enumerate().find_map(|(i, b)| b.find(w.as_str()).map(|pos| {
                let t = &c.messages[i].text;
                let start = t.char_indices().map(|(x, _)| x).find(|x| *x + 60 >= pos).unwrap_or(0);
                t[start..].chars().take(140).collect::<String>().replace('\n', " ")
            }))
        }).unwrap_or_default();
        hits.push(Hit { conversation: c, score, snippet });
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(b.conversation.updated.cmp(&a.conversation.updated)));
    hits.truncate(limit);
    hits
}

/// A paste-ready transcript for continuing a chat in another assistant. Keeps the first message and
/// the most recent ones when it must be shortened.
pub fn build_pack(c: &Conversation, max_chars: usize) -> String {
    let label = match c.source.as_str() {
        "claude" => "Claude.ai chat",
        "chatgpt" => "ChatGPT chat",
        _ => "Claude Code session",
    };
    let header = format!(
        "# Context pack: {}\n\n_Source: {label}{} · last updated {} · {} messages_\n\n> **Note to the assistant:** Below is the transcript of an earlier conversation I had with an AI assistant on another device. Treat it as shared context and continue from where it left off. Don't summarize it back to me unless I ask.\n\n---\n\n",
        c.title,
        if c.project.is_empty() { String::new() } else { format!(" in {}", c.project) },
        c.updated,
        c.messages.len()
    );
    let blocks: Vec<String> = c.messages.iter().map(|m| {
        format!("**{}**{}:\n\n{}\n\n", if m.role == "user" { "User" } else { "Assistant" }, if m.time.is_empty() { String::new() } else { format!(" ({})", m.time) }, m.text)
    }).collect();
    let budget = max_chars.saturating_sub(header.chars().count()).max(2000);
    let total: usize = blocks.iter().map(|b| b.chars().count()).sum();
    let body = if total <= budget {
        blocks.concat()
    } else {
        let first: String = blocks[0].chars().take(budget / 4).collect();
        let mut rem = budget.saturating_sub(first.chars().count() + 120);
        let mut tail: Vec<&String> = vec![];
        for b in blocks[1..].iter().rev() {
            let n = b.chars().count();
            if n > rem { break }
            rem -= n;
            tail.push(b);
        }
        tail.reverse();
        let omitted = blocks.len() - 1 - tail.len();
        format!("{first}\n_[... {omitted} earlier message(s) omitted for length ...]_\n\n{}", tail.iter().map(|s| s.as_str()).collect::<String>())
    };
    format!("{header}{body}---\n\n_End of prior transcript. My next message follows._\n")
}

pub fn to_markdown(c: &Conversation) -> String {
    let mut out = format!("# {}\n\n_{} · created {} · updated {} · id {}_\n\n", c.title, c.source, c.created, c.updated, c.id);
    for m in &c.messages {
        out.push_str(&format!("## {}{}\n\n{}\n\n", if m.role == "user" { "User" } else { "Assistant" }, if m.time.is_empty() { String::new() } else { format!(" · {}", m.time) }, m.text));
    }
    out
}

pub fn find_by_id(convs: Vec<Conversation>, id: &str) -> Result<Conversation> {
    let id = id.to_lowercase();
    let mut m: Vec<Conversation> = convs.into_iter().filter(|c| c.id.to_lowercase().starts_with(&id)).collect();
    match m.len() {
        0 => Err(anyhow!("no conversation with id starting '{id}'")),
        1 => Ok(m.remove(0)),
        n => Err(anyhow!("{n} conversations match '{id}'; use more characters")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_chatgpt_branch_following_current_node() {
        let c = json!({"title":"T","conversation_id":"abc","current_node":"c","create_time":1700000000.0,
            "mapping":{
              "a":{"parent":null,"message":{"author":{"role":"user"},"content":{"content_type":"text","parts":["hi"]}}},
              "b":{"parent":"a","message":{"author":{"role":"assistant"},"content":{"content_type":"text","parts":["old branch"]}}},
              "c":{"parent":"a","message":{"author":{"role":"assistant"},"content":{"content_type":"text","parts":["hello"]}}}}});
        let conv = parse_chatgpt(&c);
        assert_eq!(conv.messages.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), ["hi", "hello"]);
        assert_eq!(conv.created, "2023-11-14 22:13");
    }

    #[test]
    fn parses_claude_with_null_fields() {
        let c = json!({"uuid":"u1","name":null,"chat_messages":[{"sender":"human","text":"q","content":null,"attachments":[{"file_name":"a.txt"}]}]});
        let conv = parse_claude(&c);
        assert_eq!(conv.title, "Untitled");
        assert!(conv.messages[0].text.contains("[attached: a.txt]"));
    }

    #[test]
    fn search_and_pack() {
        let c = Conversation { id: "1".into(), source: "claude".into(), title: "Rust sync".into(), created: "".into(), updated: "2026-01-01 00:00".into(), project: "".into(),
            messages: (0..200).map(|i| Message { role: if i % 2 == 0 { "user" } else { "assistant" }.into(), text: format!("message number {i} about vaults"), time: String::new() }).collect() };
        let hits = search(vec![c.clone()], "vaults rust", 5);
        assert_eq!(hits.len(), 1);
        let pack = build_pack(&c, 3000);
        assert!(pack.contains("earlier message(s) omitted") && pack.contains("message number 199") && pack.chars().count() < 3600);
    }
}


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
    out.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms));
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
