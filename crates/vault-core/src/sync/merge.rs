//! Three-way text merges and key-level settings merges.

use super::*;

/// Small prose/config files (CLAUDE.md, memory notes, rules, TOML) are merged line by line when two
/// devices edit different parts. Everything else falls back to newest-wins.
pub(super) fn mergeable(p: &Path) -> bool {
    p.extension().is_some_and(|e| matches!(e.to_str(), Some("md") | Some("txt") | Some("toml")))
}

pub(super) const BASE_MAX: u64 = 256 * 1024;

/// Keep a local copy of the last-synced version so a later two-sided edit can be merged three-way.
pub(super) fn save_base(env: &Env, hash: &str, path: &Path) {
    if !mergeable(path) || fs::metadata(path).map_or(true, |m| m.len() > BASE_MAX) {
        return;
    }
    let dst = env.state_dir.join("base").join(hash);
    if !dst.exists() {
        if let Some(d) = dst.parent() {
            let _ = fs::create_dir_all(d);
        }
        let _ = fs::copy(path, dst);
    }
}

pub(super) fn prune_bases(env: &Env, state: &State) {
    let keep: HashSet<&str> = state.files.values().filter_map(|f| f.synced_hash.as_deref()).collect();
    if let Ok(rd) = fs::read_dir(env.state_dir.join("base")) {
        for e in rd.flatten() {
            if !keep.contains(e.file_name().to_string_lossy().as_ref()) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

/// Three-way merge of a text file edited on two devices. `Ok(false)` = overlapping edits or no base.
#[allow(clippy::too_many_arguments)]
pub(super) fn try_merge(env: &Env, store: &Store, state: &mut State, item: &Item, winner: &Entry, base_hash: Option<&str>, safety: &mut SafetyCopies) -> Result<bool> {
    let Some(base_hash) = base_hash else { return Ok(false) };
    let Ok(anc) = fs::read_to_string(env.state_dir.join("base").join(base_hash)) else { return Ok(false) };
    let (Ok(ours), Ok(theirs)) = (fs::read_to_string(&item.local), String::from_utf8(store.read_all(winner)?)) else { return Ok(false) };
    let Ok(merged) = diffy::merge(&anc, &ours, &theirs) else { return Ok(false) };
    safety.save(&item.local)?;
    atomic_write(&item.local, merged.as_bytes())?;
    let md = fs::metadata(&item.local)?;
    let f = state.files.entry(item.logical.clone()).or_default();
    f.size = md.len();
    f.mtime_ms = mtime_ms(&md);
    f.local_hash = store.keys.object_id(merged.as_bytes());
    // The remote version is now our common ancestor; the merged text still needs uploading.
    f.synced_hash = Some(winner.hash.clone());
    let dst = env.state_dir.join("base").join(&winner.hash);
    if let Some(d) = dst.parent() {
        let _ = fs::create_dir_all(d);
    }
    let _ = fs::write(dst, theirs);
    Ok(true)
}

/// Install shared settings but keep this device's own values for the machine-specific keys.
pub(super) fn install_settings(env: &Env, store: &Store, e: &Entry, path: &Path, safety: &mut SafetyCopies) -> Result<()> {
    let remote = store.read_all(e)?;
    let mut merged = match serde_json::from_slice::<Value>(&remote) {
        Ok(Value::Object(o)) => o,
        _ => return install_stream(store, e, path, safety),
    };
    if let Ok(local_raw) = fs::read(path) {
        if let Ok(Value::Object(local)) = serde_json::from_slice::<Value>(local_raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&local_raw)) {
            for k in &env.local_keys {
                if let Some(v) = local.get(k) {
                    merged.insert(k.clone(), v.clone());
                }
            }
        }
    }
    let mut out = serde_json::to_vec_pretty(&Value::Object(merged))?;
    out.push(b'\n');
    safety.save(path)?;
    atomic_write(path, &out)?;
    set_mtime(path, e.mtime_ms);
    Ok(())
}

/// `settings.json` is merged key by key instead of replaced as a whole file:
/// keys only one side has are kept, machine-specific keys never leave their device, and when both
/// sides set a key differently the newer file's value wins (and the difference is reported).
#[allow(clippy::too_many_arguments)]
pub(super) fn restore_settings(
    env: &Env,
    store: &Store,
    state: &mut State,
    item: &Item,
    winner: &Entry,
    local: Option<&(String, i64)>,
    opts: &Options,
    safety: &mut SafetyCopies,
    rep: &mut Report,
) -> Result<()> {
    let Ok(Value::Object(remote)) = serde_json::from_slice::<Value>(&store.read_all(winner)?) else {
        return Ok(());
    };
    let local_obj = fs::read(&item.local).ok().and_then(|b| match serde_json::from_slice::<Value>(b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&b)) {
        Ok(Value::Object(o)) => Some(o),
        _ => None,
    });
    let lmt = local.map(|l| l.1).unwrap_or(0);
    let remote_newer = winner.mtime_ms > lmt + TOL_MS;
    let mut merged = local_obj.clone().unwrap_or_default();
    let mut differing = vec![];
    for (k, rv) in &remote {
        if env.local_keys.contains(k) {
            continue;
        }
        match merged.get(k) {
            None => {
                merged.insert(k.clone(), rv.clone());
            }
            Some(lv) if lv == rv => {}
            Some(_) => {
                differing.push(k.clone());
                if remote_newer {
                    merged.insert(k.clone(), rv.clone());
                }
            }
        }
    }
    if local_obj.is_some() && !differing.is_empty() {
        rep.conflicts.push(format!("settings.json: {} set differently on two devices; kept the newer value (replaced values are in the safety copy)", differing.join(", ")));
    }
    if local_obj.as_ref() == Some(&merged) {
        rep.unchanged += 1;
        return Ok(());
    }
    rep.downloaded += 1;
    if opts.dry_run {
        return Ok(());
    }
    let mut out = serde_json::to_vec_pretty(&Value::Object(merged))?;
    out.push(b'\n');
    safety.save(&item.local)?;
    atomic_write(&item.local, &out)?;
    let md = fs::metadata(&item.local)?;
    let f = state.files.entry(item.logical.clone()).or_default();
    f.size = md.len();
    f.mtime_ms = mtime_ms(&md);
    f.local_hash = store.keys.object_id(&filter_settings(&out, &env.local_keys));
    Ok(())
}
