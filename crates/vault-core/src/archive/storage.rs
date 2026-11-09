//! The on-device archive folder.

use super::*;

pub fn archive_dir(env: &Env) -> PathBuf {
    env.state_dir.join("archive")
}

pub(super) fn conv_path(env: &Env, c: &Conversation) -> PathBuf {
    let safe: String = c.id.chars().map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '_' }).take(80).collect();
    archive_dir(env).join(&c.source).join(format!("{safe}.json"))
}

/// Add conversations to the archive. Returns `(added, updated)`.
pub fn save_all(env: &Env, convs: &[Conversation]) -> Result<(usize, usize)> {
    let (mut added, mut updated) = (0, 0);
    for c in convs {
        let p = conv_path(env, c);
        match fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Conversation>(&b).ok()) {
            None => added += 1,
            Some(old) if old == *c || old.updated > c.updated => continue,
            Some(_) => updated += 1,
        }
        atomic_write(&p, &serde_json::to_vec(c)?)?;
    }
    Ok((added, updated))
}

/// Archive conversations + Claude Code sessions found on this device.
pub fn load_all(env: &Env, with_sessions: bool) -> Vec<Conversation> {
    let mut out: Vec<Conversation> = vec![];
    for e in walkdir::WalkDir::new(archive_dir(env)).into_iter().flatten().filter(|e| e.file_type().is_file()) {
        if let Some(c) = fs::read(e.path()).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            out.push(c);
        }
    }
    if with_sessions {
        let proot = env.claude_dir.join("projects");
        for e in walkdir::WalkDir::new(&proot).max_depth(2).into_iter().flatten() {
            if e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl") {
                if let Some(c) = parse_cc_session(e.path()) {
                    out.push(c);
                }
            }
        }
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    out
}
