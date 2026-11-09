//! Shared helpers: simulated devices sharing one vault folder.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use vault_core::crypto::KdfParams;
use vault_core::env::{Env, State};
use vault_core::paths::encode_project;
use vault_core::store::Store;
use vault_core::sync::{sync, Options};

pub const PASS: &str = "correct horse battery staple";

pub struct Device {
    pub _dir: TempDir,
    pub env: Env,
    pub state: State,
}

impl Device {
    pub fn new() -> Device {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::detect(Some(dir.path())).unwrap();
        let state = State::load(&env);
        Device { _dir: dir, env, state }
    }
    pub fn home(&self) -> &Path {
        &self.env.home
    }
    pub fn project_dir(&self, name: &str) -> PathBuf {
        self.home().join(name)
    }
    pub fn session_file(&self, project: &str, id: &str) -> PathBuf {
        self.env.claude_dir.join("projects").join(encode_project(&self.project_dir(project).to_string_lossy())).join(format!("{id}.jsonl"))
    }
    pub fn write(&self, p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }
    pub fn session_line(&self, project: &str, n: u32) -> String {
        let cwd = self.project_dir(project).to_string_lossy().replace('\\', "\\\\");
        format!("{{\"type\":\"user\",\"uuid\":\"u{n}\",\"cwd\":\"{cwd}\",\"message\":\"msg {n}\"}}\n")
    }
    pub fn sync(&mut self, store: &Store) -> vault_core::sync::Report {
        sync(&self.env, store, &mut self.state, &Options::default(), &|_| {}).unwrap()
    }
}

pub fn fast() -> KdfParams {
    KdfParams { m_kib: 64, t: 1, p: 1 }
}
