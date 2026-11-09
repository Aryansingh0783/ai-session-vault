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

mod writers;
pub use writers::{ChunkWriter, HashWriter, Stored};

use crate::crypto::{Header, KdfParams, Keys};

use crate::util::{atomic_write, now_iso};

use anyhow::{anyhow, bail, Context, Result};

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use std::fs;

use std::io::{self, Write};

use std::path::{Path, PathBuf};

mod enroll;
mod manifests;

/// Files are stored as fixed 1 MiB chunks, so a growing log only uploads its new tail.
/// Unreferenced objects must be at least this old before `gc` removes them.
pub const GC_GRACE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

pub const CHUNK: usize = 1024 * 1024;

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
    /// Keyed hash of this version's whole (portable) content.
    pub hash: String,
    /// Chunk object ids when the content spans more than one chunk (empty = a single object named `hash`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<String>,
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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SlotFile {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub created: String,
    /// Enrollment slots only: unix ms after which the code stops working.
    #[serde(default)]
    pub expires_ms: i64,
    pub wrapped: String,
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

    pub fn object_path(&self, id: &str) -> PathBuf {
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

    /// Chunk ids that make up an entry's content, in order.
    pub fn chunk_ids(e: &Entry) -> Vec<&str> {
        if e.chunks.is_empty() {
            vec![e.hash.as_str()]
        } else {
            e.chunks.iter().map(|c| c.as_str()).collect()
        }
    }

    /// Stream an entry's content into `out`, verifying every chunk and the whole-file hash.
    pub fn read_to(&self, e: &Entry, out: &mut dyn Write) -> Result<()> {
        let mut h = self.keys.hasher();
        for id in Self::chunk_ids(e) {
            let data = self.get(id)?;
            h.update(&data);
            out.write_all(&data)?;
        }
        if h.finalize().to_hex().as_str() != e.hash {
            bail!("file failed its integrity check");
        }
        Ok(())
    }

    pub fn read_all(&self, e: &Entry) -> Result<Vec<u8>> {
        let mut v = Vec::with_capacity(e.size as usize);
        self.read_to(e, &mut v)?;
        Ok(v)
    }

    pub fn save_header(&self) -> Result<()> {
        atomic_write(&self.root.join("vault.json"), &serde_json::to_vec_pretty(&self.header)?)
    }
}

#[derive(Default, Debug, Serialize)]
pub struct VerifyReport {
    pub objects: usize,
    pub missing: Vec<String>,
    pub corrupt: Vec<String>,
}
