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
