//! Sync semantics: merging, conflicts, redaction, settings and text merges.

mod common;

use common::*;
use std::fs;
use vault_core::store::Store;
use vault_core::sync::Options;

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

#[test]
fn text_edits_to_different_lines_are_merged_three_way() {
    let vault = tempfile::tempdir().unwrap();
    let (store, _) = Store::init(vault.path(), PASS, fast()).unwrap();
    let mut a = Device::new();
    let mut b = Device::new();
    let (fa, fb) = (a.env.claude_dir.join("CLAUDE.md"), b.env.claude_dir.join("CLAUDE.md"));
    a.write(&fa, "# Rules\nline one\nline two\nline three\nline four\n");
    a.sync(&store);
    b.sync(&store);
    assert!(fs::read_to_string(&fb).unwrap().contains("line two"));

    // Each device edits a different line before they see each other's change.
    fs::write(&fa, "# Rules\nline one EDITED BY A\nline two\nline three\nline four\n").unwrap();
    filetime::set_file_mtime(&fa, filetime::FileTime::from_unix_time(1_700_000_000, 0)).unwrap();
    fs::write(&fb, "# Rules\nline one\nline two\nline three\nline four EDITED BY B\n").unwrap();
    a.sync(&store);
    let rb = b.sync(&store);
    assert_eq!(rb.merged, 1, "{rb:?}");
    assert!(rb.conflicts.is_empty() && rb.failed.is_empty(), "{rb:?}");
    a.sync(&store);
    for (name, f) in [("A", &fa), ("B", &fb)] {
        let t = fs::read_to_string(f).unwrap();
        assert!(t.contains("EDITED BY A") && t.contains("EDITED BY B"), "{name} lost an edit: {t}");
    }
    // And it settles: nothing left to do.
    let again = b.sync(&store);
    assert_eq!((again.uploaded, again.downloaded, again.merged), (0, 0, 0), "{again:?}");
}
