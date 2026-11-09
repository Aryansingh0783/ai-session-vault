//! First-run setup. Before there is a vault to show, the app serves a small setup page (create a vault,
//! unlock one, or join one from another device). The page talks to a [`SetupBackend`], so the actual
//! vault work (keys, credential store) stays in the program that owns it. Same local-only rules as the
//! main UI: 127.0.0.1, a per-run token, Host/Origin checks.

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::json;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tiny_http::{Method, Server};

use crate::http::{err, host_ok, ok_json, parse_url, read_body, respond_json, serve_static, token_ok, Reply};
use crate::requests::{self, CreateReq, Invalid, JoinReq, UnlockReq};

/// A folder the page offers (a cloud-synced folder is the usual choice).
#[derive(Serialize, Clone, Debug)]
pub struct Suggestion {
    pub label: String,
    pub path: String,
    /// A vault already lives there (so it should be unlocked or joined, not created).
    pub has_vault: bool,
}

/// What the page needs to start.
#[derive(Serialize, Clone, Debug)]
pub struct SetupStatus {
    /// The folder this device is already set up with, if any (the vault is locked or missing).
    pub folder: Option<String>,
    /// Why setup is showing, in words for the user (e.g. the stored passphrase stopped working).
    pub reason: Option<String>,
    pub suggestions: Vec<Suggestion>,
    pub device_name: String,
}

/// The vault operations behind the setup page. Errors are shown to the user as they are.
pub trait SetupBackend: Send + Sync + 'static {
    fn status(&self) -> SetupStatus;
    /// Is there a vault in this folder?
    fn has_vault(&self, folder: &Path) -> bool;
    /// Create a vault; returns the one-time recovery key.
    fn create(&self, folder: &Path, passphrase: &str, remember: bool) -> Result<String>;
    /// Open a vault with its passphrase or recovery key and remember this device's choice.
    fn unlock(&self, folder: &Path, secret: &str, remember: bool) -> Result<()>;
    /// Join with a one-time code from another device.
    fn join(&self, folder: &Path, code: &str) -> Result<()>;
}

struct Inner {
    backend: Arc<dyn SetupBackend>,
    token: String,
    port: u16,
    done: Mutex<bool>,
    cv: Condvar,
}

pub struct Setup {
    pub url: String,
    pub port: u16,
    pub token: String,
    server: Arc<Server>,
    inner: Arc<Inner>,
    accept: Option<std::thread::JoinHandle<()>>,
}

impl Setup {
    pub fn start(backend: Arc<dyn SetupBackend>, port: u16) -> Result<Setup> {
        let server = Arc::new(Server::http(format!("127.0.0.1:{port}")).map_err(|e| anyhow!("cannot listen: {e}"))?);
        let port = server.server_addr().to_ip().ok_or_else(|| anyhow!("no address"))?.port();
        let mut tok = [0u8; 24];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut tok);
        let token = hex::encode(tok);
        let inner = Arc::new(Inner { backend, token: token.clone(), port, done: Mutex::new(false), cv: Condvar::new() });
        let (srv, i) = (server.clone(), inner.clone());
        let accept = std::thread::spawn(move || {
            for rq in srv.incoming_requests() {
                let i = i.clone();
                std::thread::spawn(move || i.handle(rq));
            }
        });
        Ok(Setup { url: format!("http://127.0.0.1:{port}/setup.html?t={token}"), port, token, server, inner, accept: Some(accept) })
    }

    /// Wait up to `timeout` for the page to finish. True once it has.
    pub fn wait_finished(&self, timeout: Duration) -> bool {
        let done = self.inner.done.lock().unwrap();
        let (done, _) = self.inner.cv.wait_timeout_while(done, timeout, |d| !*d).unwrap();
        *done
    }

    pub fn shutdown(mut self) {
        self.server.unblock();
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }
}

impl Inner {
    fn handle(self: Arc<Self>, mut rq: tiny_http::Request) {
        if !host_ok(self.port, &rq) {
            let _ = rq.respond(tiny_http::Response::from_string("forbidden").with_status_code(403));
            return;
        }
        let (path, q) = parse_url(&rq);
        if path.starts_with("/api/") {
            if !token_ok(&rq, &q, &self.token) {
                return respond_json(rq, err(401, "missing or wrong token"));
            }
            let body = read_body(&mut rq);
            let method = rq.method().clone();
            let reply = match self.api(&method, &path, &q, &body) {
                Ok(r) => r,
                Err(Failure::Invalid(Invalid(m))) => err(400, &m),
                Err(Failure::Backend(e)) => err(400, &format!("{e:#}")),
            };
            return respond_json(rq, reply);
        }
        if *rq.method() != Method::Get {
            let _ = rq.respond(tiny_http::Response::from_string("method not allowed").with_status_code(405));
            return;
        }
        // The setup page is the home page while setting up.
        let path = if path == "/" { "/setup.html".to_string() } else { path };
        serve_static(rq, &path);
    }

    fn api(&self, method: &Method, path: &str, q: &std::collections::HashMap<String, String>, body: &str) -> Result<Reply, Failure> {
        let b = &self.backend;
        Ok(match (method, path) {
            (Method::Get, "/api/setup") => ok_json(json!(b.status())),
            (Method::Get, "/api/setup/inspect") => ok_json(json!({ "has_vault": b.has_vault(&requests::folder_param(q)?) })),
            (Method::Post, "/api/setup/create") => {
                let req: CreateReq = requests::parse_body(body)?;
                let (folder, pass) = req.checked()?;
                let recovery = b.create(&folder, pass, req.remember).map_err(Failure::Backend)?;
                ok_json(json!({ "recovery_key": recovery, "folder": folder }))
            }
            (Method::Post, "/api/setup/unlock") => {
                let req: UnlockReq = requests::parse_body(body)?;
                let (folder, secret) = req.checked()?;
                b.unlock(&folder, secret, req.remember).map_err(Failure::Backend)?;
                ok_json(json!({ "ok": true }))
            }
            (Method::Post, "/api/setup/join") => {
                let req: JoinReq = requests::parse_body(body)?;
                let (folder, code) = req.checked()?;
                b.join(&folder, code).map_err(Failure::Backend)?;
                ok_json(json!({ "ok": true }))
            }
            (Method::Post, "/api/setup/finish") => {
                *self.done.lock().unwrap() = true;
                self.cv.notify_all();
                ok_json(json!({ "ok": true }))
            }
            _ => err(404, "unknown endpoint"),
        })
    }
}

enum Failure {
    Invalid(Invalid),
    Backend(anyhow::Error),
}

impl From<Invalid> for Failure {
    fn from(i: Invalid) -> Failure {
        Failure::Invalid(i)
    }
}
