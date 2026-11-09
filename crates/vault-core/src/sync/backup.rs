//! Local files into the vault.

use super::*;

// ===================================================================== backup
pub fn backup(env: &Env, store: &Store, state: &mut State, opts: &Options, log: Log) -> Result<Report> {
    let mut rep = Report::default();
    let (mut items, projects) = scan_local(env, &opts.components, state);
    if let Some(only) = &opts.only_paths {
        items.retain(|i| only.contains(&i.local));
    }
    let mut manifest = store.read_manifest(&state.device_id)?.unwrap_or_else(|| Store::new_manifest(state.device_info(env)));
    manifest.device = state.device_info(env);
    let mut changed = false;
    for (k, v) in &projects {
        if manifest.projects.get(k) != Some(v) {
            manifest.projects.insert(k.clone(), v.clone());
            changed = true;
        }
    }
    let total = items.len();
    let mut since_checkpoint = 0;
    for (i, item) in items.iter().enumerate() {
        if opts.hooks.cancelled() {
            rep.cancelled = true;
            break;
        }
        opts.hooks.progress("backup", i, total);
        let md = match fs::metadata(&item.local) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if item.kind == Kind::Log && md.len() > LOG_MEM_CAP {
            rep.skipped_large.push(item.local.display().to_string());
            continue;
        }
        if opts.block_secrets && item.kind == Kind::Log && file_has_secrets(&item.local) {
            rep.blocked.push(item.local.display().to_string());
            continue;
        }
        let (size, mt) =
            if crate::sqlite::is_sqlite(&item.local) { crate::sqlite::fingerprint(&item.local).unwrap_or((md.len(), mtime_ms(&md))) } else { (md.len(), mtime_ms(&md)) };
        let have = manifest.entries.get(&item.logical).map(|e| e.hash.clone());
        if let Some(c) = state.files.get(&item.logical) {
            if c.size == size && c.mtime_ms == mt && Some(&c.local_hash) == have.as_ref() {
                rep.unchanged += 1;
                continue;
            }
        }
        // Stream: read -> portable form -> 1 MiB chunks -> encrypt. Memory stays bounded; chunks the
        // vault already has (e.g. the unchanged head of a growing log) are not uploaded again.
        let res: Result<Option<Uploaded>> = (|| {
            if opts.dry_run {
                let mut hw = HashWriter::new(store);
                if !prepare_to(item, env, opts.redact, &mut hw)? {
                    return Ok(None);
                }
                let (h, sz) = hw.finish();
                Ok(Some((h, vec![], sz, 0)))
            } else {
                let mut cw = ChunkWriter::new(store);
                if !prepare_to(item, env, opts.redact, &mut cw)? {
                    return Ok(None);
                }
                let st = cw.finish()?;
                Ok(Some((st.hash, st.chunks, st.size, st.new_bytes)))
            }
        })();
        let (id, chunks, out_size, new_bytes) = match res {
            Ok(Some(x)) => x,
            Ok(None) => continue,
            Err(e) => {
                rep.failed.push(format!("{}: {e}", item.local.display()));
                continue;
            }
        };
        let fs_entry = state.files.entry(item.logical.clone()).or_default();
        fs_entry.size = size;
        fs_entry.mtime_ms = mt;
        fs_entry.local_hash = id.clone();
        if have.as_ref() == Some(&id) {
            rep.unchanged += 1;
            continue;
        }
        if opts.dry_run {
            log(&format!("  would upload {}", item.logical));
            rep.uploaded += 1;
            continue;
        }
        if item.kind == Kind::File {
            fs_entry.synced_hash = Some(id.clone());
            save_base(env, &id, &item.local);
        } else if !fs_entry.seen.contains(&id) {
            fs_entry.seen.push(id.clone());
        }
        manifest.entries.insert(item.logical.clone(), Entry { kind: item.kind, size: out_size, mtime_ms: mt, hash: id, chunks });
        changed = true;
        rep.uploaded += 1;
        rep.bytes_uploaded += new_bytes;
        since_checkpoint += 1;
        if since_checkpoint >= 100 {
            // Save progress so an interrupted run resumes instead of starting over.
            manifest.seq += 1;
            manifest.updated = now_iso();
            store.write_manifest(&manifest)?;
            state.save(env)?;
            since_checkpoint = 0;
        }
    }
    opts.hooks.progress("backup", total, total);
    if changed && !opts.dry_run {
        manifest.seq += 1;
        manifest.updated = now_iso();
        store.write_manifest(&manifest)?;
    }
    if !opts.dry_run {
        state.save(env)?;
    }
    Ok(rep)
}
