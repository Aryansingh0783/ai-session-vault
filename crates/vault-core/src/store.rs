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
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
    fn slots_dir(root: &Path) -> PathBuf {
        root.join("slots")
    }

    fn read_slots(root: &Path, kind: &str) -> Vec<SlotFile> {
        let Ok(rd) = fs::read_dir(Self::slots_dir(root)) else { return vec![] };
        rd.flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{kind}-")))
            .filter_map(|e| fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<SlotFile>(&b).ok()))
            .filter(|s| s.kind == kind)
            .collect()
    }

    /// Create a one-time code that lets another device join without the passphrase.
    /// Slots are separate single-writer files, so enrolling never touches `vault.json`.
    pub fn create_enrollment(&self, ttl_secs: u64) -> Result<String> {
        use rand::{rngs::OsRng, RngCore};
        let mut raw = [0u8; 16];
        OsRng.fill_bytes(&mut raw);
        let id = hex::encode(&raw[..6]);
        let kek = crate::crypto::kek_from_enrollment(&raw);
        let slot = SlotFile {
            id: id.clone(),
            kind: "enroll".into(),
            name: String::new(),
            created: now_iso(),
            expires_ms: crate::util::now_ms() + (ttl_secs as i64) * 1000,
            wrapped: self.keys.wrap_master(&kek, &format!("slot:enroll:{id}")),
        };
        atomic_write(&Self::slots_dir(&self.root).join(format!("enroll-{id}.json")), &serde_json::to_vec_pretty(&slot)?)?;
        Ok(crate::crypto::format_code(&raw))
    }

    /// Join with an enrollment code. Returns the opened store plus this device's credential
    /// (`slot-id:key`), and consumes the code.
    pub fn join(root: &Path, code: &str, device_name: &str) -> Result<(Store, String)> {
        use rand::{rngs::OsRng, RngCore};
        let raw = crate::crypto::parse_code(code)?;
        let kek = crate::crypto::kek_from_enrollment(&raw);
        let now = crate::util::now_ms();
        for s in Self::read_slots(root, "enroll") {
            let Ok(keys) = Keys::from_wrapped(&kek, &format!("slot:enroll:{}", s.id), &s.wrapped) else { continue };
            if s.expires_ms < now {
                let _ = fs::remove_file(Self::slots_dir(root).join(format!("enroll-{}.json", s.id)));
                bail!("that code has expired - run `avault enroll` again on a device that has the vault open");
            }
            let mut dk = [0u8; 32];
            OsRng.fill_bytes(&mut dk);
            let mut idb = [0u8; 6];
            OsRng.fill_bytes(&mut idb);
            let id = hex::encode(idb);
            let slot = SlotFile {
                id: id.clone(),
                kind: "device".into(),
                name: device_name.into(),
                created: now_iso(),
                expires_ms: 0,
                wrapped: keys.wrap_master(&crate::crypto::kek_from_device(&dk), &format!("slot:device:{id}")),
            };
            atomic_write(&Self::slots_dir(root).join(format!("device-{id}.json")), &serde_json::to_vec_pretty(&slot)?)?;
            let _ = fs::remove_file(Self::slots_dir(root).join(format!("enroll-{}.json", s.id))); // one-time use
            let header = Self::read_header(root)?;
            return Ok((Store { root: root.to_path_buf(), header, keys }, format!("{id}:{}", crate::crypto::format_code(&dk))));
        }
        bail!("that code is not valid (already used, or the vault folder hasn't finished syncing)")
    }

    /// Open with a device credential produced by [`Store::join`].
    pub fn open_with_device(root: &Path, credential: &str) -> Result<Store> {
        let (id, key) = credential.split_once(':').ok_or_else(|| anyhow!("bad device credential"))?;
        let raw = crate::crypto::parse_code(key)?;
        let slot = Self::read_slots(root, "device").into_iter().find(|s| s.id == id).ok_or_else(|| anyhow!("this device was removed from the vault"))?;
        let keys = Keys::from_wrapped(&crate::crypto::kek_from_device(&raw), &format!("slot:device:{id}"), &slot.wrapped)?;
        Ok(Store { root: root.to_path_buf(), header: Self::read_header(root)?, keys })
    }

    pub fn device_slots(&self) -> Vec<SlotFile> {
        Self::read_slots(&self.root, "device")
    }

    /// Remove a device's access slot. (A device that already copied the master key out of memory or
    /// disk keeps what it had; changing every key needs a full re-encrypt, which is not implemented.)
    pub fn revoke_device_slot(&self, id: &str) -> Result<()> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("invalid slot id");
        }
        let p = Self::slots_dir(&self.root).join(format!("device-{id}.json"));
        if !p.is_file() {
            bail!("no such device slot");
        }
        fs::remove_file(p)?;
        Ok(())
    }

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
        if e.chunks.is_empty() { vec![e.hash.as_str()] } else { e.chunks.iter().map(|c| c.as_str()).collect() }
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

    /// Check that every object the manifests reference exists (and, with `deep`, decrypts and matches
    /// its id). A cloud folder that is still syncing, or a damaged copy, shows up here.
    pub fn verify(&self, manifests: &[Manifest], deep: bool) -> VerifyReport {
        let mut rep = VerifyReport::default();
        let mut seen = std::collections::HashSet::new();
        for m in manifests {
            for (logical, e) in &m.entries {
                for id in Self::chunk_ids(e) {
                    if !seen.insert(id.to_string()) {
                        continue;
                    }
                    rep.objects += 1;
                    if !self.has_object(id) {
                        rep.missing.push(format!("{} ({}): {}", logical, m.device.name, &id[..12]));
                    } else if deep {
                        if let Err(err) = self.get(id) {
                            rep.corrupt.push(format!("{} ({}): {} - {err}", logical, m.device.name, &id[..12]));
                        }
                    }
                }
            }
        }
        rep
    }

    /// Forget a device (its manifest). Its data is freed by `gc` unless another device has the same files.
    pub fn remove_device(&self, device_id: &str) -> Result<()> {
        if device_id.is_empty() || device_id.contains(['/', '\\', '.']) {
            bail!("invalid device id");
        }
        let dir = self.root.join("devices").join(device_id);
        if !dir.is_dir() {
            bail!("no such device");
        }
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    pub fn save_header(&self) -> Result<()> {
        atomic_write(&self.root.join("vault.json"), &serde_json::to_vec_pretty(&self.header)?)
    }

    pub fn new_manifest(device: DeviceInfo) -> Manifest {
        Manifest { device, seq: 0, updated: now_iso(), ..Default::default() }
    }

    /// Delete objects no manifest references. Returns (count, bytes).
    /// Objects younger than `GC_GRACE` are kept: another device may be mid-upload, with its new
    /// objects not yet listed in its manifest (manifests are checkpointed every 100 uploads).
    pub fn gc(&self, manifests: &[Manifest]) -> Result<(usize, u64)> {
        let now = std::time::SystemTime::now();
        let live: std::collections::HashSet<&str> = manifests
            .iter()
            .flat_map(|m| m.entries.values().flat_map(|e| std::iter::once(e.hash.as_str()).chain(e.chunks.iter().map(|c| c.as_str()))))
            .collect();
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
                let age = f.metadata().and_then(|m| m.modified()).ok().and_then(|t| now.duration_since(t).ok());
                if age.is_none_or(|a| a < GC_GRACE) {
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

/// Computes the content hash without storing anything (dry runs, change detection).
pub struct HashWriter {
    hasher: blake3::Hasher,
    pub size: u64,
}

impl HashWriter {
    pub fn new(store: &Store) -> Self {
        HashWriter { hasher: store.keys.hasher(), size: 0 }
    }
    pub fn finish(self) -> (String, u64) {
        (self.hasher.finalize().to_hex().to_string(), self.size)
    }
}

impl Write for HashWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.hasher.update(b);
        self.size += b.len() as u64;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Splits a byte stream into encrypted chunk objects. Chunks the vault already has are not re-uploaded.
pub struct ChunkWriter<'a> {
    store: &'a Store,
    buf: Vec<u8>,
    hasher: blake3::Hasher,
    chunks: Vec<String>,
    size: u64,
    new_bytes: u64,
}

pub struct Stored {
    pub hash: String,
    pub chunks: Vec<String>,
    pub size: u64,
    /// Plaintext bytes of chunks that were new to the vault.
    pub new_bytes: u64,
}

impl<'a> ChunkWriter<'a> {
    pub fn new(store: &'a Store) -> Self {
        ChunkWriter { store, buf: Vec::with_capacity(CHUNK), hasher: store.keys.hasher(), chunks: vec![], size: 0, new_bytes: 0 }
    }

    fn flush_chunk(&mut self, n: usize) -> io::Result<()> {
        let chunk: Vec<u8> = self.buf.drain(..n).collect();
        let id = self.store.keys.object_id(&chunk);
        if !self.store.has_object(&id) {
            self.store.put(&chunk).map_err(|e| io::Error::other(e.to_string()))?;
            self.new_bytes += n as u64;
        }
        self.chunks.push(id);
        Ok(())
    }

    pub fn finish(mut self) -> Result<Stored> {
        if !self.buf.is_empty() || self.chunks.is_empty() {
            let n = self.buf.len();
            self.flush_chunk(n)?;
        }
        let hash = self.hasher.finalize().to_hex().to_string();
        if self.chunks.len() == 1 {
            self.chunks.clear(); // the single chunk's id equals the whole-file hash
        }
        Ok(Stored { hash, chunks: self.chunks, size: self.size, new_bytes: self.new_bytes })
    }
}

impl Write for ChunkWriter<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.hasher.update(b);
        self.size += b.len() as u64;
        self.buf.extend_from_slice(b);
        while self.buf.len() >= CHUNK {
            self.flush_chunk(CHUNK)?;
        }
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default, Debug, Serialize)]
pub struct VerifyReport {
    pub objects: usize,
    pub missing: Vec<String>,
    pub corrupt: Vec<String>,
}
