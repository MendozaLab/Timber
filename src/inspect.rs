//! Source reads only; never ask Git to refresh an index or run content filters.
use crate::{
    fsx, git,
    model::{Entry, EntryKind},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::os::unix::fs::MetadataExt;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub fn paths(repo: &Path, args: &[&str]) -> Result<Vec<String>> {
    git::run(repo, args)?
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let path = String::from_utf8(p.to_vec()).context("non-UTF-8 path unsupported")?;
            fsx::validate_relative(&path)?;
            Ok(path)
        })
        .collect()
}

pub fn index(repo: &Path) -> Result<BTreeMap<String, (String, String)>> {
    records(&git::run(repo, &["ls-files", "--stage", "-z"])?, true)
}
fn records(data: &[u8], index: bool) -> Result<BTreeMap<String, (String, String)>> {
    let mut entries = BTreeMap::new();
    for record in data.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .context("invalid Git path record")?;
        let header = std::str::from_utf8(&record[..tab])?
            .split_whitespace()
            .collect::<Vec<_>>();
        ensure!(header.len() == 3, "invalid Git index/tree record");
        if index {
            ensure!(header[2] == "0", "unresolved index stages unsupported");
        }
        let path =
            String::from_utf8(record[tab + 1..].to_vec()).context("non-UTF-8 path unsupported")?;
        fsx::validate_relative(&path)?;
        let oid = if index { header[1] } else { header[2] };
        entries.insert(path, (header[0].to_owned(), oid.to_owned()));
    }
    Ok(entries)
}
fn file_info(root: &Path, path: &str) -> Value {
    match fs::symlink_metadata(root.join(path)) {
        Ok(m) => {
            json!({"path":path,"bytes":m.len(),"kind":if m.file_type().is_symlink(){"symlink"}else if m.is_dir(){"directory"}else{"file"},"cloud_placeholder":fsx::is_placeholder(&m)})
        }
        Err(e) => json!({"path":path,"error":e.to_string()}),
    }
}

pub fn worktree(path: &Path) -> Result<Value> {
    let head = git::text(path, &["rev-parse", "--verify", "HEAD"])?;
    let index = index(path)?;
    let base = records(&git::run(path, &["ls-tree", "-r", "-z", "HEAD"])?, false)?;
    let mut staged = Vec::new();
    for name in index
        .keys()
        .chain(base.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if index.get(name) != base.get(name) {
            staged.push(name.to_owned());
        }
    }
    let mut working = Vec::new();
    let mut blockers = Vec::new();
    if let Err(error) = crate::capture::validate_worktree(path) {
        blockers.push(format!("Checkpoint unavailable: {error:#}"));
    }
    for (name, (mode, oid)) in &index {
        if mode == "160000" {
            blockers.push(format!("submodule unsupported: {name}"));
            continue;
        }
        let metadata = match fs::symlink_metadata(path.join(name)) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                working.push(name.clone());
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if fsx::is_placeholder(&metadata) {
            blockers.push(format!("cloud placeholder: {name}"));
            continue;
        }
        if !metadata.is_file() && !metadata.file_type().is_symlink() {
            working.push(name.clone());
            continue;
        }
        let entry = Entry {
            path: name.clone(),
            kind: if metadata.file_type().is_symlink() {
                EntryKind::Symlink
            } else {
                EntryKind::File
            },
            mode: metadata.mode() & 0o777,
            size: 0,
            sha256: String::new(),
        };
        let bytes = fsx::read_entry(path, &entry)?;
        let indexed = git::run(path, &["cat-file", "blob", oid])?;
        let executable = metadata.mode() & 0o111 != 0;
        if bytes != indexed
            || (mode == "120000") != metadata.file_type().is_symlink()
            || (mode == "100755" && !executable)
            || (mode == "100644" && executable)
        {
            working.push(name.clone());
        }
    }
    let untracked = paths(path, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let ignored = paths(
        path,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )?;
    Ok(
        json!({"path":path,"kind":"registered_worktree","head":head,"staged_paths":staged,"working_paths":working,"untracked":untracked.iter().map(|p|file_info(path,p)).collect::<Vec<_>>(),"ignored":ignored.iter().map(|p|file_info(path,p)).collect::<Vec<_>>(),"blockers":blockers,"note":"Working differences compare raw bytes; no clean filters or index refresh are run."}),
    )
}

pub fn inspect(repo: &Path, folders: &[PathBuf]) -> Result<Value> {
    let repo = fs::canonicalize(repo)?;
    let data = git::run(&repo, &["worktree", "list", "--porcelain", "-z"])?;
    let mut worktrees = Vec::new();
    let mut current = BTreeMap::<String, String>::new();
    fn finish(current: &mut BTreeMap<String, String>, result: &mut Vec<Value>) {
        if let Some(path) = current.get("worktree") {
            let root = Path::new(path);
            let registration = current.clone();
            let value = if !root.exists() {
                json!({"path":path,"kind":"missing_worktree_registration","registration":registration,"blockers":["Worktree directory is absent; metadata is not pruned."]})
            } else {
                match worktree(root) {
                    Ok(mut v) => {
                        v["registration"] = json!(registration);
                        v
                    }
                    Err(e) => {
                        json!({"path":path,"kind":"inspection_unavailable","registration":registration,"blockers":[e.to_string()]})
                    }
                }
            };
            result.push(value);
        }
        current.clear();
    }
    for item in data.split(|b| *b == 0) {
        if item.is_empty() {
            finish(&mut current, &mut worktrees);
            continue;
        }
        let line = std::str::from_utf8(item).context("non-UTF-8 worktree path unsupported")?;
        let (key, value) = line.split_once(' ').unwrap_or((line, "true"));
        current.insert(key.to_owned(), value.to_owned());
    }
    finish(&mut current, &mut worktrees);
    let mut preserved = Vec::new();
    for path in folders {
        let path = fs::canonicalize(path)?;
        let receipt = path.join("receipt.json").is_file();
        ensure!(path.is_dir(), "folder must be a directory");
        // Metadata only: inspection does not read ignored or preserved payload bytes.
        let mut pending = vec![path.clone()];
        let mut files = 0u64;
        let mut bytes = 0u64;
        let mut blockers = Vec::new();
        while let Some(dir) = pending.pop() {
            ensure!(
                files < 100_000,
                "folder metadata inventory exceeds 100000 entries"
            );
            let meta = fs::symlink_metadata(&dir)?;
            if fsx::is_placeholder(&meta) {
                blockers.push("Cloud-placeholder directory not traversed".to_owned());
                continue;
            }
            for item in fs::read_dir(dir)? {
                ensure!(
                    files < 100_000,
                    "folder metadata inventory exceeds 100000 entries"
                );
                let item = item?;
                let meta = fs::symlink_metadata(item.path())?;
                files += 1;
                bytes = bytes.saturating_add(meta.len());
                if fsx::is_placeholder(&meta) {
                    blockers.push(format!("cloud placeholder: {:?}", item.file_name()));
                } else if meta.is_dir() && !meta.file_type().is_symlink() {
                    if item.file_name() == ".git" {
                        blockers.push(
                            "Nested Git metadata; folder cannot be imported as plain files"
                                .to_owned(),
                        );
                    } else {
                        pending.push(item.path());
                    }
                }
            }
        }
        preserved.push(json!({"path":path,"kind":if receipt{"preservation_folder_with_receipt"}else{"unmanaged_folder"},"entries":files,"metadata_bytes":bytes,"historical_completeness":"unknown","receipt_claims":"not independently verified","blockers":blockers}));
    }
    Ok(
        json!({"schema":1,"repo":repo,"worktrees":worktrees,"folders":preserved,"notes":["Inspection never prunes, moves, stages, commits, or deletes source work.","Ignored files are listed from metadata; their contents are not captured without explicit selection."]}),
    )
}
