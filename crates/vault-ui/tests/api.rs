use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vault_core::crypto::KdfParams;
use vault_core::engine::EngineConfig;
use vault_core::env::Env;
use vault_core::store::Store;
use vault_ui::{Ui, UiOptions};

fn start() -> (Ui, tempfile::TempDir, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let env = Env::detect(Some(home.path())).unwrap();
    std::fs::create_dir_all(env.claude_dir.join("projects")).unwrap();
    std::fs::write(env.claude_dir.join("CLAUDE.md"), "hello").unwrap();
    let (store, _) = Store::init(vault.path(), "correct horse battery staple", KdfParams { m_kib: 64, t: 1, p: 1 }).unwrap();
    let cfg = EngineConfig { watch: false, quiet: Duration::from_millis(200), ..Default::default() };
    let ui = Ui::start(env, Arc::new(store), UiOptions { port: 0, engine: Some(cfg) }).unwrap();
    (ui, home, vault)
}

fn raw(port: u16, request: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(request.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

#[test]
fn serves_the_app_and_guards_the_api() {
    let (ui, _h, _v) = start();
    let base = format!("http://127.0.0.1:{}", ui.port);

    // The page itself is public; it carries no data.
    let html = ureq::get(&format!("{base}/")).call().unwrap().into_string().unwrap();
    assert!(html.contains("<title>AI Session Vault</title>"));
    assert!(ureq::get(&format!("{base}/js/app.js")).call().unwrap().into_string().unwrap().contains("avault_t"));

    // The API needs the per-run token.
    match ureq::get(&format!("{base}/api/overview")).call() {
        Err(ureq::Error::Status(401, _)) => {}
        other => panic!("expected 401, got {other:?}"),
    }
    match ureq::get(&format!("{base}/api/overview")).set("X-Token", "nope").call() {
        Err(ureq::Error::Status(401, _)) => {}
        other => panic!("expected 401, got {other:?}"),
    }
    // A forged Host (DNS rebinding) is refused even with the right token.
    let r = raw(ui.port, &format!("GET /api/overview HTTP/1.1\r\nHost: evil.example\r\nX-Token: {}\r\nConnection: close\r\n\r\n", ui.token));
    assert!(r.starts_with("HTTP/1.1 403"), "{r}");
    // So is a cross-site POST.
    let r = raw(ui.port, &format!("POST /api/sync HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: http://evil.example\r\nX-Token: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", ui.port, ui.token));
    assert!(r.starts_with("HTTP/1.1 403"), "{r}");
    ui.shutdown();
}

#[test]
fn api_drives_the_engine_and_reports_state() {
    let (ui, home, _v) = start();
    let base = format!("http://127.0.0.1:{}", ui.port);
    let get = |p: &str| -> serde_json::Value { ureq::get(&format!("{base}{p}")).set("X-Token", &ui.token).call().unwrap().into_json().unwrap() };
    let post = |p: &str, b: serde_json::Value| -> serde_json::Value { ureq::post(&format!("{base}{p}")).set("X-Token", &ui.token).send_json(b).unwrap().into_json().unwrap() };

    // The engine's first sync uploads CLAUDE.md; wait for it to be recorded.
    let end = Instant::now() + Duration::from_secs(20);
    while get("/api/history").as_array().unwrap().is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(200));
    }
    let h = get("/api/history");
    assert_eq!(h[0]["uploaded"], 1, "{h}");
    assert!(home.path().join(".ai-vault/history.json").exists(), "history is persisted");

    let o = get("/api/overview");
    assert_eq!(o["totals"]["devices"], 1);
    assert!(o["disk_bytes"].as_u64().unwrap() > 0);
    assert_eq!(get("/api/status")["engine"], true);
    assert_eq!(get("/api/devices")["devices"].as_array().unwrap().len(), 1);

    // Enrollment, exclusion and verify round-trip through the API.
    assert!(post("/api/enroll", serde_json::json!({"minutes": 5}))["code"].as_str().unwrap().contains('-'));
    let ex = post("/api/exclude", serde_json::json!({"path": "~/noise"}));
    assert_eq!(ex["exclude_projects"][0], "~/noise");
    assert_eq!(get("/api/settings")["exclude_projects"][0], "~/noise");
    let v = post("/api/verify", serde_json::json!({"deep": true}));
    assert_eq!(v["report"]["missing"].as_array().unwrap().len(), 0);
    assert_eq!(post("/api/pause", serde_json::json!({}))["ok"], true);
    ui.shutdown();
}
