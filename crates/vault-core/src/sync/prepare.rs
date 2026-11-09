//! Turn a local file into what is stored: portable paths, settings filtering, SQLite snapshots.

use super::*;

pub(super) fn sqlite_busy(p: &Path) -> bool {
    if !p.extension().is_some_and(|e| e == "sqlite") {
        return false;
    }
    let mut w = p.as_os_str().to_owned();
    w.push("-wal");
    fs::metadata(PathBuf::from(w)).map(|m| m.len() > 0).unwrap_or(false)
}

/// Serialize just the `mcpServers` of `.claude.json` (the rest is account state, never synced).
pub(super) fn mcp_extract(raw: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw)).ok()?;
    let m = v.get("mcpServers")?.as_object()?;
    if m.is_empty() {
        return None;
    }
    serde_json::to_vec_pretty(&Value::Object(m.clone())).ok()
}

/// Stream a local file's *portable* form (what goes into the vault) into `out`.
/// Returns false when there is nothing to store. Live logs: a trailing partial line is never shipped.
pub(super) fn prepare_to(item: &Item, env: &Env, redact: bool, out: &mut dyn Write) -> Result<bool> {
    if item.logical == MCP {
        return match mcp_extract(&fs::read(&item.local)?) {
            Some(b) => {
                out.write_all(&b)?;
                Ok(true)
            }
            None => Ok(false),
        };
    }
    if item.logical == SETTINGS {
        let raw = fs::read(&item.local)?;
        out.write_all(&filter_settings(&raw, &env.local_keys))?;
        return Ok(true);
    }
    if crate::sqlite::is_sqlite(&item.local) {
        // Live database: take a consistent snapshot with SQLite's backup API, then stream that.
        let tmp = env.state_dir.join("tmp").join(format!("snap-{}.sqlite", std::process::id()));
        if let Some(d) = tmp.parent() {
            fs::create_dir_all(d)?;
        }
        let r = crate::sqlite::snapshot(&item.local, &tmp).and_then(|_| Ok(std::io::copy(&mut fs::File::open(&tmp)?, out).map(|_| ())?));
        let _ = fs::remove_file(&tmp);
        r?;
        return Ok(true);
    }
    let f = fs::File::open(&item.local)?;
    if item.kind == Kind::Log {
        let mut r = BufReader::with_capacity(256 * 1024, f);
        let mut line = Vec::new();
        loop {
            line.clear();
            if r.read_until(b'\n', &mut line)? == 0 || !line.ends_with(b"\n") {
                break;
            }
            let t = jsonl::transform(&line, &|p| to_portable(p, &env.home));
            out.write_all(&if redact { crate::secrets::redact(&t) } else { t })?;
        }
    } else {
        std::io::copy(&mut BufReader::with_capacity(256 * 1024, f), out)?;
    }
    Ok(true)
}

pub(super) fn prepare_mem(item: &Item, env: &Env, redact: bool) -> Result<Option<Vec<u8>>> {
    let mut v = Vec::new();
    Ok(prepare_to(item, env, redact, &mut v)?.then_some(v))
}

/// `settings.json` minus the machine-specific keys, in a canonical form. Unparseable files pass through.
pub(super) fn filter_settings(raw: &[u8], local_keys: &[String]) -> Vec<u8> {
    let parsed = serde_json::from_slice::<Value>(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw));
    match parsed {
        Ok(Value::Object(mut o)) => {
            for k in local_keys {
                o.shift_remove(k);
            }
            let mut out = serde_json::to_vec_pretty(&Value::Object(o)).unwrap_or_else(|_| raw.to_vec());
            out.push(b'\n');
            out
        }
        _ => raw.to_vec(),
    }
}

pub(super) fn file_has_secrets(path: &Path) -> bool {
    let Ok(f) = fs::File::open(path) else { return false };
    let mut r = BufReader::with_capacity(256 * 1024, f);
    let mut line = Vec::new();
    while r.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
        if crate::secrets::count(&line) > 0 {
            return true;
        }
        line.clear();
    }
    false
}
