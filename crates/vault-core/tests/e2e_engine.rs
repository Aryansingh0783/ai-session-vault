//! The always-on engine and live databases.

mod common;

use common::*;
use std::fs;
use vault_core::store::Store;

#[test]
fn engine_syncs_automatically_between_two_devices() {
    use std::time::{Duration, Instant};
    use vault_core::engine::{Engine, EngineConfig, Event};
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let a = Device::new();
    let b = Device::new();
    fs::create_dir_all(a.project_dir("app")).unwrap();
    fs::create_dir_all(b.project_dir("app")).unwrap();
    fs::create_dir_all(a.env.claude_dir.join("projects")).unwrap();
    fs::create_dir_all(b.env.claude_dir.join("projects")).unwrap();
    let cfg = EngineConfig { quiet: Duration::from_millis(300), full_sync_every: Duration::from_secs(3600), ..Default::default() };
    let ea = Engine::start(a.env.clone(), std::sync::Arc::new(store), cfg.clone()).unwrap();
    let eb = Engine::start(b.env.clone(), std::sync::Arc::new(Store::open(vault.path(), PASS).unwrap()), cfg).unwrap();

    let wait_finished = |e: &Engine, secs: u64| {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if let Ok(Event::SyncFinished { .. }) = e.events.recv_timeout(Duration::from_millis(200)) {
                return true;
            }
        }
        false
    };
    assert!(wait_finished(&ea, 20) && wait_finished(&eb, 20), "initial syncs");

    // A new session appears on A; B should receive it without anyone pressing a button.
    a.write(&a.session_file("app", "live"), &a.session_line("app", 7));
    let target = b.session_file("app", "live");
    let end = Instant::now() + Duration::from_secs(40);
    while !target.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(target.exists(), "B never received the session");
    assert!(fs::read_to_string(&target).unwrap().contains("msg 7"));
    ea.shutdown();
    eb.shutdown();
}

#[test]
fn live_wal_database_is_snapshotted_consistently() {
    use rusqlite::Connection;
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let mut a = Device::new();
    fs::create_dir_all(&a.env.codex_dir).unwrap();
    let db = a.env.codex_dir.join("state_5.sqlite");
    let conn = Connection::open(&db).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute("CREATE TABLE threads (id INTEGER PRIMARY KEY, title TEXT)", []).unwrap();
    for i in 0..50 {
        conn.execute("INSERT INTO threads (title) VALUES (?1)", [format!("thread {i}")]).unwrap();
    }
    // The app still has the database open and its -wal file is non-empty.
    assert!(fs::metadata(db.with_extension("sqlite-wal")).map(|m| m.len() > 0).unwrap_or(false));
    let r = a.sync(&store);
    assert!(r.failed.is_empty() && r.busy.is_empty(), "{r:?}");
    assert_eq!(r.uploaded, 1, "{r:?}");

    let mut b = Device::new();
    let rb = b.sync(&Store::open(vault.path(), PASS).unwrap());
    assert!(rb.failed.is_empty(), "{rb:?}");
    let copy = Connection::open(b.env.codex_dir.join("state_5.sqlite")).unwrap();
    let n: i64 = copy.query_row("SELECT count(*) FROM threads", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 50);
    drop(conn);
}
