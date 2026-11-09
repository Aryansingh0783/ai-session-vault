//! Claude Code session logs as conversations.

use super::import::s;
use super::*;

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
    let updated = md
        .modified()
        .ok()
        .map(OffsetDateTime::from)
        .map(|d| format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year(), d.month() as u8, d.day(), d.hour(), d.minute()))
        .unwrap_or_default();
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
