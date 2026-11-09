//! Per-device manifests, integrity checks and garbage collection.

use super::*;

impl Store {
    pub(super) fn manifest_path(&self, device_id: &str) -> PathBuf {
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

    pub fn new_manifest(device: DeviceInfo) -> Manifest {
        Manifest { device, seq: 0, updated: now_iso(), ..Default::default() }
    }

    /// Delete objects no manifest references. Returns (count, bytes).
    /// Objects younger than `GC_GRACE` are kept: another device may be mid-upload, with its new
    /// objects not yet listed in its manifest (manifests are checkpointed every 100 uploads).
    pub fn gc(&self, manifests: &[Manifest]) -> Result<(usize, u64)> {
        let now = std::time::SystemTime::now();
        let live: std::collections::HashSet<&str> =
            manifests.iter().flat_map(|m| m.entries.values().flat_map(|e| std::iter::once(e.hash.as_str()).chain(e.chunks.iter().map(|c| c.as_str())))).collect();
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
