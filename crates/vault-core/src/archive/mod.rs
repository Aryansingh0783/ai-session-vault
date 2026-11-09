//! Offline archive of claude.ai / ChatGPT data exports, cross-source search, and context packs.
//! Each conversation is stored as its own small file under `~/.ai-vault/archive/<source>/<id>.json`,
//! which the sync engine carries (encrypted) to your other devices like any other file.

mod cc_session;
mod import;
mod search;
mod sources;
mod storage;
pub use cc_session::parse_cc_session;
pub use import::{fmt_ts, load_export, parse_chatgpt, parse_claude};
pub use search::{build_pack, find_by_id, search, to_markdown, Hit};
pub use sources::{find_conversation, list_sources, load_source, recent, search_fast, SourceFile};
pub use storage::{archive_dir, load_all, save_all};

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
        let c = Conversation {
            id: "1".into(),
            source: "claude".into(),
            title: "Rust sync".into(),
            created: "".into(),
            updated: "2026-01-01 00:00".into(),
            project: "".into(),
            messages: (0..200)
                .map(|i| Message { role: if i % 2 == 0 { "user" } else { "assistant" }.into(), text: format!("message number {i} about vaults"), time: String::new() })
                .collect(),
        };
        let hits = search(vec![c.clone()], "vaults rust", 5);
        assert_eq!(hits.len(), 1);
        let pack = build_pack(&c, 3000);
        assert!(pack.contains("earlier message(s) omitted") && pack.contains("message number 199") && pack.chars().count() < 3600);
    }
}
