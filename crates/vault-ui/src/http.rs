//! HTTP plumbing: host/origin/token checks, static assets, server-sent events and reply helpers.

use include_dir::{include_dir, Dir};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response};

use crate::state::App;

static WEB: Dir = include_dir!("$CARGO_MANIFEST_DIR/web");

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

pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) content_type: &'static str,
    pub(crate) body: Vec<u8>,
}

pub(crate) fn ok_json(v: Value) -> Reply {
    Reply { status: 200, content_type: "application/json", body: serde_json::to_vec(&v).unwrap_or_default() }
}

pub(crate) fn err(status: u16, msg: &str) -> Reply {
    Reply { status, content_type: "application/json", body: serde_json::to_vec(&json!({ "error": msg })).unwrap_or_default() }
}

/// Only requests addressed to this server (right Host, and no foreign Origin) get through; that
/// stops a web page you visit from talking to it (DNS rebinding, cross-site POST).
pub(crate) fn host_ok(port: u16, rq: &Request) -> bool {
    let want = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let host = rq.headers().iter().find(|h| h.field.equiv("Host")).map(|h| h.value.as_str().to_string()).unwrap_or_default();
    if !want.contains(&host) {
        return false;
    }
    match rq.headers().iter().find(|h| h.field.equiv("Origin")).map(|h| h.value.as_str().to_string()) {
        None => true,
        Some(o) => want.iter().any(|w| o == format!("http://{w}")),
    }
}

/// The per-run token from the `X-Token` header, or `?t=` as a fallback (event streams).
pub(crate) fn token_ok(rq: &Request, q: &HashMap<String, String>, want: &str) -> bool {
    let tok = rq.headers().iter().find(|h| h.field.equiv("X-Token")).map(|h| h.value.as_str().to_string()).or_else(|| q.get("t").cloned());
    tok.as_deref() == Some(want)
}

pub(crate) fn respond_json(rq: Request, reply: Reply) {
    let resp = Response::from_data(reply.body).with_status_code(reply.status).with_header(hdr("Content-Type", reply.content_type)).with_header(hdr("Cache-Control", "no-store"));
    let _ = rq.respond(resp);
}

/// Read a POST body (capped at 1 MiB).
pub(crate) fn read_body(rq: &mut Request) -> String {
    let mut body = String::new();
    if *rq.method() == Method::Post {
        let _ = rq.as_reader().take(1 << 20).read_to_string(&mut body);
    }
    body
}

/// Serve an embedded file of the web app (`/` is the index page).
pub(crate) fn serve_static(rq: Request, path: &str) {
    let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
    match WEB.get_file(rel) {
        Some(f) => {
            let mut r = Response::from_data(f.contents().to_vec()).with_header(hdr("Content-Type", mime(rel)));
            r = r.with_header(hdr(
                "Content-Security-Policy",
                "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self'; frame-ancestors 'none'",
            ));
            r = r.with_header(hdr("X-Content-Type-Options", "nosniff")).with_header(hdr("Cache-Control", "no-cache"));
            let _ = rq.respond(r);
        }
        None => {
            let _ = rq.respond(Response::from_string("not found").with_status_code(404));
        }
    }
}

pub(crate) fn parse_url(rq: &Request) -> (String, HashMap<String, String>) {
    let url = rq.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    (path.to_string(), parse_query(query))
}

impl App {
    pub(crate) fn handle(self: Arc<Self>, mut rq: Request) {
        if !host_ok(self.port, &rq) {
            let _ = rq.respond(Response::from_string("forbidden").with_status_code(403));
            return;
        }
        let (path, q) = parse_url(&rq);
        let method = rq.method().clone();

        if path.starts_with("/api/") {
            if !token_ok(&rq, &q, &self.token) {
                return respond_json(rq, err(401, "missing or wrong token"));
            }
            if path == "/api/events" {
                return self.sse(rq);
            }
            let body = read_body(&mut rq);
            let reply = self.api(&method, &path, &q, &body);
            return respond_json(rq, reply);
        }
        if method != Method::Get {
            let _ = rq.respond(Response::from_string("method not allowed").with_status_code(405));
            return;
        }
        serve_static(rq, &path);
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
}
