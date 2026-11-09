//! The first-run setup server: the page, the token guard, validation, and the create/unlock/join calls.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vault_ui::{Setup, SetupBackend, SetupStatus, Suggestion};

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
}

impl SetupBackend for Fake {
    fn status(&self) -> SetupStatus {
        SetupStatus {
            folder: None,
            reason: Some("hello".into()),
            suggestions: vec![Suggestion { label: "Documents".into(), path: "C:/Docs/AI Session Vault".into(), has_vault: false }],
            device_name: "test-pc".into(),
        }
    }
    fn has_vault(&self, folder: &Path) -> bool {
        folder.ends_with("has")
    }
    fn create(&self, folder: &Path, passphrase: &str, remember: bool) -> Result<String> {
        self.calls.lock().unwrap().push(format!("create {} {} {remember}", folder.display(), passphrase.len()));
        Ok("AAAA-BBBB".into())
    }
    fn unlock(&self, _folder: &Path, secret: &str, _remember: bool) -> Result<()> {
        if secret == "right right right" {
            Ok(())
        } else {
            Err(anyhow!("That passphrase or recovery key doesn't open this vault."))
        }
    }
    fn join(&self, _folder: &Path, code: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("join {code}"));
        Ok(())
    }
}

fn status_of(r: Result<ureq::Response, ureq::Error>) -> (u16, String) {
    match r {
        Ok(r) => (r.status(), r.into_string().unwrap()),
        Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap()),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn setup_page_api_and_finish() {
    let fake = Arc::new(Fake::default());
    let setup = Setup::start(fake.clone(), 0).unwrap();
    let base = format!("http://127.0.0.1:{}", setup.port);
    let get = |p: &str| status_of(ureq::get(&format!("{base}{p}")).set("X-Token", &setup.token).call());
    let post = |p: &str, b: &str| status_of(ureq::post(&format!("{base}{p}")).set("X-Token", &setup.token).set("Content-Type", "application/json").send_string(b));

    // The page is public, and is the home page while setting up.
    let html = ureq::get(&format!("{base}/")).call().unwrap().into_string().unwrap();
    assert!(html.contains("Set up AI Session Vault"));
    assert!(setup.url.contains("/setup.html?t="));
    assert!(ureq::get(&format!("{base}/js/setup.js")).call().unwrap().into_string().unwrap().contains("/api/setup/create"));

    // The API needs the token.
    assert!(matches!(ureq::get(&format!("{base}/api/setup")).call(), Err(ureq::Error::Status(401, _))));

    let (code, body) = get("/api/setup");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("test-pc") && body.contains("hello"));
    assert!(get("/api/setup/inspect?folder=D%3A%2Fhas").1.contains("true"));
    assert_eq!(get("/api/setup/inspect").0, 400);

    // Bad input is refused with a clear message before the backend is touched.
    let (code, body) = post("/api/setup/create", r#"{"folder":"D:/V","passphrase":"short"}"#);
    assert_eq!(code, 400);
    assert!(body.contains("at least 12"), "{body}");
    assert_eq!(post("/api/setup/create", r#"{"folder":"","passphrase":"correct horse battery"}"#).0, 400);
    assert_eq!(post("/api/setup/join", r#"{"folder":"D:/V","code":""}"#).0, 400);
    assert!(fake.calls.lock().unwrap().is_empty());

    // Valid calls reach the backend; a backend failure is shown as it is.
    let (code, body) = post("/api/setup/create", r#"{"folder":"D:/V","passphrase":"correct horse battery","remember":true}"#);
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("AAAA-BBBB"));
    assert_eq!(fake.calls.lock().unwrap()[0], "create D:/V 21 true");
    let (code, body) = post("/api/setup/unlock", r#"{"folder":"D:/V","passphrase":"nope nope nope"}"#);
    assert_eq!(code, 400);
    assert!(body.contains("doesn't open this vault"), "{body}");
    assert_eq!(post("/api/setup/unlock", r#"{"folder":"D:/V","passphrase":"right right right"}"#).0, 200);
    assert_eq!(post("/api/setup/join", r#"{"folder":"D:/V","code":" X-1 "}"#).0, 200);
    assert_eq!(fake.calls.lock().unwrap()[1], "join X-1");

    // Finishing releases whoever is waiting.
    assert!(!setup.wait_finished(Duration::from_millis(50)));
    assert_eq!(post("/api/setup/finish", "{}").0, 200);
    assert!(setup.wait_finished(Duration::from_secs(2)));
    setup.shutdown();
}

#[test]
fn setup_refuses_foreign_hosts() {
    use std::io::{Read, Write};
    let setup = Setup::start(Arc::new(Fake::default()), 0).unwrap();
    let mut s = std::net::TcpStream::connect(("127.0.0.1", setup.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(s, "GET /api/setup HTTP/1.1\r\nHost: evil.example\r\nX-Token: {}\r\nConnection: close\r\n\r\n", setup.token).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert!(out.starts_with("HTTP/1.1 403"), "{out}");
    setup.shutdown();
}
