//! Read-only view models for a UI: what is in the vault, per project and per device.

use crate::env::{Env, State};
use crate::paths::{to_local, PathMap};
use crate::store::Store;
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Serialize, Debug)]
pub struct ProjectView {
    pub key: String,
    /// Portable path, e.g. `~/code/app` (or `raw:<name>` when the folder couldn't be determined).
    pub portable_path: String,
    /// Where this project lives on *this* device.
    pub local_path: Option<String>,
    pub exists_locally: bool,
    pub sessions: usize,
    pub files: usize,
    pub bytes: u64,
    /// Newest file modification time in the vault, unix ms.
    pub last_activity_ms: i64,
    pub devices: Vec<String>,
}

#[derive(Serialize, Debug)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub os: String,
    pub last_sync: String,
    pub files: usize,
    pub this_device: bool,
}

#[derive(Serialize, Debug, Default)]
pub struct Totals {
    pub files: usize,
    pub bytes: u64,
    /// Size of the distinct contents (a file synced to several devices counts once).
    pub unique_bytes: u64,
    pub sessions: usize,
    pub projects: usize,
    pub devices: usize,
}

#[derive(Serialize, Debug)]
pub struct Overview {
    pub vault_path: String,
    pub vault_id: String,
    pub devices: Vec<DeviceView>,
    /// Most recently active first.
    pub projects: Vec<ProjectView>,
    pub totals: Totals,
    /// Manifests that could not be read (e.g. the cloud folder is still syncing).
    pub warnings: Vec<String>,
}

pub fn overview(env: &Env, store: &Store, maps: &[PathMap]) -> Result<Overview> {
    let (manifests, warnings) = store.read_all_manifests()?;
    let me = State::load(env).device_id;
    let mut projects: BTreeMap<String, ProjectView> = BTreeMap::new();
    let mut project_devices: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut totals = Totals::default();
    let mut unique: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut devices = vec![];
    for m in &manifests {
        devices.push(DeviceView {
            id: m.device.id.clone(),
            name: m.device.name.clone(),
            os: m.device.os.clone(),
            last_sync: m.updated.clone(),
            files: m.entries.len(),
            this_device: m.device.id == me,
        });
        for (logical, e) in &m.entries {
            totals.files += 1;
            totals.bytes += e.size;
            if unique.insert(e.hash.clone()) {
                totals.unique_bytes += e.size;
            }
            let Some(rest) = logical.strip_prefix("claude-code/projects/") else { continue };
            let Some((key, rel)) = rest.split_once('/') else { continue };
            let portable = m.projects.get(key).cloned().unwrap_or_else(|| format!("raw:{key}"));
            let local = if portable.starts_with("raw:") { None } else { Some(to_local(&portable, &env.home, maps)) };
            let p = projects.entry(key.to_string()).or_insert_with(|| ProjectView {
                key: key.to_string(),
                exists_locally: local.as_deref().map_or(false, |l| Path::new(l).is_dir()),
                local_path: local,
                portable_path: portable,
                sessions: 0,
                files: 0,
                bytes: 0,
                last_activity_ms: 0,
                devices: vec![],
            });
            p.files += 1;
            p.bytes += e.size;
            p.last_activity_ms = p.last_activity_ms.max(e.mtime_ms);
            if !rel.contains('/') && rel.ends_with(".jsonl") {
                p.sessions += 1;
            }
            project_devices.entry(key.to_string()).or_default().insert(m.device.id.clone(), m.device.name.clone());
        }
    }
    // A session present on several devices is one session: count distinct log names per project.
    let mut distinct: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for m in &manifests {
        for logical in m.entries.keys() {
            if let Some(rest) = logical.strip_prefix("claude-code/projects/") {
                if let Some((key, rel)) = rest.split_once('/') {
                    if !rel.contains('/') && rel.ends_with(".jsonl") {
                        distinct.entry(key.to_string()).or_default().insert(rel.to_string());
                    }
                }
            }
        }
    }
    let mut list: Vec<ProjectView> = projects.into_values().collect();
    for p in &mut list {
        p.sessions = distinct.get(&p.key).map_or(0, |s| s.len());
        p.devices = project_devices.remove(&p.key).map(|s| s.into_values().collect()).unwrap_or_default();
    }
    list.sort_by(|a, b| b.last_activity_ms.cmp(&a.last_activity_ms));
    totals.sessions = list.iter().map(|p| p.sessions).sum();
    totals.projects = list.len();
    totals.devices = devices.len();
    Ok(Overview {
        vault_path: store.root.display().to_string(),
        vault_id: store.header.vault_id.clone(),
        devices,
        projects: list,
        totals,
        warnings,
    })
}
