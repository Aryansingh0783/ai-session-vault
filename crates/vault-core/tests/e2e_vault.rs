//! The vault itself: keys, integrity, devices, overview, import and cleanup.

mod common;

use common::*;
use std::fs;
use vault_core::env::{Env, State};
use vault_core::store::Store;

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
fn overview_summarises_projects_for_a_ui() {
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let mut a = Device::new();
    fs::create_dir_all(a.project_dir("app")).unwrap();
    a.write(&a.session_file("app", "s1"), &a.session_line("app", 1));
    a.write(&a.session_file("app", "s2"), &a.session_line("app", 2));
    a.sync(&store);
    let mut b = Device::new();
    fs::create_dir_all(b.project_dir("app")).unwrap();
    b.sync(&store);
    let o = vault_core::overview::overview(&b.env, &store, &[]).unwrap();
    assert_eq!((o.totals.devices, o.totals.projects, o.totals.sessions), (2, 1, 2), "{o:?}");
    let p = &o.projects[0];
    assert_eq!(p.portable_path, "~/app");
    assert!(p.exists_locally && p.devices.len() == 2, "{p:?}");
    assert_eq!(o.devices.iter().filter(|d| d.this_device).count(), 1);
}

#[test]
fn a_device_can_join_with_a_one_time_code_and_be_revoked() {
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let secret_id = store.put(b"top secret session").unwrap();

    let code = store.create_enrollment(600).unwrap();
    let (joined, cred) = Store::join(vault.path(), &code, "laptop").unwrap();
    assert_eq!(joined.get(&secret_id).unwrap(), b"top secret session");
    // The code works once.
    assert!(Store::join(vault.path(), &code, "other").is_err());
    // Later the device opens the vault with just its credential - no passphrase.
    let again = Store::open_with_device(vault.path(), &cred).unwrap();
    assert_eq!(again.get(&secret_id).unwrap(), b"top secret session");
    assert!(Store::open_with_device(vault.path(), "deadbeef:AAAA-AAAA").is_err());
    assert_eq!(store.device_slots().len(), 1);

    // Revoking removes its access slot.
    let slot = store.device_slots().remove(0);
    assert_eq!(slot.name, "laptop");
    store.revoke_device_slot(&slot.id).unwrap();
    assert!(Store::open_with_device(vault.path(), &cred).is_err());

    // Expired codes are refused.
    let stale = store.create_enrollment(0).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(Store::join(vault.path(), &stale, "late").err().unwrap().to_string().contains("expired"));
}

#[test]
fn gc_keeps_fresh_unreferenced_objects_and_removes_old_ones() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let (store, _rec) = Store::init(&vault, PASS, fast()).unwrap();
    // An object no manifest lists, like another device's in-flight upload.
    let id = store.put(b"in flight").unwrap();
    let (ms, warns) = store.read_all_manifests().unwrap();
    assert!(warns.is_empty());
    assert_eq!(store.gc(&ms).unwrap().0, 0, "a fresh object must survive gc");
    assert!(store.get(&id).is_ok());
    // Once it is older than the grace period it is garbage.
    let path = store.object_path(&id);
    let old = std::time::SystemTime::now() - vault_core::store::GC_GRACE - std::time::Duration::from_secs(60);
    fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
    assert_eq!(store.gc(&ms).unwrap().0, 1);
    assert!(store.get(&id).is_err());
}

#[test]
fn excluded_projects_are_never_uploaded() {
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".ai-vault")).unwrap();
    vault_core::env::set_config(&dir.path().join(".ai-vault"), "exclude_projects", serde_json::json!(["~/noise"])).unwrap();
    let env = Env::detect(Some(dir.path())).unwrap();
    assert_eq!(env.exclude_projects, ["~/noise"]);
    let mut a = Device { env, state: State::default(), _dir: dir };
    a.state = State::load(&a.env);
    for p in ["noise", "keep"] {
        fs::create_dir_all(a.project_dir(p)).unwrap();
        a.write(&a.session_file(p, "s1"), &a.session_line(p, 1));
    }
    a.sync(&store);
    let (ms, _) = store.read_all_manifests().unwrap();
    let keys: Vec<_> = ms[0].entries.keys().collect();
    assert!(keys.iter().any(|k| k.contains("keep")) && !keys.iter().any(|k| k.contains("noise")), "{keys:?}");
}
