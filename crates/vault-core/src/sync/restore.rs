//! The vault into local files.

use super::*;

// ==================================================================== restore
pub(super) fn local_hash_of(item: &Item, env: &Env, store: &Store, state: &mut State, redact: bool) -> Option<(String, i64)> {
    let md = fs::metadata(&item.local).ok()?;
    let (size, mt) = if crate::sqlite::is_sqlite(&item.local) { crate::sqlite::fingerprint(&item.local).unwrap_or((md.len(), mtime_ms(&md))) } else { (md.len(), mtime_ms(&md)) };
    if let Some(c) = state.files.get(&item.logical) {
        if c.size == size && c.mtime_ms == mt && !c.local_hash.is_empty() {
            return Some((c.local_hash.clone(), mt));
        }
    }
    let mut hw = HashWriter::new(store);
    if !prepare_to(item, env, redact, &mut hw).ok()? {
        return None;
    }
    let (id, _) = hw.finish();
    let e = state.files.entry(item.logical.clone()).or_default();
    e.size = size;
    e.mtime_ms = mt;
    e.local_hash = id.clone();
    Some((id, mt))
}

pub(super) fn write_local(path: &Path, data: &[u8], mtime: Option<i64>, safety: &mut SafetyCopies) -> Result<()> {
    safety.save(path)?;
    atomic_write(path, data)?;
    if let Some(ms) = mtime {
        set_mtime(path, ms);
    }
    Ok(())
}

pub fn restore(env: &Env, store: &Store, state: &mut State, opts: &Options, log: Log) -> Result<Report> {
    let mut rep = Report::default();
    let (manifests, warns) = store.read_all_manifests()?;
    rep.warnings.extend(warns);
    let mut projects: BTreeMap<String, String> = BTreeMap::new();
    let mut cands: BTreeMap<String, Vec<(&str, &Entry)>> = BTreeMap::new();
    for m in &manifests {
        projects.extend(m.projects.clone());
        for (l, e) in &m.entries {
            if Component::of(l).is_some_and(|c| opts.components.contains(&c)) {
                cands.entry(l.clone()).or_default().push((m.device.id.as_str(), e));
            }
        }
    }
    let mut safety = SafetyCopies { env, stamp: stamp(), count: 0 };
    let mut missing = BTreeSet::new();
    let mut mcp_blobs: Vec<&Entry> = vec![];

    let total = cands.len();
    for (idx, (logical, entries)) in cands.iter().enumerate() {
        if opts.hooks.cancelled() {
            rep.cancelled = true;
            break;
        }
        opts.hooks.progress("restore", idx, total);
        let all: Vec<&Entry> = entries.iter().map(|(_, e)| *e).collect();
        // This device's own manifest entry is only what it last uploaded; prefer what other devices have.
        let others: Vec<&Entry> = entries.iter().filter(|(d, _)| *d != state.device_id).map(|(_, e)| *e).collect();
        let entries: Vec<&Entry> = if others.is_empty() { all } else { others };
        if logical == MCP {
            mcp_blobs = entries.clone();
            continue;
        }
        let Some(local) = local_path_for(env, logical, &projects, &opts.maps, &mut missing) else {
            rep.warnings.push(format!("ignored unrecognised vault entry {logical}"));
            continue;
        };
        if sqlite_busy(&local) {
            rep.busy.push(local.file_name().unwrap().to_string_lossy().into_owned());
            continue;
        }
        let kind = entries[0].kind;
        let item = Item { logical: logical.clone(), local: local.clone(), kind };
        let local_state = local.is_file().then(|| local_hash_of(&item, env, store, state, opts.redact)).flatten();
        let res = match kind {
            Kind::Log => restore_log(env, store, state, &item, &entries, local_state.as_ref().map(|x| x.0.as_str()), opts, &mut safety, &mut rep),
            Kind::File => restore_file(env, store, state, &item, &entries, local_state.as_ref(), opts, &mut safety, &mut rep, log),
        };
        if let Err(e) = res {
            rep.failed.push(format!("{}: {e}", local.display()));
        }
    }
    if !mcp_blobs.is_empty() {
        if let Err(e) = restore_mcp(env, store, &mcp_blobs, opts, &mut safety, &mut rep, log) {
            rep.failed.push(format!("MCP servers: {e}"));
        }
    }
    rep.safety_copies = safety.count;
    // Only worth mentioning when something was actually written into the tools' folders.
    if (rep.downloaded + rep.merged) > 0 && !opts.dry_run {
        let running = running_tools();
        if !running.is_empty() {
            rep.warnings.push(format!("{} appears to be running; restored files may be overwritten by it. Close it and restore again to be safe.", running.join(" and ")));
        }
    }
    rep.missing_projects = missing.into_iter().collect();
    if !opts.dry_run {
        state.save(env)?;
        prune_safety(env);
        prune_bases(env, state);
    }
    Ok(rep)
}

pub(super) fn env_safety_hint(s: &SafetyCopies) -> String {
    s.env.safety_root().join(&s.stamp).display().to_string()
}

/// MCP servers run commands, so they are only *added* (never overwritten), and each is listed.
pub(super) fn restore_mcp(env: &Env, store: &Store, entries: &[&Entry], opts: &Options, safety: &mut SafetyCopies, rep: &mut Report, log: Log) -> Result<()> {
    let mut sorted: Vec<&Entry> = entries.to_vec();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.mtime_ms));
    let mut servers = serde_json::Map::new();
    for e in sorted {
        if let Value::Object(o) = serde_json::from_slice::<Value>(&store.read_all(e)?)? {
            for (k, v) in o {
                servers.entry(k).or_insert(v);
            }
        }
    }
    if !env.claude_json.is_file() {
        rep.warnings.push("run `claude` once on this device, then restore again to add your MCP servers".into());
        return Ok(());
    }
    let raw = fs::read(&env.claude_json)?;
    let mut doc: Value = serde_json::from_slice(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw))?;
    let obj = doc.as_object_mut().ok_or_else(|| anyhow!(".claude.json is not an object"))?;
    let cur = obj.entry("mcpServers").or_insert_with(|| Value::Object(Default::default()));
    let cur = cur.as_object_mut().ok_or_else(|| anyhow!("mcpServers is not an object"))?;
    let mut added = vec![];
    for (k, v) in servers {
        if !cur.contains_key(&k) {
            let cmd = v.get("command").or_else(|| v.get("url")).and_then(|c| c.as_str()).unwrap_or("?");
            log(&format!("  + MCP server '{k}': {cmd}"));
            added.push(k.clone());
            cur.insert(k, v);
        }
    }
    if added.is_empty() {
        return Ok(());
    }
    if !opts.dry_run {
        safety.save(&env.claude_json)?;
        atomic_write(&env.claude_json, serde_json::to_string_pretty(&doc)?.as_bytes())?;
    }
    rep.mcp_added = added;
    Ok(())
}
