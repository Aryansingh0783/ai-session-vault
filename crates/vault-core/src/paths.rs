//! Portable path handling: `~/...` form in the vault, mapping to this device's real paths.

use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub fn norm(p: &str) -> String {
    p.replace('\\', "/")
}

pub fn is_win(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || p.starts_with("\\\\")
}

/// Remainder (posix style) if `p` is inside `prefix`, `Some("")` if equal, else `None`.
pub fn prefix_match(p: &str, prefix: &str) -> Option<String> {
    let a = norm(p);
    let b = norm(prefix);
    let b = b.trim_end_matches('/');
    if b.is_empty() {
        return None;
    }
    let ci = is_win(p) || is_win(prefix);
    let (ac, bc) = if ci { (a.to_lowercase(), b.to_lowercase()) } else { (a.clone(), b.to_string()) };
    if ac == bc {
        return Some(String::new());
    }
    if ac.starts_with(&format!("{bc}/")) {
        let skip = b.chars().count() + 1;
        return Some(a.chars().skip(skip).collect());
    }
    None
}

fn join(base: &str, rest: &str) -> String {
    if rest.is_empty() {
        return base.to_string();
    }
    if base.starts_with('~') {
        return format!("{}/{}", base.trim_end_matches(['/', '\\']), rest);
    }
    let mut pb = PathBuf::from(base);
    for part in rest.split('/') {
        pb.push(part);
    }
    pb.to_string_lossy().into_owned()
}

/// Absolute path on this device -> `~/...` when under home.
pub fn to_portable(p: &str, home: &Path) -> String {
    match prefix_match(p, &home.to_string_lossy()) {
        Some(r) if r.is_empty() => "~".into(),
        Some(r) => format!("~/{r}"),
        None => p.to_string(),
    }
}

/// A user supplied `--map OLD=NEW` rule.
#[derive(Clone, Debug)]
pub struct PathMap {
    pub old: String,
    pub new: String,
}

pub fn parse_maps(items: &[String]) -> anyhow::Result<Vec<PathMap>> {
    items
        .iter()
        .map(|m| {
            let (o, n) = m
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("--map must look like OLD=NEW, got: {m}"))?;
            Ok(PathMap { old: o.trim().into(), new: n.trim().into() })
        })
        .collect()
}

/// Vault path (`~/...` or foreign absolute) -> absolute path on this device.
pub fn to_local(p: &str, home: &Path, maps: &[PathMap]) -> String {
    let mut p = p.to_string();
    for m in maps {
        if let Some(rest) = prefix_match(&p, &m.old) {
            p = join(&m.new, &rest);
            break;
        }
    }
    if p == "~" || p.starts_with("~/") {
        let rest = p.get(2..).unwrap_or("");
        return join(&home.to_string_lossy(), rest);
    }
    p
}

fn non_alnum() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[^A-Za-z0-9]").unwrap())
}

/// Claude Code names `~/.claude/projects/<dir>` by replacing non-alphanumerics with `-`.
pub fn encode_project(path: &str) -> String {
    non_alnum().replace_all(path, "-").into_owned()
}

/// Stable, readable, path-safe key for a project's portable path.
pub fn project_key(portable: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = R.get_or_init(|| Regex::new(r"[^A-Za-z0-9]+").unwrap());
    let src = portable.replacen('~', "HOME", 1);
    let slug: String = r.replace_all(&src, "-").trim_matches('-').chars().take(60).collect();
    let h = blake3::hash(portable.as_bytes()).to_hex();
    format!("{slug}-{}", &h.as_str()[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_roundtrip_windows_to_unix() {
        let p = to_portable("C:\\Users\\Aryan\\code\\app", Path::new("C:\\Users\\Aryan"));
        assert_eq!(p, "~/code/app");
        let back = to_local(&p, Path::new("/home/a"), &[]);
        assert!(back.replace('\\', "/").ends_with("home/a/code/app"));
    }

    #[test]
    fn case_insensitive_on_windows_paths() {
        assert_eq!(prefix_match("c:\\users\\aryan\\x", "C:\\Users\\Aryan").as_deref(), Some("x"));
        assert_eq!(prefix_match("/home/ab", "/home/a"), None);
    }

    #[test]
    fn maps_apply_first() {
        let m = parse_maps(&["D:\\work=~/work".into()]).unwrap();
        let out = to_local("D:/work/proj", Path::new("/home/a"), &m);
        assert!(out.replace('\\', "/").ends_with("/home/a/work/proj"));
    }

    #[test]
    fn encode_matches_claude() {
        assert_eq!(encode_project("C:\\Users\\a b\\p"), "C--Users-a-b-p");
    }
}
