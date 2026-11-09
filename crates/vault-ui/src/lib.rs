//! A local web UI for AI Session Vault. The Rust core serves the app and a small JSON/SSE API on
//! `127.0.0.1`; every API call needs a random per-run token, and requests with a foreign `Host` or
//! `Origin` are refused (so a web page you visit can't talk to it).

mod api;
mod http;
mod requests;
mod state;

use anyhow::{anyhow, Result};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tiny_http::Server;
use vault_core::engine::{Engine, EngineConfig, Handle};
use vault_core::env::Env;
use vault_core::store::Store;

pub use state::HistoryItem;
use state::{App, Shared};

pub struct UiOptions {
    /// 0 = pick a free port.
    pub port: u16,
    /// Run the sync engine behind the UI. `None` = read-only dashboard (nothing is synced).
    pub engine: Option<EngineConfig>,
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
