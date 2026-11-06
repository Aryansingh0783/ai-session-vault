//! On-disk vault layout (safe to put inside OneDrive / Google Drive / Dropbox / Syncthing / a USB stick):
//!
//! ```text
//! vault/
//!   vault.json                   plaintext header: KDF params + wrapped master key slots
//!   objects/ab/<id>              immutable, write-once, encrypted+compressed file versions
//!   devices/<device-id>/manifest encrypted manifest; ONLY that device ever writes it
//! ```
//! Because objects are write-once and every manifest has a single writer, no two machines ever
//! write the same file, so a dumb file-sync tool cannot produce conflicted copies or corruption.

use crate::crypto::{Header, KdfParams, Keys};
use crate::util::{atomic_write, now_iso};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Append-only log (JSONL): merged by line union, never overwritten.
    Log,
    /// Anything else: newest wins, with conflict detection and a safety copy.
    File,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Entry {
    pub kind: Kind,
    pub size: u64,
    pub mtime_ms: i64,
    /// Object id of this version's (portable) content.
    pub hash: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub os: String,
    pub home: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Manifest {
    pub device: DeviceInfo,
    pub seq: u64,
    pub updated: String,
    /// logical path -> entry
    pub entries: BTreeMap<String, Entry>,
    /// project key -> portable project path (or `raw:<encoded-name>`)
    pub projects: BTreeMap<String, String>,
}

pub struct Store {
    pub root: PathBuf,
    pub header: Header,
    pub keys: Keys,
}

impl Store {
    pub fn exists(root: &Path) -> bool {
        root.join("vault.json").is_file()
    }

    /// Create a new vault. Returns the store and the one-time recovery key.
    pub fn init(root: &Path, passphrase: &str, kdf: KdfParams) -> Result<(Store, String)> {
        if Self::exists(root) {
            bail!("a vault already exists in {}", root.display());
        }
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join("devices"))?;
        let (header, keys, recovery) = Header::create(passphrase, kdf)?;
        atomic_write(&root.join("vault.json"), &serde_json::to_vec_pretty(&header)?)?;
        Ok((Store { root: root.to_path_buf(), header, keys }, recovery))
    }

    /// Open with the passphrase or the recovery key.
    pub fn open(root: &Path, secret: &str) -> Result<Store> {
        let raw = fs::read(root.join("vault.json")).with_context(|| format!("no vault in {}", root.display()))?;
        let header: Header = serde_json::from_slice(&raw).context("vault.json is damaged")?;
        let keys = header.unlock(secret)?;
        Ok(Store { root: root.to_path_buf(), header, keys })
    }

    pub fn read_header(root: &Path) -> Result<Header> {
        Ok(serde_json::from_slice(&fs::read(root.join("vault.json"))?)?)
    }

    fn object_path(&self, id: &str) -> PathBuf {
        self.root.join("objects").join(&id[..2]).join(id)
    }

    pub fn has_object(&self, id: &str) -> bool {
        self.object_path(id).is_file()
    }

    /// Store content; returns its id. Existing objects are never rewritten.
    pub fn put(&self, content: &[u8]) -> Result<String> {
        let id = self.keys.object_id(content);
        let p = self.object_path(&id);
        if !p.is_file() {
            atomic_write(&p, &self.keys.pack(&id, content)?)?;
        }
        Ok(id)
    }

    pub fn get(&self, id: &str) -> Result<Vec<u8>> {
        if id.len() < 4 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("invalid object id");
        }
        let blob = fs::read(self.object_path(id)).map_err(|e| anyhow!("object {}: {e} (is the folder still syncing?)", &id[..8]))?;
        let data = self.keys.unpack(id, &blob)?;
        if self.keys.object_id(&data) != id {
            bail!("object {} failed its integrity check", &id[..8]);
        }
        Ok(data)
    }

    fn manifest_path(&self, device_id: &str) -> PathBuf {
        self.root.join("devices").join(device_id).join("manifest")
    }

    pub fn write_manifest(&self, m: &Manifest) -> Result<()> {
        let json = serde_json::to_vec(m)?;
        let aad = format!("manifest:{}", m.device.id);
        atomic_write(&self.manifest_path(&m.device.id), &self.keys.pack(&aad, &json)?)
    }

    pub fn read_manifest(&self, device_id: &str) -> Result<Option<Manifest>> {
        let p = self.manifest_path(device_id);
        if !p.is_file() {
            return Ok(None);
        }
        let aad = format!("manifest:{device_id}");
        let json = self.keys.unpack(&aad, &fs::read(&p)?)?;
        Ok(Some(serde_json::from_slice(&json)?))
    }

    /// All device manifests. Unreadable ones (still syncing / damaged) are returned as warnings.
    pub fn read_all_manifests(&self) -> Result<(Vec<Manifest>, Vec<String>)> {
        let mut out = vec![];
        let mut warn = vec![];
        let dir = self.root.join("devices");
        if !dir.is_dir() {
            return Ok((out, warn));
        }
        for e in fs::read_dir(dir)? {
            let e = e?;
            if !e.file_type()?.is_dir() {
                continue;
            }
            let id = e.file_name().to_string_lossy().into_owned();
            match self.read_manifest(&id) {
                Ok(Some(m)) => out.push(m),
                Ok(None) => {}
                Err(err) => warn.push(format!("device {id}: manifest unreadable ({err})")),
            }
        }
        out.sort_by(|a, b| a.device.id.cmp(&b.device.id));
        Ok((out, warn))
    }

    pub fn save_header(&self) -> Result<()> {
        atomic_write(&self.root.join("vault.json"), &serde_json::to_vec_pretty(&self.header)?)
    }

    pub fn new_manifest(device: DeviceInfo) -> Manifest {
        Manifest { device, seq: 0, updated: now_iso(), ..Default::default() }
    }

    /// Delete objects no manifest references. Returns (count, bytes).
    pub fn gc(&self, manifests: &[Manifest]) -> Result<(usize, u64)> {
        let live: std::collections::HashSet<&str> =
            manifests.iter().flat_map(|m| m.entries.values().map(|e| e.hash.as_str())).collect();
        let (mut n, mut bytes) = (0, 0);
        let root = self.root.join("objects");
        if !root.is_dir() {
            return Ok((0, 0));
        }
        for shard in fs::read_dir(root)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for f in fs::read_dir(shard.path())? {
                let f = f?;
                let name = f.file_name().to_string_lossy().into_owned();
                if name.ends_with(".vaulttmp") || live.contains(name.as_str()) {
                    continue;
                }
                bytes += f.metadata().map(|m| m.len()).unwrap_or(0);
                fs::remove_file(f.path())?;
                n += 1;
            }
        }
        Ok((n, bytes))
    }
}
