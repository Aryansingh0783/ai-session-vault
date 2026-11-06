//! A local web UI for AI Session Vault. The Rust core serves the app and a small JSON/SSE API on
//! `127.0.0.1`; every API call needs a random per-run token, and requests with a foreign `Host` or
//! `Origin` are refused (so a web page you visit can't talk to it).

use anyhow::{anyhow, Result};
use include_dir::{include_dir, Dir};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server};
use vault_core::archive;
use vault_core::doctor;
use vault_core::engine::{Engine, EngineConfig, Event, Handle, Status};
use vault_core::env::{read_config, set_config, Env};
use vault_core::overview;
use vault_core::store::Store;

static WEB: Dir = include_dir!("$CARGO_MANIFEST_DIR/web");

pub struct UiOptions {
    /// 0 = pick a free port.
    pub port: u16,
    /// Run the sync engine behind the UI. `None` = read-only dashboard (nothing is synced).
    pub engine: Option<EngineConfig>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryItem {
    pub at: String,
    pub full: bool,
    pub uploaded: usize,
    pub downloaded: usize,
    pub merged: usize,
    pub conflicts: usize,
    pub failed: usize,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    pub notes: Vec<String>,
}

#[derive(Serialize, Clone, Default)]
struct Progress {
    phase: String,
    done: usize,
    total: usize,
}

#[derive(Default)]
struct Shared {
    status: Option<Status>,
    progress: Option<Progress>,
    history: Vec<HistoryItem>,
    last_error: Option<String>,
    overview: Option<(Instant, Value)>,
    disk: Option<(Instant, u64)>,
}

struct App {
    env: Env,
    store: Arc<Store>,
    token: String,
    port: u16,
    handle: Option<Handle>,
    shared: Mutex<Shared>,
    subscribers: Mutex<Vec<Sender<String>>>,
    started: Instant,
}

pub struct Ui {
    pub url: String,
    pub port: u16,
    pub token: String,
    server: Arc<Server>,
    accept: Option<std::thread::JoinHandle<()>>,
    handle: Option<Handle>,
}

impl Ui {
    pub fn start(env: Env, store: Arc<Store>, opts: UiOptions) -> Result<Ui> {
        let server = Arc::new(Server::http(format!("127.0.0.1:{}", opts.port)).map_err(|e| anyhow!("cannot listen: {e}"))?);
        let port = server.server_addr().to_ip().ok_or_else(|| anyhow!("no address"))?.port();
        let mut tok = [0u8; 24];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut tok);
        let token = hex::encode(tok);

        let history = std::fs::read(env.state_dir.join("history.json")).ok().and_then(|b| serde_json::from_slice::<Vec<HistoryItem>>(&b).ok()).unwrap_or_default();
        let (handle, events, engine_thread) = match opts.engine {
            Some(cfg) => {
                let (h, ev, t) = Engine::start(env.clone(), store.clone(), cfg)?.into_parts();
                (Some(h), Some(ev), t)
            }
            None => (None, None, None),
        };
        let app = Arc::new(App {
            env,
            store,
            token: token.clone(),
            port,
            handle: handle.clone(),
            shared: Mutex::new(Shared { history, ..Default::default() }),
            subscribers: Mutex::new(vec![]),
            started: Instant::now(),
        });

        if let Some(rx) = events {
            let a = app.clone();
            std::thread::spawn(move || {
                while let Ok(ev) = rx.recv() {
                    a.on_event(&ev);
                }
                drop(engine_thread);
            });
        }

        let srv = server.clone();
        let a = app.clone();
        let accept = std::thread::spawn(move || {
            for rq in srv.incoming_requests() {
                let a = a.clone();
                std::thread::spawn(move || a.handle(rq));
            }
        });
        Ok(Ui { url: format!("http://127.0.0.1:{port}/?t={token}"), port, token, server, accept: Some(accept), handle })
    }

    /// Block until the server stops.
    pub fn wait(mut self) {
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }

    pub fn shutdown(mut self) {
        if let Some(h) = &self.handle {
            h.stop();
        }
        self.server.unblock();
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }
}

// ------------------------------------------------------------------------------------------------
fn hdr(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (pct_decode(k), pct_decode(v)),
            None => (pct_decode(p), String::new()),
        })
        .collect()
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "woff2" => "font/woff2",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn dir_size(root: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(root).into_iter().flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()
}

struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

fn ok_json(v: Value) -> Reply {
    Reply { status: 200, content_type: "application/json", body: serde_json::to_vec(&v).unwrap_or_default() }
}

fn err(status: u16, msg: &str) -> Reply {
    Reply { status, content_type: "application/json", body: serde_json::to_vec(&json!({ "error": msg })).unwrap_or_default() }
}

impl App {
    fn broadcast(&self, line: String) {
        self.subscribers.lock().unwrap().retain(|s| s.send(line.clone()).is_ok());
    }

    fn on_event(&self, ev: &Event) {
        {
            let mut sh = self.shared.lock().unwrap();
            match ev {
                Event::Status(s) => sh.status = Some(s.clone()),
                Event::Progress { phase, done, total } => {
                    sh.progress = Some(Progress { phase: phase.clone(), done: *done, total: *total });
                }
                Event::SyncFinished { full, report } => {
                    let changed = report.uploaded + report.downloaded + report.merged + report.conflicts.len() + report.failed.len() > 0;
                    sh.progress = None;
                    if !changed {
                        // Routine "nothing to do" runs would bury the real activity.
                        return self.broadcast(serde_json::to_string(ev).unwrap_or_default());
                    }
                    let mut notes: Vec<String> = report.conflicts.clone();
                    notes.extend(report.warnings.iter().take(3).cloned());
                    sh.history.push(HistoryItem {
                        at: vault_core::util::now_iso(),
                        full: *full,
                        uploaded: report.uploaded,
                        downloaded: report.downloaded,
                        merged: report.merged,
                        conflicts: report.conflicts.len(),
                        failed: report.failed.len(),
                        bytes_uploaded: report.bytes_uploaded,
                        bytes_downloaded: report.bytes_downloaded,
                        notes: notes.into_iter().take(6).collect(),
                    });
                    let n = sh.history.len();
                    if n > 300 {
                        sh.history.drain(..n - 300);
                    }
                    sh.progress = None;
                    sh.overview = None; // the vault changed
                    sh.disk = None;
                    let _ = std::fs::write(self.env.state_dir.join("history.json"), serde_json::to_vec(&sh.history).unwrap_or_default());
                }
                Event::Error { message } => sh.last_error = Some(message.clone()),
                Event::Log { .. } => {}
            }
        }
        if !matches!(ev, Event::Log { .. }) {
            self.broadcast(serde_json::to_string(ev).unwrap_or_default());
        }
    }

    fn host_ok(&self, rq: &Request) -> bool {
        let want = [format!("127.0.0.1:{}", self.port), format!("localhost:{}", self.port)];
        let host = rq.headers().iter().find(|h| h.field.equiv("Host")).map(|h| h.value.as_str().to_string()).unwrap_or_default();
        if !want.contains(&host) {
            return false;
        }
        match rq.headers().iter().find(|h| h.field.equiv("Origin")).map(|h| h.value.as_str().to_string()) {
            None => true,
            Some(o) => want.iter().any(|w| o == format!("http://{w}")),
        }
    }

    fn handle(self: Arc<Self>, mut rq: Request) {
        if !self.host_ok(&rq) {
            let _ = rq.respond(Response::from_string("forbidden").with_status_code(403));
            return;
        }
        let url = rq.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
        let q = parse_query(query);
        let method = rq.method().clone();

        if path.starts_with("/api/") {
            let tok = rq.headers().iter().find(|h| h.field.equiv("X-Token")).map(|h| h.value.as_str().to_string()).or_else(|| q.get("t").cloned());
            if tok.as_deref() != Some(self.token.as_str()) {
                let r = err(401, "missing or wrong token");
                let _ = rq.respond(Response::from_data(r.body).with_status_code(401).with_header(hdr("Content-Type", r.content_type)));
                return;
            }
            if path == "/api/events" {
                return self.sse(rq);
            }
            let mut body = String::new();
            if method == Method::Post {
                let _ = rq.as_reader().take(1 << 20).read_to_string(&mut body);
            }
            let reply = self.api(&method, path, &q, &body);
            let resp = Response::from_data(reply.body)
                .with_status_code(reply.status)
                .with_header(hdr("Content-Type", reply.content_type))
                .with_header(hdr("Cache-Control", "no-store"));
            let _ = rq.respond(resp);
            return;
        }
        if method != Method::Get {
            let _ = rq.respond(Response::from_string("method not allowed").with_status_code(405));
            return;
        }
        let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
        match WEB.get_file(rel) {
            Some(f) => {
                let mut r = Response::from_data(f.contents().to_vec()).with_header(hdr("Content-Type", mime(rel)));
                r = r.with_header(hdr("Content-Security-Policy", "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self'; frame-ancestors 'none'"));
                r = r.with_header(hdr("X-Content-Type-Options", "nosniff")).with_header(hdr("Cache-Control", "no-cache"));
                let _ = rq.respond(r);
            }
            None => {
                let _ = rq.respond(Response::from_string("not found").with_status_code(404));
            }
        }
    }

    /// Server-sent events: engine status/progress/finished events, pushed as they happen.
    fn sse(&self, rq: Request) {
        let (tx, rx) = channel::<String>();
        self.subscribers.lock().unwrap().push(tx);
        let mut w = rq.into_writer();
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: keep-alive\r\n\r\n";
        if w.write_all(head.as_bytes()).is_err() {
            return;
        }
        let _ = w.write_all(b": connected\n\n");
        let _ = w.flush();
        loop {
            let chunk = match rx.recv_timeout(Duration::from_secs(15)) {
                Ok(line) => format!("data: {line}\n\n"),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => ": ping\n\n".to_string(),
                Err(_) => return,
            };
            if w.write_all(chunk.as_bytes()).is_err() || w.flush().is_err() {
                return;
            }
        }
    }

    fn overview_json(&self) -> Result<Value> {
        {
            let sh = self.shared.lock().unwrap();
            if let Some((at, v)) = &sh.overview {
                if at.elapsed() < Duration::from_secs(10) {
                    return Ok(v.clone());
                }
            }
        }
        let o = overview::overview(&self.env, &self.store, &[])?;
        let disk = {
            let sh = self.shared.lock().unwrap();
            match sh.disk {
                Some((at, d)) if at.elapsed() < Duration::from_secs(60) => d,
                _ => {
                    drop(sh);
                    let d = dir_size(&self.store.root);
                    self.shared.lock().unwrap().disk = Some((Instant::now(), d));
                    d
                }
            }
        };
        let mut v = serde_json::to_value(&o)?;
        v["disk_bytes"] = json!(disk);
        self.shared.lock().unwrap().overview = Some((Instant::now(), v.clone()));
        Ok(v)
    }

    fn api(&self, method: &Method, path: &str, q: &HashMap<String, String>, body: &str) -> Reply {
        let post = *method == Method::Post;
        let b: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let run = || -> Result<Reply> {
            Ok(match (post, path) {
                (false, "/api/overview") => ok_json(self.overview_json()?),
                (false, "/api/status") => {
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
                (false, "/api/history") => ok_json(json!(self.shared.lock().unwrap().history)),
                (false, "/api/doctor") => ok_json(json!(doctor::run_opts(&self.env, true, q.get("deep").is_some_and(|v| v == "1")))),
                (false, "/api/devices") => {
                    let (ms, warns) = self.store.read_all_manifests()?;
                    let me = vault_core::env::State::load(&self.env).device_id;
                    ok_json(json!({
                        "devices": ms.iter().map(|m| json!({"id": m.device.id, "name": m.device.name, "os": m.device.os, "updated": m.updated, "files": m.entries.len(), "this_device": m.device.id == me})).collect::<Vec<_>>(),
                        "joined": self.store.device_slots().iter().map(|s| json!({"id": s.id, "name": s.name, "created": s.created})).collect::<Vec<_>>(),
                        "warnings": warns,
                    }))
                }
                (false, "/api/search") => {
                    let query = q.get("q").cloned().unwrap_or_default();
                    let hits = archive::search_fast(&self.env, &query, 40);
                    ok_json(json!(hits.iter().map(|h| json!({
                        "id": h.conversation.id, "source": h.conversation.source, "title": h.conversation.title,
                        "updated": h.conversation.updated, "project": h.conversation.project,
                        "messages": h.conversation.messages.len(), "snippet": h.snippet,
                    })).collect::<Vec<_>>()))
                }
                (false, "/api/conversation") => match archive::find_conversation(&self.env, q.get("source").map_or("", |s| s), q.get("id").map_or("", |s| s)) {
                    Some(c) => ok_json(serde_json::to_value(c)?),
                    None => err(404, "conversation not found"),
                },
                (false, "/api/pack") => match archive::find_conversation(&self.env, q.get("source").map_or("", |s| s), q.get("id").map_or("", |s| s)) {
                    Some(c) => Reply { status: 200, content_type: "text/markdown; charset=utf-8", body: archive::build_pack(&c, 60000).into_bytes() },
                    None => err(404, "conversation not found"),
                },
                (false, "/api/settings") => {
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
                (true, "/api/sync") => self.command(|h| h.sync_now()),
                (true, "/api/pause") => self.command(|h| h.pause()),
                (true, "/api/resume") => self.command(|h| h.resume()),
                (true, "/api/cancel") => self.command(|h| h.cancel_current()),
                (true, "/api/enroll") => {
                    let minutes = b.get("minutes").and_then(|m| m.as_u64()).unwrap_or(15).clamp(1, 120);
                    ok_json(json!({ "code": self.store.create_enrollment(minutes * 60)?, "expires_in_minutes": minutes }))
                }
                (true, "/api/revoke") => {
                    self.store.revoke_device_slot(b.get("id").and_then(|i| i.as_str()).unwrap_or(""))?;
                    ok_json(json!({ "ok": true }))
                }
                (true, "/api/forget-device") => {
                    self.store.remove_device(b.get("id").and_then(|i| i.as_str()).unwrap_or(""))?;
                    self.shared.lock().unwrap().overview = None;
                    ok_json(json!({ "ok": true }))
                }
                (true, "/api/exclude") => {
                    let p = b.get("path").and_then(|p| p.as_str()).unwrap_or("").trim().replace('\\', "/");
                    if p.is_empty() {
                        return Ok(err(400, "path is required"));
                    }
                    let mut list: Vec<String> = read_config(&self.env.state_dir).get("exclude_projects").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
                    if b.get("remove").and_then(|r| r.as_bool()).unwrap_or(false) {
                        list.retain(|x| *x != p);
                    } else if !list.contains(&p) {
                        list.push(p);
                    }
                    set_config(&self.env.state_dir, "exclude_projects", json!(list))?;
                    ok_json(json!({ "exclude_projects": list }))
                }
                (true, "/api/verify") => {
                    let (ms, warns) = self.store.read_all_manifests()?;
                    let r = self.store.verify(&ms, b.get("deep").and_then(|d| d.as_bool()).unwrap_or(false));
                    ok_json(json!({ "report": r, "warnings": warns }))
                }
                (true, "/api/gc") => {
                    let (ms, warns) = self.store.read_all_manifests()?;
                    if !warns.is_empty() {
                        return Ok(err(409, "some device manifests are unreadable; let the folder finish syncing first"));
                    }
                    let (n, bytes) = self.store.gc(&ms)?;
                    self.shared.lock().unwrap().disk = None;
                    ok_json(json!({ "removed": n, "freed_bytes": bytes }))
                }
                _ => err(404, "unknown endpoint"),
            })
        };
        run().unwrap_or_else(|e| err(500, &format!("{e:#}")))
    }

    fn command(&self, f: impl FnOnce(&Handle)) -> Reply {
        match &self.handle {
            Some(h) => {
                f(h);
                ok_json(json!({ "ok": true }))
            }
            None => err(409, "the sync engine is not running in this session"),
        }
    }
}

