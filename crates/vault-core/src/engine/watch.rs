//! File watchers for the tools' folders and the vault folder.

use super::*;

pub(super) fn start_watchers(env: &Env, store: &Store, tx: &Sender<Msg>) -> Vec<RecommendedWatcher> {
    let own_state = env.state_dir.clone();
    let vault_devices = store.root.join("devices");
    let mut out = vec![];
    let mut add = |root: &Path, mode: RecursiveMode, remote: bool| {
        if !root.exists() {
            return;
        }
        let tx = tx.clone();
        let skip = own_state.clone();
        let devices = vault_devices.clone();
        // Some platforms (macOS FSEvents) report real paths, so a watched folder behind a symlink
        // (/var -> /private/var) comes back with a different prefix. Map events back to the path we were given.
        let given = root.to_path_buf();
        let real = std::fs::canonicalize(root).unwrap_or_else(|_| given.clone());
        let w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            for p in ev.paths {
                let p = match p.strip_prefix(&real) {
                    Ok(rest) if real != given => given.join(rest),
                    _ => p,
                };
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if name.ends_with(".vaulttmp") {
                    continue;
                }
                if remote {
                    // Any change under devices/ - other devices publishing a new manifest.
                    if p.starts_with(&devices) {
                        let _ = tx.send(Msg::Remote);
                    }
                } else if !p.starts_with(&skip) || p.starts_with(skip.join("archive")) {
                    let _ = tx.send(Msg::Fs(p));
                }
            }
        });
        if let Ok(mut w) = w {
            if w.watch(root, mode).is_ok() {
                out.push(w);
            }
        }
    };
    add(&env.claude_dir, RecursiveMode::Recursive, false);
    add(&env.codex_dir, RecursiveMode::Recursive, false);
    add(&env.state_dir.join("archive"), RecursiveMode::Recursive, false);
    if let Some(d) = env.desktop_cfg.parent() {
        add(d, RecursiveMode::NonRecursive, false);
    }
    if let Some(h) = env.claude_json.parent() {
        add(h, RecursiveMode::NonRecursive, false); // for ~/.claude.json
    }
    add(&store.root.join("devices"), RecursiveMode::Recursive, true);
    out
}
