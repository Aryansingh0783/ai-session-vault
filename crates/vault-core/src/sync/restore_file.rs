//! Restoring a single log or plain file.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn restore_log(
    env: &Env,
    store: &Store,
    state: &mut State,
    item: &Item,
    entries: &[&Entry],
    local_hash: Option<&str>,
    opts: &Options,
    safety: &mut SafetyCopies,
    rep: &mut Report,
) -> Result<()> {
    let mut remote: Vec<&Entry> = entries.to_vec();
    remote.sort_by_key(|e| e.mtime_ms);
    let seen: Vec<String> = state.files.get(&item.logical).map(|f| f.seen.clone()).unwrap_or_default();
    let mut distinct: Vec<&Entry> = vec![];
    for e in &remote {
        if !distinct.iter().any(|d| d.hash == e.hash) {
            distinct.push(e);
        }
    }
    if local_hash.is_some() && distinct.iter().all(|e| Some(e.hash.as_str()) == local_hash || seen.contains(&e.hash)) {
        rep.unchanged += 1;
        return Ok(());
    }
    if distinct.iter().any(|e| e.size > LOG_MEM_CAP) {
        rep.skipped_large.push(item.logical.clone());
        return Ok(());
    }
    let mut cur: Vec<u8> = if item.local.is_file() { prepare_mem(item, env, opts.redact)?.unwrap_or_default() } else { vec![] };
    let before = cur.clone();
    let mut merged = false;
    for e in &distinct {
        if local_hash == Some(e.hash.as_str()) {
            continue;
        }
        let r = store.read_all(e)?;
        rep.bytes_downloaded += r.len() as u64;
        match jsonl::relation(&cur, &r) {
            Relation::Same | Relation::BBehind => {}
            Relation::ABehind => cur = r,
            Relation::Diverged => {
                cur = jsonl::merge(&cur, &r);
                merged = true;
            }
        }
    }
    if cur != before || !item.local.is_file() {
        if opts.dry_run {
            rep.downloaded += 1;
            return Ok(());
        }
        let out = jsonl::transform(&cur, &|p| to_local(p, &env.home, &opts.maps));
        write_local(&item.local, &out, None, safety)?;
        if merged {
            rep.merged += 1
        } else {
            rep.downloaded += 1
        }
    } else {
        rep.unchanged += 1;
    }
    if !opts.dry_run {
        let md = fs::metadata(&item.local).ok();
        let f = state.files.entry(item.logical.clone()).or_default();
        f.local_hash = store.keys.object_id(&cur);
        if let Some(md) = md {
            f.size = md.len();
            f.mtime_ms = mtime_ms(&md);
        }
        f.seen = distinct.iter().map(|e| e.hash.clone()).chain(seen).take(16).collect();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn restore_file(
    env: &Env,
    store: &Store,
    state: &mut State,
    item: &Item,
    entries: &[&Entry],
    local: Option<&(String, i64)>,
    opts: &Options,
    safety: &mut SafetyCopies,
    rep: &mut Report,
    log: Log,
) -> Result<()> {
    let winner = entries.iter().max_by(|a, b| a.mtime_ms.cmp(&b.mtime_ms).then(a.hash.cmp(&b.hash))).unwrap();
    if item.logical == SETTINGS {
        return restore_settings(env, store, state, item, winner, local, opts, safety, rep);
    }
    let base = state.files.get(&item.logical).and_then(|f| f.synced_hash.clone());
    let install = |state: &mut State, safety: &mut SafetyCopies, rep: &mut Report| -> Result<()> {
        if item.logical == SETTINGS {
            install_settings(env, store, winner, &item.local, safety)?;
        } else {
            install_stream(store, winner, &item.local, safety)?;
        }
        rep.bytes_downloaded += winner.size;
        let md = fs::metadata(&item.local)?;
        let f = state.files.entry(item.logical.clone()).or_default();
        f.size = md.len();
        f.mtime_ms = mtime_ms(&md);
        f.local_hash = winner.hash.clone();
        f.synced_hash = Some(winner.hash.clone());
        save_base(env, &winner.hash, &item.local);
        Ok(())
    };
    match local {
        None => {
            if opts.dry_run {
                log(&format!("  would create {}", item.local.display()));
            } else {
                install(state, safety, rep)?;
            }
            rep.downloaded += 1;
        }
        Some((lh, lmt)) => {
            if *lh == winner.hash {
                state.files.entry(item.logical.clone()).or_default().synced_hash = Some(lh.clone());
                save_base(env, lh, &item.local);
                rep.unchanged += 1;
                return Ok(());
            }
            let local_changed = base.as_deref() != Some(lh.as_str());
            let remote_changed = base.as_deref() != Some(winner.hash.as_str());
            if local_changed && remote_changed && !opts.dry_run && mergeable(&item.local) && try_merge(env, store, state, item, winner, base.as_deref(), safety)? {
                rep.merged += 1;
                rep.warnings.push(format!("{}: edited on two devices; the changes were combined automatically", item.logical));
                return Ok(());
            }
            if !remote_changed {
                rep.unchanged += 1; // local edit waiting to be uploaded
            } else if winner.mtime_ms > *lmt + TOL_MS {
                if local_changed {
                    rep.conflicts.push(format!("{} changed on two devices; kept the newer version (yours is saved in {})", item.logical, env_safety_hint(safety)));
                }
                if opts.dry_run {
                    log(&format!("  would update {}", item.local.display()));
                } else {
                    install(state, safety, rep)?;
                }
                rep.downloaded += 1;
            } else {
                if local_changed {
                    rep.conflicts.push(format!("{} changed on two devices; kept this device's newer version", item.logical));
                }
                rep.unchanged += 1;
            }
        }
    }
    Ok(())
}
