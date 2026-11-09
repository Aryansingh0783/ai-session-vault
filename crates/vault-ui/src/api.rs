//! The JSON API. Each endpoint is a small method; inputs are validated in `requests` first.

use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use tiny_http::Method;
use vault_core::archive;
use vault_core::doctor;
use vault_core::env::{read_config, set_config};

use crate::http::{err, ok_json, Reply};
use crate::requests::{self, DeviceReq, EnrollReq, ExcludeReq, Invalid, VerifyReq};
use crate::state::App;

type Query = HashMap<String, String>;

/// What the user sees when something unexpected fails. The details go to `ui.log`.
const INTERNAL_MESSAGE: &str = "Something went wrong on this device. The details were saved to ui.log in the vault state folder.";

impl App {
    pub(crate) fn api(&self, method: &Method, path: &str, q: &Query, body: &str) -> Reply {
        let post = *method == Method::Post;
        let result = if post { self.post(path, body) } else { self.get(path, q) };
        match result {
            Ok(reply) => reply,
            Err(ApiError::Invalid(Invalid(msg))) => err(400, &msg),
            Err(ApiError::Internal(e)) => {
                self.log_internal(path, &e);
                err(500, INTERNAL_MESSAGE)
            }
        }
    }

    fn get(&self, path: &str, q: &Query) -> Result<Reply, ApiError> {
        Ok(match path {
            "/api/overview" => ok_json(self.overview_json()?),
            "/api/status" => self.status_json(),
            "/api/history" => ok_json(json!(self.shared.lock().unwrap().history)),
            "/api/doctor" => ok_json(json!(doctor::run_opts(&self.env, true, requests::deep_flag(q)))),
            "/api/devices" => self.devices_json()?,
            "/api/search" => self.search_json(q)?,
            "/api/conversation" => self.conversation_json(q)?,
            "/api/pack" => self.pack(q)?,
            "/api/settings" => self.settings_json(),
            _ => err(404, "unknown endpoint"),
        })
    }

    fn post(&self, path: &str, body: &str) -> Result<Reply, ApiError> {
        Ok(match path {
            "/api/sync" => self.command(|h| h.sync_now()),
            "/api/pause" => self.command(|h| h.pause()),
            "/api/resume" => self.command(|h| h.resume()),
            "/api/cancel" => self.command(|h| h.cancel_current()),
            "/api/enroll" => self.enroll(requests::parse_body(body)?)?,
            "/api/revoke" => self.revoke(requests::parse_body(body)?)?,
            "/api/forget-device" => self.forget_device(requests::parse_body(body)?)?,
            "/api/exclude" => self.exclude(requests::parse_body(body)?)?,
            "/api/verify" => self.verify(requests::parse_body(body)?)?,
            "/api/gc" => self.gc()?,
            _ => err(404, "unknown endpoint"),
        })
    }

    // ---- reads ---------------------------------------------------------------------------------

    fn status_json(&self) -> Reply {
        let sh = self.shared.lock().unwrap();
        ok_json(json!({
            "engine": self.handle.is_some(),
            "status": sh.status,
            "progress": sh.progress,
            "last_error": sh.last_error,
            "uptime_secs": self.started.elapsed().as_secs(),
            "version": env!("CARGO_PKG_VERSION"),
        }))
    }

    fn devices_json(&self) -> Result<Reply> {
        let (ms, warns) = self.store.read_all_manifests()?;
        let me = vault_core::env::State::load(&self.env).device_id;
        let devices: Vec<Value> = ms
            .iter()
            .map(|m| json!({"id": m.device.id, "name": m.device.name, "os": m.device.os, "updated": m.updated, "files": m.entries.len(), "this_device": m.device.id == me}))
            .collect();
        let joined: Vec<Value> = self.store.device_slots().iter().map(|s| json!({"id": s.id, "name": s.name, "created": s.created})).collect();
        Ok(ok_json(json!({ "devices": devices, "joined": joined, "warnings": warns })))
    }

    fn search_json(&self, q: &Query) -> Result<Reply, ApiError> {
        let text = requests::search_text(q)?;
        let hits = archive::search_fast(&self.env, &text, 40);
        let rows: Vec<Value> = hits
            .iter()
            .map(|h| {
                json!({
                    "id": h.conversation.id, "source": h.conversation.source, "title": h.conversation.title,
                    "updated": h.conversation.updated, "project": h.conversation.project,
                    "messages": h.conversation.messages.len(), "snippet": h.snippet,
                })
            })
            .collect();
        Ok(ok_json(json!(rows)))
    }

    fn conversation_json(&self, q: &Query) -> Result<Reply, ApiError> {
        let (source, id) = requests::conversation_ref(q)?;
        Ok(match archive::find_conversation(&self.env, source, id) {
            Some(c) => ok_json(serde_json::to_value(c).map_err(anyhow::Error::from)?),
            None => err(404, "conversation not found"),
        })
    }

    fn pack(&self, q: &Query) -> Result<Reply, ApiError> {
        let (source, id) = requests::conversation_ref(q)?;
        Ok(match archive::find_conversation(&self.env, source, id) {
            Some(c) => Reply { status: 200, content_type: "text/markdown; charset=utf-8", body: archive::build_pack(&c, 60000).into_bytes() },
            None => err(404, "conversation not found"),
        })
    }

    fn settings_json(&self) -> Reply {
        let cfg = read_config(&self.env.state_dir);
        ok_json(json!({
            "vault_path": self.store.root, "vault_id": self.store.header.vault_id,
            "user": std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default(),
            "exclude_projects": cfg.get("exclude_projects").cloned().unwrap_or(json!([])),
            "local_keys": self.env.local_keys,
            "tools": { "claude_code": self.env.claude_dir.is_dir(), "codex": self.env.codex_dir.is_dir(), "claude_desktop": self.env.desktop_cfg.is_file() },
            "claude_dir": self.env.claude_dir, "codex_dir": self.env.codex_dir,
            "state_dir": self.env.state_dir, "engine": self.handle.is_some(),
        }))
    }

    // ---- writes --------------------------------------------------------------------------------

    fn enroll(&self, req: EnrollReq) -> Result<Reply, ApiError> {
        let minutes = req.minutes()?;
        Ok(ok_json(json!({ "code": self.store.create_enrollment(minutes * 60)?, "expires_in_minutes": minutes })))
    }

    fn revoke(&self, req: DeviceReq) -> Result<Reply, ApiError> {
        let id = req.id()?;
        if !self.store.device_slots().iter().any(|s| s.id == id) {
            return Ok(err(404, "that device has no passphrase-free access to revoke"));
        }
        self.store.revoke_device_slot(id)?;
        Ok(ok_json(json!({ "ok": true })))
    }

    fn forget_device(&self, req: DeviceReq) -> Result<Reply, ApiError> {
        let id = req.id()?;
        let (ms, _) = self.store.read_all_manifests()?;
        if !ms.iter().any(|m| m.device.id == id) {
            return Ok(err(404, "no such device in this vault"));
        }
        self.store.remove_device(id)?;
        self.shared.lock().unwrap().overview = None;
        Ok(ok_json(json!({ "ok": true })))
    }

    fn exclude(&self, req: ExcludeReq) -> Result<Reply, ApiError> {
        let path = req.path()?;
        let mut list: Vec<String> = read_config(&self.env.state_dir).get("exclude_projects").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
        if req.remove {
            list.retain(|x| *x != path);
        } else if !list.contains(&path) {
            list.push(path);
        }
        set_config(&self.env.state_dir, "exclude_projects", json!(list))?;
        Ok(ok_json(json!({ "exclude_projects": list })))
    }

    fn verify(&self, req: VerifyReq) -> Result<Reply, ApiError> {
        let (ms, warns) = self.store.read_all_manifests()?;
        let report = self.store.verify(&ms, req.deep);
        Ok(ok_json(json!({ "report": report, "warnings": warns })))
    }

    fn gc(&self) -> Result<Reply, ApiError> {
        let (ms, warns) = self.store.read_all_manifests()?;
        if !warns.is_empty() {
            return Ok(err(409, "some device manifests are unreadable; let the folder finish syncing first"));
        }
        let (n, bytes) = self.store.gc(&ms)?;
        self.shared.lock().unwrap().disk = None;
        Ok(ok_json(json!({ "removed": n, "freed_bytes": bytes })))
    }

    // ---- errors --------------------------------------------------------------------------------

    /// Unexpected failures: keep the full detail in a log file, never on screen.
    fn log_internal(&self, path: &str, e: &anyhow::Error) {
        let line = format!("{} {} {:#}\n", vault_core::util::now_iso(), path, e);
        let _ = std::fs::create_dir_all(&self.env.state_dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.env.state_dir.join("ui.log")) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// How an endpoint can fail: bad input (400) or an unexpected error (500, details go to the log).
pub(crate) enum ApiError {
    Invalid(Invalid),
    Internal(anyhow::Error),
}

impl From<Invalid> for ApiError {
    fn from(e: Invalid) -> Self {
        ApiError::Invalid(e)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}
