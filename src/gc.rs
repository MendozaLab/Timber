// Copyright 2026 Kenneth A. Mendoza (MendozaLab). https://github.com/MendozaLab/Timber
// SPDX-License-Identifier: Apache-2.0
//! Reclaim incomplete captures inside the store. Reachability from completed
//! manifests governs every deletion; journals only attribute bytes to a run.
//! Nothing outside the store root is read or written, and completed checkpoints
//! are never removed.
use crate::{
    model::SCHEMA,
    store::{self, Store, check_private_directory, read_regular, referenced_objects, valid_digest},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// Incomplete markers are named `capture-<random>.json` in `store/incomplete/`.
pub const MARKER_PREFIX: &str = "capture-";
pub const MARKER_SUFFIX: &str = ".json";
/// Bundle staging directories a capture creates in `store/incomplete/`.
const STAGING_PREFIX: &str = "bundle-";
/// Temporary files an interrupted immutable write can leave in `store/objects/`.
const TEMPORARY_PREFIX: &str = ".tmp";
const MARKER_LIMIT: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JournalState {
    /// The marker carries a complete object journal.
    Present,
    /// Written by an earlier Timber without a journal; its objects are reclaimed
    /// only when no completed manifest references them, like every other object.
    Absent,
    /// The journal ends in an incomplete or malformed record; entries before it count.
    Truncated,
    /// The marker header cannot be read as an incomplete-capture record.
    Unreadable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncompleteMarker {
    pub id: String,
    pub operation: Option<String>,
    pub source: Option<String>,
    pub started_unix: Option<u64>,
    pub age_seconds: Option<u64>,
    /// A capture still holds this marker's lock; its objects are not yet referenced.
    pub in_progress: bool,
    pub journal: JournalState,
    /// Objects the journal says this run added to the store.
    pub journaled_objects: u64,
    /// Bytes of those journaled objects still present in the store.
    pub journaled_bytes: u64,
    /// Journaled objects no longer present in the store.
    pub journaled_absent: u64,
    /// Journaled objects a completed checkpoint references; always kept.
    pub journaled_kept_referenced: u64,
    pub journaled_reclaimable_objects: u64,
    pub journaled_reclaimable_bytes: u64,
    pub marker_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectRecord {
    pub id: String,
    pub bytes: u64,
    /// Markers whose journal lists this object; empty when no journal does.
    pub journaled_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrayItem {
    /// Store-relative name inside `objects/` or `incomplete/`.
    pub name: String,
    /// `object_temporary` or `capture_staging`.
    pub kind: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissingReference {
    pub checkpoint: String,
    pub object: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Removed {
    pub markers: Vec<String>,
    pub marker_bytes: u64,
    pub objects: u64,
    pub object_bytes: u64,
    pub stray_items: u64,
    pub stray_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kept {
    pub completed_checkpoints: u64,
    pub referenced_objects: u64,
    pub referenced_bytes: u64,
    /// Store items Timber does not recognize; never removed.
    pub unrecognized: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reclamation {
    pub schema: u32,
    /// `dry_run`, `dry_run_would_refuse` or `reclaimed`.
    pub status: String,
    pub dry_run: bool,
    pub completed_checkpoints: Vec<String>,
    /// Objects referenced by a completed manifest but absent from the store.
    pub missing_references: Vec<MissingReference>,
    pub incomplete_markers: Vec<IncompleteMarker>,
    /// Objects referenced by no completed manifest.
    pub unreferenced_objects: Vec<ObjectRecord>,
    pub unreferenced_object_bytes: u64,
    /// Unreferenced objects that no marker journal lists (for example, from a
    /// marker without a journal).
    pub unattributed_objects: u64,
    pub unattributed_bytes: u64,
    pub stray_items: Vec<StrayItem>,
    /// Everything `timber gc` would remove (dry run) or removed: unreferenced
    /// objects, stray temporary items and the marker files themselves.
    pub reclaimable_bytes: u64,
    pub kept: Kept,
    pub removed: Option<Removed>,
    pub notes: Vec<String>,
}

struct Scan {
    report: Reclamation,
    markers: Vec<String>,
    referenced: BTreeSet<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn regular_size(path: &Path) -> Option<u64> {
    fs::symlink_metadata(path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
}

/// Bytes below a staging directory, never following a symbolic link.
fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_dir() {
        fs::read_dir(path)
            .map(|items| {
                items
                    .filter_map(|i| i.ok())
                    .map(|i| tree_bytes(&i.path()))
                    .fold(0u64, u64::saturating_add)
            })
            .unwrap_or(0)
    } else {
        meta.len()
    }
}

/// True when a capture still holds the marker's lock.
fn marker_in_progress(path: &Path) -> bool {
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => !store::flock_waiting(&file, false).unwrap_or(true),
        Err(_) => false,
    }
}

struct ParsedMarker {
    operation: Option<String>,
    source: Option<String>,
    started_unix: Option<u64>,
    journal: JournalState,
    objects: Vec<String>,
}

/// Reads a marker header and its journal lines. Never trusted for deletion:
/// journaled ids only attribute bytes, and every id is checked as a digest.
fn parse_marker(path: &Path) -> ParsedMarker {
    let mut parsed = ParsedMarker {
        operation: None,
        source: None,
        started_unix: None,
        journal: JournalState::Unreadable,
        objects: Vec::new(),
    };
    let Ok(bytes) = read_regular(path, Some(MARKER_LIMIT)) else {
        return parsed;
    };
    let mut stream = serde_json::Deserializer::from_slice(&bytes).into_iter::<serde_json::Value>();
    let Some(Ok(header)) = stream.next() else {
        return parsed;
    };
    if header.get("complete") != Some(&serde_json::Value::Bool(false)) {
        return parsed;
    }
    parsed.operation = header["operation"].as_str().map(str::to_owned);
    parsed.source = header["source"].as_str().map(str::to_owned);
    parsed.started_unix = header["started_unix"].as_u64();
    if header.get("journal").is_none() {
        parsed.journal = JournalState::Absent;
        return parsed;
    }
    parsed.journal = JournalState::Present;
    let mut seen = BTreeSet::new();
    for record in stream {
        let object = record
            .ok()
            .and_then(|r| r.get("object").and_then(|o| o.as_str()).map(str::to_owned))
            .filter(|o| valid_digest(o));
        match object {
            Some(object) => {
                if seen.insert(object.clone()) {
                    parsed.objects.push(object);
                }
            }
            None => {
                parsed.journal = JournalState::Truncated;
                break;
            }
        }
    }
    parsed
}

fn scan(store: &Store, dry_run: bool) -> Result<Scan> {
    check_private_directory(&store.root)?;
    for name in ["objects", "checkpoints", "incomplete"] {
        check_private_directory(&store.root.join(name))?;
    }
    let objects_dir = store.root.join("objects");
    let incomplete_dir = store.root.join("incomplete");
    let manifests = store.list().context(
        "gc refused: a completed checkpoint manifest cannot be read; nothing was removed",
    )?;
    let mut referenced = BTreeSet::new();
    let mut missing = Vec::new();
    let mut completed = Vec::new();
    for manifest in &manifests {
        completed.push(manifest.id.clone());
        for object in referenced_objects(manifest) {
            if regular_size(&objects_dir.join(&object)).is_none() {
                missing.push(MissingReference {
                    checkpoint: manifest.id.clone(),
                    object: object.clone(),
                });
            }
            referenced.insert(object);
        }
    }

    let mut unrecognized = Vec::new();
    let mut stray_items = Vec::new();
    let mut stored = BTreeMap::new();
    for item in fs::read_dir(&objects_dir)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        let Ok(meta) = fs::symlink_metadata(item.path()) else {
            continue;
        };
        if meta.is_file() && valid_digest(&name) {
            stored.insert(name, meta.len());
        } else if meta.is_file() && name.starts_with(TEMPORARY_PREFIX) {
            stray_items.push(StrayItem {
                name: format!("objects/{name}"),
                kind: "object_temporary".into(),
                bytes: meta.len(),
            });
        } else {
            unrecognized.push(format!("objects/{name}"));
        }
    }

    let referenced_bytes = referenced
        .iter()
        .filter_map(|id| stored.get(id))
        .fold(0u64, |a, b| a.saturating_add(*b));
    let mut journaled_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut markers = Vec::new();
    let mut marker_names = Vec::new();
    let clock = now();
    for item in fs::read_dir(&incomplete_dir)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        let Ok(meta) = fs::symlink_metadata(item.path()) else {
            continue;
        };
        if meta.is_file() && name.starts_with(MARKER_PREFIX) && name.ends_with(MARKER_SUFFIX) {
            let id = name[..name.len() - MARKER_SUFFIX.len()].to_owned();
            let parsed = parse_marker(&item.path());
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            let mut record = IncompleteMarker {
                id: id.clone(),
                operation: parsed.operation,
                source: parsed.source,
                started_unix: parsed.started_unix,
                age_seconds: parsed
                    .started_unix
                    .or(modified)
                    .map(|t| clock.saturating_sub(t)),
                in_progress: marker_in_progress(&item.path()),
                journal: parsed.journal,
                journaled_objects: parsed.objects.len() as u64,
                journaled_bytes: 0,
                journaled_absent: 0,
                journaled_kept_referenced: 0,
                journaled_reclaimable_objects: 0,
                journaled_reclaimable_bytes: 0,
                marker_bytes: meta.len(),
            };
            for object in parsed.objects {
                match stored.get(&object) {
                    None => record.journaled_absent += 1,
                    Some(bytes) => {
                        record.journaled_bytes += bytes;
                        if referenced.contains(&object) {
                            record.journaled_kept_referenced += 1;
                        } else {
                            record.journaled_reclaimable_objects += 1;
                            record.journaled_reclaimable_bytes += bytes;
                            journaled_by.entry(object).or_default().push(id.clone());
                        }
                    }
                }
            }
            markers.push(record);
            marker_names.push(name);
        } else if meta.is_dir() && name.starts_with(STAGING_PREFIX) {
            stray_items.push(StrayItem {
                name: format!("incomplete/{name}"),
                kind: "capture_staging".into(),
                bytes: tree_bytes(&item.path()),
            });
        } else {
            unrecognized.push(format!("incomplete/{name}"));
        }
    }
    markers.sort_by(|a, b| (a.started_unix, &a.id).cmp(&(b.started_unix, &b.id)));
    marker_names.sort();
    stray_items.sort_by(|a, b| a.name.cmp(&b.name));
    unrecognized.sort();

    let unreferenced = stored
        .iter()
        .filter(|(id, _)| !referenced.contains(*id))
        .map(|(id, bytes)| ObjectRecord {
            id: id.clone(),
            bytes: *bytes,
            journaled_by: journaled_by.remove(id).unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    let unreferenced_object_bytes = unreferenced.iter().map(|o| o.bytes).sum::<u64>();
    let unattributed = unreferenced
        .iter()
        .filter(|o| o.journaled_by.is_empty())
        .collect::<Vec<_>>();
    let unattributed_objects = unattributed.len() as u64;
    let unattributed_bytes = unattributed.iter().map(|o| o.bytes).sum::<u64>();
    let stray_bytes = stray_items.iter().map(|s| s.bytes).sum::<u64>();
    let marker_bytes = markers.iter().map(|m| m.marker_bytes).sum::<u64>();
    let mut notes = vec![
        "Completed checkpoints are never removed; an object is reclaimable only when no completed manifest references it.".to_owned(),
        "The presence check confirms each referenced object exists as a regular file; `timber verify <id>` remains the content check.".to_owned(),
    ];
    if markers.iter().any(|m| m.in_progress) {
        notes.push("A capture is in progress; its objects stay unreferenced until it completes, and `timber gc` refuses until it finishes.".into());
    }
    if markers.iter().any(|m| m.journal == JournalState::Absent) {
        notes.push("journal absent: a marker written by an earlier Timber has no object journal; its objects are reclaimed only when no completed manifest references them.".into());
    }
    if let Some(first) = missing.first() {
        notes.push(format!(
            "`timber gc` would refuse: completed checkpoint {} references missing object {}; nothing would be removed.",
            first.checkpoint, first.object
        ));
    }
    let status = if !dry_run {
        "reclaimed"
    } else if missing.is_empty() {
        "dry_run"
    } else {
        "dry_run_would_refuse"
    };
    let report = Reclamation {
        schema: SCHEMA,
        status: status.into(),
        dry_run,
        kept: Kept {
            completed_checkpoints: completed.len() as u64,
            referenced_objects: referenced.len() as u64,
            referenced_bytes,
            unrecognized,
        },
        completed_checkpoints: completed,
        missing_references: missing,
        incomplete_markers: markers,
        unreferenced_object_bytes,
        unattributed_objects,
        unattributed_bytes,
        unreferenced_objects: unreferenced,
        reclaimable_bytes: unreferenced_object_bytes + stray_bytes + marker_bytes,
        stray_items,
        removed: None,
        notes,
    };
    Ok(Scan {
        report,
        markers: marker_names,
        referenced,
    })
}

/// `timber gc --dry-run`: reports what `timber gc` would reclaim. Writes nothing.
pub fn dry_run(store: &Store) -> Result<Reclamation> {
    Ok(scan(store, true)?.report)
}

/// `timber gc`: removes incomplete markers, unreferenced objects and stray
/// temporary items, all inside the store. Refuses before removing anything when
/// a capture is in progress or a completed manifest references a missing object.
pub fn reclaim(store: &Store) -> Result<Reclamation> {
    let _lock = store.reclaim_lock()?;
    let Scan {
        mut report,
        markers,
        referenced,
    } = scan(store, false)?;
    if let Some(first) = report.missing_references.first() {
        bail!(
            "gc refused before removing anything: completed checkpoint {} references missing object {} ({} missing references in total); run `timber verify {}` and keep the store as it is",
            first.checkpoint,
            first.object,
            report.missing_references.len(),
            first.checkpoint
        );
    }
    if let Some(live) = report.incomplete_markers.iter().find(|m| m.in_progress) {
        bail!(
            "gc refused: incomplete marker {} belongs to a capture in progress; nothing was removed",
            live.id
        );
    }
    let objects_dir = store.root.join("objects");
    let incomplete_dir = store.root.join("incomplete");
    let mut removed = Removed::default();
    // Objects first: an interruption leaves the markers, so a rerun still reports them.
    for object in &report.unreferenced_objects {
        if referenced.contains(&object.id) || !valid_digest(&object.id) {
            bail!("gc stopped: object {} is referenced", object.id);
        }
        let path = objects_dir.join(&object.id);
        if regular_size(&path).is_none() {
            continue;
        }
        fs::remove_file(&path).with_context(|| format!("remove object {}", object.id))?;
        removed.objects += 1;
        removed.object_bytes += object.bytes;
    }
    for stray in &report.stray_items {
        let path = store.root.join(&stray.name);
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        match stray.kind.as_str() {
            "object_temporary" if meta.is_file() => fs::remove_file(&path)?,
            "capture_staging" if meta.is_dir() => fs::remove_dir_all(&path)?,
            _ => continue,
        }
        removed.stray_items += 1;
        removed.stray_bytes += stray.bytes;
    }
    File::open(&objects_dir)?.sync_all()?;
    for name in &markers {
        let path = incomplete_dir.join(name);
        let Some(bytes) = regular_size(&path) else {
            continue;
        };
        fs::remove_file(&path).with_context(|| format!("remove incomplete marker {name}"))?;
        removed
            .markers
            .push(name[..name.len() - MARKER_SUFFIX.len()].to_owned());
        removed.marker_bytes += bytes;
    }
    File::open(&incomplete_dir)?.sync_all()?;
    removed.total_bytes = removed.object_bytes + removed.stray_bytes + removed.marker_bytes;
    // Confirm every completed checkpoint still has each object it references.
    for manifest in store.list()? {
        for object in referenced_objects(&manifest) {
            if regular_size(&objects_dir.join(&object)).is_none() {
                bail!(
                    "after gc, completed checkpoint {} lacks object {}",
                    manifest.id,
                    object
                );
            }
        }
    }
    report.removed = Some(removed);
    Ok(report)
}
