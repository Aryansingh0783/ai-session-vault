//! Atomic installs of restored files, safety copies and the run lock.

use super::*;

/// Write an entry's content to `path` through a temp file, verifying it before it replaces anything.
pub(super) fn install_stream(store: &Store, e: &Entry, path: &Path, safety: &mut SafetyCopies) -> Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".vaulttmp");
    let tmp = PathBuf::from(tmp);
    let r = (|| -> Result<()> {
        let mut w = BufWriter::new(fs::File::create(&tmp)?);
        store.read_to(e, &mut w)?;
        w.flush()?;
        Ok(())
    })();
    if let Err(err) = r {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    safety.save(path)?;
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    set_mtime(path, e.mtime_ms);
    Ok(())
}

pub(super) fn set_mtime(p: &Path, ms: i64) {
    let ft = filetime::FileTime::from_unix_time(ms.div_euclid(1000), (ms.rem_euclid(1000) as u32) * 1_000_000);
    let _ = filetime::set_file_mtime(p, ft);
}

pub(super) struct SafetyCopies<'a> {
    pub(super) env: &'a Env,
    pub(super) stamp: String,
    pub(super) count: usize,
}

impl SafetyCopies<'_> {
    pub(super) fn save(&mut self, p: &Path) -> Result<()> {
        if !p.is_file() {
            return Ok(());
        }
        let rel = p.strip_prefix(&self.env.home).map(|r| r.to_path_buf()).unwrap_or_else(|_| PathBuf::from(p.to_string_lossy().replace([':', '\\', '/'], "_")));
        let dst = self.env.safety_root().join(&self.stamp).join(rel);
        if let Some(d) = dst.parent() {
            fs::create_dir_all(d)?;
        }
        fs::copy(p, dst)?;
        self.count += 1;
        Ok(())
    }
}

pub(super) fn dir_size(d: &Path) -> u64 {
    WalkDir::new(d).into_iter().flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()
}

pub(super) fn prune_safety(env: &Env) {
    let root = env.safety_root();
    let Ok(rd) = fs::read_dir(&root) else { return };
    let mut runs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    runs.sort();
    if runs.len() > KEEP_SAFETY_RUNS {
        for d in runs.drain(..runs.len() - KEEP_SAFETY_RUNS) {
            let _ = fs::remove_dir_all(d);
        }
    }
    let mut total = 0u64;
    for (i, d) in runs.iter().rev().enumerate() {
        total += dir_size(d);
        if i > 0 && total > MAX_SAFETY_BYTES {
            let _ = fs::remove_dir_all(d);
        }
    }
}

/// Take an exclusive per-device lock so two runs (e.g. a scheduled one and a manual one) don't collide.
pub struct RunLock(#[allow(dead_code)] fs::File);

pub fn lock(env: &Env) -> Result<RunLock> {
    fs::create_dir_all(&env.state_dir)?;
    let f = fs::OpenOptions::new().create(true).write(true).truncate(false).open(env.state_dir.join("lock"))?;
    f.try_lock().map_err(|_| anyhow!("another AI Session Vault run is already in progress on this device"))?;
    Ok(RunLock(f))
}
