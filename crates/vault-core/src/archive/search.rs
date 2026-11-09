//! Searching conversations and building context packs.

use super::*;

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
            if title.contains(w) {
                score += 10
            }
            if project.contains(w) {
                score += 4
            }
        }
        let snippet = words
            .first()
            .and_then(|w| {
                body.iter().enumerate().find_map(|(i, b)| {
                    b.find(w.as_str()).map(|pos| {
                        let t = &c.messages[i].text;
                        let start = t.char_indices().map(|(x, _)| x).find(|x| *x + 60 >= pos).unwrap_or(0);
                        t[start..].chars().take(140).collect::<String>().replace('\n', " ")
                    })
                })
            })
            .unwrap_or_default();
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
    let blocks: Vec<String> = c
        .messages
        .iter()
        .map(|m| {
            format!("**{}**{}:\n\n{}\n\n", if m.role == "user" { "User" } else { "Assistant" }, if m.time.is_empty() { String::new() } else { format!(" ({})", m.time) }, m.text)
        })
        .collect();
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
            if n > rem {
                break;
            }
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
        out.push_str(&format!(
            "## {}{}\n\n{}\n\n",
            if m.role == "user" { "User" } else { "Assistant" },
            if m.time.is_empty() { String::new() } else { format!(" · {}", m.time) },
            m.text
        ));
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
