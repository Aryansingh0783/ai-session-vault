//! Two simulated devices sharing one vault folder.

use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use vault_core::crypto::KdfParams;
use vault_core::env::{Env, State};
use vault_core::paths::encode_project;
use vault_core::store::Store;
use vault_core::sync::{sync, Options};

const PASS: &str = "correct horse battery staple";

struct Device {
    _dir: TempDir,
    env: Env,
    state: State,
}

impl Device {
    fn new() -> Device {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::detect(Some(dir.path())).unwrap();
        let state = State::load(&env);
        Device { _dir: dir, env, state }
    }
    fn home(&self) -> &Path {
        &self.env.home
    }
    fn project_dir(&self, name: &str) -> PathBuf {
        self.home().join(name)
    }
    fn session_file(&self, project: &str, id: &str) -> PathBuf {
        self.env.claude_dir.join("projects").join(encode_project(&self.project_dir(project).to_string_lossy())).join(format!("{id}.jsonl"))
    }
    fn write(&self, p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }
    fn session_line(&self, project: &str, n: u32) -> String {
        let cwd = self.project_dir(project).to_string_lossy().replace('\\', "\\\\");
        format!("{{\"type\":\"user\",\"uuid\":\"u{n}\",\"cwd\":\"{cwd}\",\"message\":\"msg {n}\"}}\n")
    }
    fn sync(&mut self, store: &Store) -> vault_core::sync::Report {
        sync(&self.env, store, &mut self.state, &Options::default(), &|_| {}).unwrap()
    }
}

fn fast() -> KdfParams {
    KdfParams { m_kib: 64, t: 1, p: 1 }
}

#[test]
fn two_devices_share_sessions_and_merge_divergence() {
    let vault = tempfile::tempdir().unwrap();
    let (store, recovery) = Store::init(vault.path(), PASS, fast()).unwrap();
    assert!(recovery.len() > 20);

    // Device A has a project with one session and some config.
    let mut a = Device::new();
    fs::create_dir_all(a.project_dir("app")).unwrap();
    a.write(&a.session_file("app", "s1"), &(a.session_line("app", 1) + &a.session_line("app", 2)));
    a.write(&a.env.claude_dir.join("CLAUDE.md"), "rules v1");
    a.write(&a.env.claude_dir.join(".credentials.json"), "SECRET-TOKEN");
    let r = a.sync(&store);
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert!(r.uploaded >= 2);

    // Nothing in the vault is readable without the key, and logins never leave the device.
    let mut all = vec![];
    for e in walkdir::WalkDir::new(vault.path()).into_iter().flatten().filter(|e| e.file_type().is_file()) {
        all.extend(fs::read(e.path()).unwrap());
    }
    let hay = String::from_utf8_lossy(&all);
    assert!(!hay.contains("msg 1") && !hay.contains("SECRET-TOKEN") && !hay.contains("rules v1"));

    // Device B (different home) receives it, re-homed.
    let mut b = Device::new();
    fs::create_dir_all(b.project_dir("app")).unwrap();
    let store_b = Store::open(vault.path(), PASS).unwrap();
    let r = b.sync(&store_b);
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    let got = fs::read_to_string(b.session_file("app", "s1")).unwrap();
    assert!(got.contains("msg 2"));
    assert!(got.contains(&b.project_dir("app").to_string_lossy().replace('\\', "\\\\")), "cwd should be rewritten for B: {got}");
    assert!(!got.contains(&a.project_dir("app").to_string_lossy().replace('\\', "\\\\")));
    assert_eq!(fs::read_to_string(b.env.claude_dir.join("CLAUDE.md")).unwrap(), "rules v1");
    assert!(!b.env.claude_dir.join(".credentials.json").exists());

    // Both devices continue the same session independently.
    let (pa, pb) = (a.session_file("app", "s1"), b.session_file("app", "s1"));
    let mut sa = fs::read_to_string(&pa).unwrap();
    sa.push_str(&a.session_line("app", 3));
    fs::write(&pa, sa).unwrap();
    let mut sb = fs::read_to_string(&pb).unwrap();
    sb.push_str(&b.session_line("app", 4));
    fs::write(&pb, sb).unwrap();
    a.sync(&store);
    b.sync(&store_b);
    a.sync(&store);
    for d in [&a, &b] {
        let s = fs::read_to_string(d.session_file("app", "s1")).unwrap();
        for n in 1..=4 {
            assert!(s.contains(&format!("msg {n}")), "device missing msg {n}: {s}");
        }
        assert_eq!(s.matches("msg 2").count(), 1, "no duplicated lines");
    }

    // A running sync again changes nothing.
    let r = a.sync(&store);
    assert_eq!((r.uploaded, r.downloaded, r.merged), (0, 0, 0), "{r:?}");
}

#[test]
fn conflicting_config_edits_are_reported_and_loser_saved() {
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let mut a = Device::new();
    let mut b = Device::new();
    let (fa, fb) = (a.env.claude_dir.join("CLAUDE.md"), b.env.claude_dir.join("CLAUDE.md"));
    a.write(&fa, "base");
    a.sync(&store);
    b.sync(&store);
    assert_eq!(fs::read_to_string(&fb).unwrap(), "base");

    // Both edit; B's edit is clearly newer.
    fs::write(&fa, "edit from A").unwrap();
    filetime::set_file_mtime(&fa, filetime::FileTime::from_unix_time(1_700_000_000, 0)).unwrap();
    fs::write(&fb, "edit from B").unwrap();
    a.sync(&store);
    // B notices both sides changed since the last sync and says so.
    let r = b.sync(&store);
    assert_eq!(fs::read_to_string(&fb).unwrap(), "edit from B");
    assert_eq!(r.conflicts.len(), 1, "{r:?}");
    // A then picks up the newer version; its own edit is kept as a safety copy.
    let r = a.sync(&store);
    assert_eq!(fs::read_to_string(&fa).unwrap(), "edit from B");
    assert!(r.safety_copies >= 1, "A's losing edit must be saved");
    let saved: Vec<_> = walkdir::WalkDir::new(a.env.safety_root()).into_iter().flatten().filter(|e| e.file_type().is_file()).collect();
    assert_eq!(fs::read_to_string(saved[0].path()).unwrap(), "edit from A");
}

#[test]
fn wrong_passphrase_is_rejected_and_tampering_detected() {
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    assert!(Store::open(vault.path(), "not the passphrase").is_err());
    let id = store.put(b"hello").unwrap();
    assert_eq!(store.get(&id).unwrap(), b"hello");
    let p = vault.path().join("objects").join(&id[..2]).join(&id);
    let mut raw = fs::read(&p).unwrap();
    let last = raw.len() - 1;
    raw[last] ^= 1;
    fs::write(&p, raw).unwrap();
    assert!(store.get(&id).is_err());
}

#[test]
fn redact_mode_keeps_secrets_off_other_devices() {
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let mut a = Device::new();
    fs::create_dir_all(a.project_dir("app")).unwrap();
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let line = a.session_line("app", 1).replace("msg 1", &format!("key {secret}"));
    a.write(&a.session_file("app", "s1"), &line);
    let opts = Options { redact: true, ..Options::default() };
    vault_core::sync::sync(&a.env, &store, &mut a.state, &opts, &|_| {}).unwrap();
    let mut b = Device::new();
    fs::create_dir_all(b.project_dir("app")).unwrap();
    vault_core::sync::sync(&b.env, &store, &mut b.state, &opts, &|_| {}).unwrap();
    let got = fs::read_to_string(b.session_file("app", "s1")).unwrap();
    assert!(got.contains("REDACTED") && !got.contains(secret), "{got}");
    // The original stays untouched on the device that has it.
    assert!(fs::read_to_string(a.session_file("app", "s1")).unwrap().contains(secret));
}

#[test]
fn imports_a_python_v1_vault() {
    let v1 = tempfile::tempdir().unwrap();
    let pdir = v1.path().join("claude-code/projects/HOME-code-app-12345678");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(pdir.join("_vault_meta.json"), r#"{"portable_path":"~/app","encoded_name":"x"}"#).unwrap();
    fs::write(pdir.join("old.jsonl"), "{\"cwd\":\"~/app\",\"message\":\"from python\"}\n").unwrap();
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let r = vault_core::legacy::import_v1(&store, v1.path()).unwrap();
    assert_eq!((r.files, r.projects), (1, 1));
    let mut b = Device::new();
    fs::create_dir_all(b.project_dir("app")).unwrap();
    b.sync(&store);
    let got = fs::read_to_string(b.session_file("app", "old")).unwrap();
    assert!(got.contains("from python") && got.contains(&b.project_dir("app").to_string_lossy().replace('\\', "\\\\")), "{got}");
}

#[test]
fn growing_log_uploads_only_its_new_tail_and_big_files_roundtrip() {
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let mut a = Device::new();
    fs::create_dir_all(a.project_dir("app")).unwrap();
    // ~6 MiB session log of incompressible-ish lines.
    let mut log = String::new();
    let mut n = 0u32;
    while log.len() < 6 * 1024 * 1024 {
        n += 1;
        log.push_str(&a.session_line("app", n).replace("msg", &format!("{:x}{:x}", n.wrapping_mul(2654435761), n.wrapping_mul(40503))));
    }
    let path = a.session_file("app", "big");
    a.write(&path, &log);
    // A 5 MiB plain file (multi-chunk, streamed on restore).
    let blob: Vec<u8> = (0..5 * 1024 * 1024u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    fs::create_dir_all(a.env.claude_dir.join("rules")).unwrap();
    fs::write(a.env.claude_dir.join("rules").join("blob.bin"), &blob).unwrap();
    let r1 = a.sync(&store);
    assert!(r1.bytes_uploaded > 5 * 1024 * 1024, "first upload sends everything: {r1:?}");

    // Append one line: only the changed tail chunk(s) should be new.
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    use std::io::Write;
    f.write_all(a.session_line("app", 999_999).as_bytes()).unwrap();
    drop(f);
    let r2 = a.sync(&store);
    assert_eq!(r2.uploaded, 1, "{r2:?}");
    assert!(r2.bytes_uploaded <= 1024 * 1024, "only the tail chunk uploads, got {} bytes", r2.bytes_uploaded);

    // Another device receives the big log intact (verified chunk by chunk).
    let mut b = Device::new();
    fs::create_dir_all(b.project_dir("app")).unwrap();
    let store_b = Store::open(vault.path(), PASS).unwrap();
    let rb = b.sync(&store_b);
    assert!(rb.failed.is_empty(), "{rb:?}");
    let got = fs::read_to_string(b.session_file("app", "big")).unwrap();
    assert!(got.contains("msg 999999") || got.contains(":\"msg 999999\""), "tail line missing");
    assert_eq!(got.lines().count(), n as usize + 1);
}

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
    let ea = Engine::start(a.env.clone(), store, cfg.clone()).unwrap();
    let eb = Engine::start(b.env.clone(), Store::open(vault.path(), PASS).unwrap(), cfg).unwrap();

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

#[test]
fn verify_finds_missing_and_corrupt_objects_and_devices_can_be_forgotten() {
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let mut a = Device::new();
    a.write(&a.env.claude_dir.join("CLAUDE.md"), "hello");
    a.write(&a.env.claude_dir.join("history.jsonl"), "{\"a\":1}\n");
    a.sync(&store);
    let (ms, _) = store.read_all_manifests().unwrap();
    assert!(store.verify(&ms, true).missing.is_empty() && store.verify(&ms, true).corrupt.is_empty());

    let ids: Vec<String> = ms[0].entries.values().map(|e| e.hash.clone()).collect();
    let p0 = vault.path().join("objects").join(&ids[0][..2]).join(&ids[0]);
    let p1 = vault.path().join("objects").join(&ids[1][..2]).join(&ids[1]);
    fs::remove_file(&p0).unwrap();
    let mut raw = fs::read(&p1).unwrap();
    let last = raw.len() - 1;
    raw[last] ^= 0xff;
    fs::write(&p1, raw).unwrap();
    assert_eq!(store.verify(&ms, false).missing.len(), 1);
    let deep = store.verify(&ms, true);
    assert_eq!((deep.missing.len(), deep.corrupt.len()), (1, 1));

    store.remove_device(&ms[0].device.id).unwrap();
    assert!(store.read_all_manifests().unwrap().0.is_empty());
    assert!(store.remove_device("../x").is_err());
}

#[test]
fn machine_specific_settings_keys_stay_local() {
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let mut a = Device::new();
    let mut b = Device::new();
    let (sa, sb) = (a.env.claude_dir.join("settings.json"), b.env.claude_dir.join("settings.json"));
    a.write(&sa, r#"{"theme":"dark","statusLine":{"command":"C:/a/status.cmd"},"model":"opus"}"#);
    b.write(&sb, r#"{"statusLine":{"command":"/home/b/status.sh"}}"#);
    a.sync(&store);
    // The vault copy never contains A's machine-specific command.
    let (ms, _) = store.read_all_manifests().unwrap();
    let e = &ms[0].entries["claude-code/config/settings.json"];
    let shared = String::from_utf8(store.read_all(e).unwrap()).unwrap();
    assert!(shared.contains("\"theme\"") && !shared.contains("status.cmd"), "{shared}");

    let rb = b.sync(&Store::open(vault.path(), PASS).unwrap());
    assert!(rb.failed.is_empty(), "{rb:?}");
    let got: serde_json::Value = serde_json::from_str(&fs::read_to_string(&sb).unwrap()).unwrap();
    assert_eq!(got["theme"], "dark");
    assert_eq!(got["model"], "opus");
    assert_eq!(got["statusLine"]["command"], "/home/b/status.sh", "B keeps its own statusLine");
    // Stable afterwards: no phantom conflict or re-upload.
    let again = b.sync(&Store::open(vault.path(), PASS).unwrap());
    assert_eq!((again.uploaded, again.downloaded), (0, 0), "{again:?}");
    assert!(again.conflicts.is_empty());
}

#[test]
fn block_secrets_refuses_to_upload_logs_with_keys() {
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let mut a = Device::new();
    fs::create_dir_all(a.project_dir("app")).unwrap();
    let dirty = a.session_line("app", 1).replace("msg 1", "token ghp_abcdefghijklmnopqrstuvwxyz0123456789");
    a.write(&a.session_file("app", "leaky"), &dirty);
    a.write(&a.session_file("app", "clean"), &a.session_line("app", 2));
    let opts = Options { block_secrets: true, ..Options::default() };
    let r = vault_core::sync::sync(&a.env, &store, &mut a.state, &opts, &|_| {}).unwrap();
    assert_eq!(r.blocked.len(), 1, "{r:?}");
    assert!(r.blocked[0].contains("leaky"));
    let (ms, _) = store.read_all_manifests().unwrap();
    assert!(ms[0].entries.keys().any(|k| k.ends_with("clean.jsonl")));
    assert!(!ms[0].entries.keys().any(|k| k.ends_with("leaky.jsonl")));
}
