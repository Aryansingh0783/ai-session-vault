//! Enrollment codes and passphrase-free device access.

use super::*;

impl Store {
    pub(super) fn slots_dir(root: &Path) -> PathBuf {
        root.join("slots")
    }

    pub(super) fn read_slots(root: &Path, kind: &str) -> Vec<SlotFile> {
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
}
