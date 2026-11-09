//! Parsers for claude.ai, ChatGPT and Claude Code history.

use super::*;

pub(super) fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
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

pub(super) fn gpt_text(content: &Value) -> String {
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
        let parts: Vec<&str> = m.get("content").and_then(|c| c.as_array()).into_iter().flatten().filter(|b| s(b, "type") == Some("text")).filter_map(|b| s(b, "text")).collect();
        let mut text = parts.join("\n\n").trim().to_string();
        if text.is_empty() {
            text = s(m, "text").unwrap_or("").trim().to_string();
        }
        let names: Vec<&str> = ["attachments", "files"].iter().flat_map(|k| m.get(*k).and_then(|a| a.as_array()).into_iter().flatten()).filter_map(|a| s(a, "file_name")).collect();
        if !names.is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
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

pub(super) fn is_conv_file(name: &str) -> bool {
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
