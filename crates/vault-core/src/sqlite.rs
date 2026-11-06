//! Consistent snapshots of live SQLite databases (Codex keeps its history in `state_*.sqlite`).
//! Copying the file while the app has it open in WAL mode can capture a torn database, so we use
//! SQLite's own online-backup API, which yields a consistent copy even while the app is writing.

use anyhow::Result;
use rusqlite::{backup::Backup, Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

pub fn is_sqlite(p: &Path) -> bool {
    p.extension().map_or(false, |e| e == "sqlite" || e == "sqlite3" || e == "db")
}

/// Copy `src` into a new database file at `dst`.
pub fn snapshot(src: &Path, dst: &Path) -> Result<()> {
    let _ = std::fs::remove_file(dst);
    let from = Connection::open_with_flags(src, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    from.busy_timeout(Duration::from_secs(5))?;
    let mut to = Connection::open(dst)?;
    let b = Backup::new(&from, &mut to)?;
    b.run_to_completion(512, Duration::from_millis(5), None)?;
    drop(b);
    // The vault copy should be a plain single-file database (no WAL sidecar needed to read it).
    let _ = to.pragma_update(None, "journal_mode", "DELETE");
    Ok(())
}

/// Size and newest mtime across the database and its `-wal` sidecar: a WAL-mode database changes
/// in the sidecar long before the main file's timestamp moves.
pub fn fingerprint(p: &Path) -> Option<(u64, i64)> {
    let md = std::fs::metadata(p).ok()?;
    let (mut size, mut mt) = (md.len(), crate::util::mtime_ms(&md));
    let mut w = p.as_os_str().to_owned();
    w.push("-wal");
    if let Ok(wm) = std::fs::metadata(std::path::PathBuf::from(w)) {
        size += wm.len();
        mt = mt.max(crate::util::mtime_ms(&wm));
    }
    Some((size, mt))
}
