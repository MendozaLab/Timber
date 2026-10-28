// Copyright 2026 Kenneth A. Mendoza (MendozaLab). https://github.com/MendozaLab/Timber
// SPDX-License-Identifier: Apache-2.0
//! Exact supported filesystem state. Extraction never follows a stored symlink.
use crate::model::{Entry, EntryKind};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(target_os = "macos")]
use std::os::macos::fs::MetadataExt as MacMetadataExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn validate_relative(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && !value.contains('\0'),
        "empty or NUL-containing path"
    );
    ensure!(
        value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".."),
        "non-normal path component refused"
    );
    let path = Path::new(value);
    ensure!(!path.is_absolute(), "absolute path refused: {value:?}");
    for part in path.components() {
        match part {
            Component::Normal(s) => {
                let s = s.to_str().context("non-UTF-8 path unsupported")?;
                ensure!(
                    !s.eq_ignore_ascii_case(".git"),
                    "reserved Git path refused: {value:?}"
                );
            }
            _ => bail!("non-normal path component refused: {value:?}"),
        }
    }
    ensure!(
        path.to_str() == Some(value) && !value.ends_with('/') && !value.contains("//"),
        "noncanonical relative path"
    );
    Ok(())
}

pub fn validate_entries(entries: &[Entry]) -> Result<()> {
    let mut names = BTreeSet::new();
    let mut folded = BTreeSet::new();
    let leaves: BTreeSet<_> = entries
        .iter()
        .filter(|e| e.kind != EntryKind::Directory)
        .map(|e| e.path.as_str())
        .collect();
    for e in entries {
        validate_relative(&e.path)?;
        ensure!(names.insert(&e.path), "duplicate path: {:?}", e.path);
        ensure!(
            folded.insert(e.path.to_lowercase()),
            "case-colliding paths unsupported: {:?}",
            e.path
        );
        ensure!(e.mode <= 0o777, "invalid file permissions");
        if matches!(e.kind, EntryKind::File | EntryKind::Symlink) {
            ensure!(
                e.sha256.len() == 64 && e.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid content digest"
            );
        }
        for parent in Path::new(&e.path)
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            ensure!(
                !leaves.contains(parent.to_str().context("invalid parent")?),
                "file or symlink used as parent: {:?}",
                e.path
            );
        }
    }
    Ok(())
}

pub fn is_placeholder(metadata: &fs::Metadata) -> bool {
    #[cfg(target_os = "macos")]
    {
        metadata.st_flags() & 0x4000_0000 != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = metadata;
        false
    }
}

pub fn read_entry(root: &Path, entry: &Entry) -> Result<Vec<u8>> {
    validate_relative(&entry.path)?;
    let path = root.join(&entry.path);
    let meta = fs::symlink_metadata(&path)?;
    ensure!(
        !is_placeholder(&meta),
        "cloud placeholder not hydrated: {:?}",
        entry.path
    );
    let bytes = match entry.kind {
        EntryKind::Directory => Vec::new(),
        EntryKind::Submodule => bail!("submodule contents are never read: {:?}", entry.path),
        EntryKind::Symlink => {
            ensure!(meta.file_type().is_symlink(), "source type changed");
            let (parent, name) = parent_fd(root, &entry.path)?;
            let mut bytes = vec![0u8; 65536];
            // SAFETY: confined parent and NUL-terminated leaf; target remains data.
            let length = unsafe {
                libc::readlinkat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if length < 0 {
                return Err(std::io::Error::last_os_error()).context("read confined symlink");
            }
            ensure!(
                (length as usize) < bytes.len(),
                "symlink target exceeds supported size"
            );
            bytes.truncate(length as usize);
            bytes
        }
        EntryKind::File => {
            ensure!(meta.is_file(), "source type changed");
            let (parent, name) = parent_fd(root, &entry.path)?;
            // SAFETY: confined directory descriptor; never follow parent or leaf symlinks.
            let raw = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if raw < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("open source without following symlinks");
            }
            // SAFETY: openat returned a uniquely owned descriptor.
            let mut f = unsafe { File::from_raw_fd(raw) };
            let opened = f.metadata()?;
            ensure!(
                opened.is_file() && opened.ino() == meta.ino() && opened.dev() == meta.dev(),
                "source changed while opening"
            );
            ensure!(!is_placeholder(&opened), "cloud placeholder not hydrated");
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes)?;
            let after = f.metadata()?;
            ensure!(
                after.len() == opened.len()
                    && after.mtime() == opened.mtime()
                    && after.mtime_nsec() == opened.mtime_nsec(),
                "source changed during read"
            );
            bytes
        }
    };
    if !entry.sha256.is_empty() {
        ensure!(
            digest(&bytes) == entry.sha256 && bytes.len() as u64 == entry.size,
            "source changed: {:?}",
            entry.path
        );
    }
    Ok(bytes)
}

pub fn inventory(root: &Path) -> Result<Vec<Entry>> {
    inventory_filtered(root, |_| true)
}

/// Hash supported leaf contents without buffering entire media files in memory.
pub fn fingerprint(root: &Path, entry: &Entry) -> Result<(String, u64)> {
    validate_relative(&entry.path)?;
    if entry.kind != EntryKind::File {
        let bytes = read_entry(root, entry)?;
        return Ok((digest(&bytes), bytes.len() as u64));
    }
    let path = root.join(&entry.path);
    let before = fs::symlink_metadata(&path)?;
    ensure!(
        before.is_file() && !is_placeholder(&before),
        "unsupported or cloud-placeholder source"
    );
    let (parent, name) = parent_fd(root, &entry.path)?;
    // SAFETY: live confined directory and NUL-terminated component; no symlinks followed.
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error()).context("open source for fingerprint");
    }
    // SAFETY: openat returned a uniquely owned descriptor.
    let mut file = unsafe { File::from_raw_fd(raw) };
    let opened = file.metadata()?;
    ensure!(
        opened.is_file()
            && opened.ino() == before.ino()
            && opened.dev() == before.dev()
            && !is_placeholder(&opened),
        "source changed while opening fingerprint"
    );
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        count += n as u64;
    }
    let after = file.metadata()?;
    let current = fs::symlink_metadata(&path)?;
    ensure!(
        opened.len() == after.len()
            && opened.mtime() == after.mtime()
            && opened.mtime_nsec() == after.mtime_nsec()
            && opened.ctime() == after.ctime()
            && opened.ctime_nsec() == after.ctime_nsec()
            && opened.mode() == after.mode()
            && current.ino() == opened.ino()
            && current.dev() == opened.dev()
            && count == opened.len(),
        "source changed during fingerprint"
    );
    let digest = hex::encode(hash.finalize());
    if !entry.sha256.is_empty() {
        ensure!(
            entry.sha256 == digest && entry.size == count,
            "source fingerprint mismatch"
        );
    }
    Ok((digest, count))
}
pub fn inventory_filtered(root: &Path, include: impl Fn(&str) -> bool) -> Result<Vec<Entry>> {
    inventory_pruned(root, |_| false, include)
}

/// Like [`inventory_filtered`], but a pruned path is skipped entirely: no entry is
/// recorded for it and a pruned directory is never read. Checkpoint prunes only
/// gitlink paths, explicit operator exclusions and linked worktrees of the same
/// repository; any other nested repository below an unpruned directory still refuses.
pub fn inventory_pruned(
    root: &Path,
    prune: impl Fn(&str) -> bool,
    include: impl Fn(&str) -> bool,
) -> Result<Vec<Entry>> {
    let meta = fs::symlink_metadata(root)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "source root must be a real directory"
    );
    ensure!(!is_placeholder(&meta), "source root is a cloud placeholder");
    let mut result = Vec::new();
    fn visit(
        root: &Path,
        dir: &Path,
        prune: &impl Fn(&str) -> bool,
        include: &impl Fn(&str) -> bool,
        out: &mut Vec<Entry>,
    ) -> Result<()> {
        let mut items = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
        items.sort_by_key(|e| e.file_name());
        for item in items {
            let path = item.path();
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF-8 paths unsupported in version one")?
                .to_owned();
            if item.file_name() == ".git" {
                ensure!(dir == root, "nested repository refused: {:?}", relative);
                continue;
            }
            if prune(&relative) {
                continue;
            }
            validate_relative(&relative)?;
            let meta = fs::symlink_metadata(&path)?;
            let kind = if meta.file_type().is_symlink() {
                EntryKind::Symlink
            } else if meta.is_dir() {
                EntryKind::Directory
            } else if meta.is_file() {
                EntryKind::File
            } else {
                bail!("special filesystem object unsupported: {relative:?}")
            };
            if kind != EntryKind::Directory && !include(&relative) {
                continue;
            }
            ensure!(
                !is_placeholder(&meta),
                "cloud placeholder not hydrated: {relative:?}"
            );
            let mut entry = Entry {
                path: relative,
                kind: kind.clone(),
                mode: if kind == EntryKind::Symlink {
                    0o777
                } else {
                    meta.mode() & 0o777
                },
                size: 0,
                sha256: String::new(),
            };
            if kind != EntryKind::Directory {
                let bytes = read_entry(root, &entry)?;
                entry.size = bytes.len() as u64;
                entry.sha256 = digest(&bytes);
            }
            out.push(entry);
            if kind == EntryKind::Directory {
                visit(root, &path, prune, include, out)?;
            }
        }
        Ok(())
    }
    visit(root, root, &prune, &include, &mut result)?;
    result.sort_by(|a, b| a.path.cmp(&b.path));
    validate_entries(&result)?;
    Ok(result)
}

/// Directories below `root` that contain a `.git` directory or file, relative to
/// `root`, in sorted order. The root's own `.git` is not reported. Symbolic links are
/// never followed, and a nested repository's contents are never read: the walk stops
/// at the directory that holds the `.git` entry. This uses the same name-based rule
/// that makes capture refuse a nested repository, so inspection and checkpoint agree.
pub fn nested_repositories(root: &Path) -> Result<Vec<String>> {
    nested_walk(root, &|_| false)
}

/// One directory below a worktree that holds its own `.git` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedRepository {
    /// Path relative to the worktree root.
    pub path: String,
    /// Registration name when the `.git` entry is a file that Git registered as a
    /// linked worktree of this same repository (`<common dir>/worktrees/<name>`).
    /// `None` means a foreign nested repository.
    pub linked_worktree: Option<String>,
}

/// Like [`nested_repositories`], but classifies each one, and never descends into a
/// directory for which `prune` (given the path relative to `root`) returns true.
///
/// A nested repository is a linked worktree of this repository only when all of these
/// hold, and is foreign otherwise; nothing is guessed:
/// - its `.git` is a regular file (not a directory or symbolic link) of at most 4096
///   bytes holding exactly one line `gitdir: <path>`;
/// - that path, absolute or relative to the folder, reaches an existing directory
///   without passing through any symbolic link, and that directory is exactly
///   `<canonical common dir>/worktrees/<name>`;
/// - Git's own registration file `<common dir>/worktrees/<name>/gitdir` names this
///   same `.git` file, again without passing through a symbolic link.
///
/// Only the two small pointer files are read; the nested folder's contents are not.
pub fn classify_nested_repositories(
    root: &Path,
    common_dir: &Path,
    prune: impl Fn(&str) -> bool,
) -> Result<Vec<NestedRepository>> {
    let found = nested_walk(root, &prune)?;
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let root = fs::canonicalize(root)?;
    let worktrees = fs::canonicalize(common_dir)
        .with_context(|| format!("resolve common Git directory {}", common_dir.display()))?
        .join("worktrees");
    Ok(found
        .into_iter()
        .map(|path| {
            let linked_worktree = linked_worktree_name(&root.join(&path), &worktrees);
            NestedRepository {
                path,
                linked_worktree,
            }
        })
        .collect())
}

fn nested_walk(root: &Path, prune: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
    let meta = fs::symlink_metadata(root)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "source root must be a real directory"
    );
    let relative = |path: &Path| -> Result<String> {
        Ok(path
            .strip_prefix(root)?
            .to_str()
            .context("non-UTF-8 paths unsupported in version one")?
            .to_owned())
    };
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let items = fs::read_dir(&dir)
            .with_context(|| format!("read directory {}", dir.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        if dir != root && items.iter().any(|item| item.file_name() == ".git") {
            found.push(relative(&dir)?);
            continue;
        }
        for item in items {
            if item.file_name() == ".git" {
                continue;
            }
            // DirEntry::file_type does not follow symbolic links.
            if item.file_type()?.is_dir() && !prune(&relative(&item.path())?) {
                pending.push(item.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Largest Git pointer file read during classification.
const POINTER_FILE_LIMIT: u64 = 4096;

/// Bytes of a small regular file, opened without following a symbolic link. `None`
/// when the file is absent, unreadable, not regular, a cloud placeholder, too large,
/// or replaced while opening.
fn read_pointer_file(path: &Path) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > POINTER_FILE_LIMIT || is_placeholder(&meta) {
        return None;
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let opened = file.metadata().ok()?;
    if !opened.is_file() || opened.ino() != meta.ino() || opened.dev() != meta.dev() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(POINTER_FILE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= POINTER_FILE_LIMIT).then_some(bytes)
}

/// The single non-empty line of a pointer file, with at most one trailing newline.
fn pointer_line(bytes: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(bytes).ok()?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    (!text.is_empty() && !text.contains(['\n', '\r', '\0'])).then_some(text)
}

/// Resolves a path named in a pointer file, absolute or relative to the canonical
/// directory `base`. Returns it only when it exists and was reached without passing
/// through a symbolic link: the lexical resolution of `.` and `..` must equal the
/// canonical path.
fn resolve_without_links(base: &Path, named: &str) -> Option<std::path::PathBuf> {
    let joined = base.join(named);
    let mut lexical = std::path::PathBuf::new();
    for part in joined.components() {
        match part {
            Component::RootDir => lexical.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                if !lexical.pop() {
                    return None;
                }
            }
            Component::Normal(name) => lexical.push(name),
            Component::Prefix(_) => return None,
        }
    }
    let canonical = fs::canonicalize(&joined).ok()?;
    (canonical == lexical).then_some(canonical)
}

/// Registration name of the linked worktree at the canonical directory `holder`, or
/// `None` when its `.git` is not provably registered under `worktrees` (the canonical
/// `<common dir>/worktrees`). See [`classify_nested_repositories`].
fn linked_worktree_name(holder: &Path, worktrees: &Path) -> Option<String> {
    let dot_git = holder.join(".git");
    let pointer = read_pointer_file(&dot_git)?;
    let named = pointer_line(&pointer)?.strip_prefix("gitdir: ")?;
    let admin = resolve_without_links(holder, named)?;
    if admin.parent()? != worktrees {
        return None;
    }
    let name = admin.file_name()?.to_str()?.to_owned();
    let meta = fs::symlink_metadata(&admin).ok()?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return None;
    }
    let registration = read_pointer_file(&admin.join("gitdir"))?;
    let registered = resolve_without_links(&admin, pointer_line(&registration)?)?;
    (registered == dot_git).then_some(name)
}

pub fn ensure_outside(child: &Path, source: &Path) -> Result<()> {
    fn resolved(path: &Path) -> Result<std::path::PathBuf> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut existing = path.as_path();
        let mut tail = Vec::new();
        while !existing.exists() {
            let name = existing.file_name().context("invalid destination path")?;
            ensure!(name != ".." && name != ".", "non-normal destination");
            tail.push(name.to_owned());
            existing = existing.parent().context("destination parent missing")?;
        }
        let mut result = fs::canonicalize(existing)?;
        for part in tail.into_iter().rev() {
            result.push(part);
        }
        Ok(result)
    }
    let child = resolved(child)?;
    let source = resolved(source)?;
    ensure!(
        !child.starts_with(&source),
        "destination/store must be outside source: {}",
        source.display()
    );
    Ok(())
}

pub fn fresh_dir(path: &Path) -> Result<()> {
    ensure!(
        !path.exists() && fs::symlink_metadata(path).is_err(),
        "destination already exists: {}",
        path.display()
    );
    fs::create_dir(path).with_context(|| format!("create fresh destination {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn directory_fd(path: &Path) -> Result<OwnedFd> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?;
    Ok(f.into())
}
fn parent_fd(root: &Path, path: &str) -> Result<(OwnedFd, std::ffi::CString)> {
    validate_relative(path)?;
    let mut fd = directory_fd(root)?;
    let parts = path.split('/').collect::<Vec<_>>();
    for part in &parts[..parts.len() - 1] {
        let name = std::ffi::CString::new(*part)?;
        // SAFETY: live directory descriptor and NUL-terminated component; no-follow prevents escape.
        let next = unsafe {
            libc::openat(
                fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if next < 0 {
            return Err(std::io::Error::last_os_error())
                .context("open extraction parent without following symlinks");
        }
        // SAFETY: openat returned a new, uniquely owned descriptor.
        fd = unsafe { OwnedFd::from_raw_fd(next) };
    }
    Ok((
        fd,
        std::ffi::CString::new(*parts.last().context("missing filename")?)?,
    ))
}

fn confined_directory_fd(root: &Path, path: &str) -> Result<OwnedFd> {
    let (parent, name) = parent_fd(root, path)?;
    // SAFETY: traversal to parent and final directory both refuse symlinks.
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error()).context("open confined extraction directory");
    }
    // SAFETY: openat returned a new uniquely owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

pub fn materialize(
    root: &Path,
    entries: &[Entry],
    read: impl Fn(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    validate_entries(entries)?;
    let mut dirs = BTreeSet::new();
    for e in entries {
        for p in Path::new(&e.path)
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            dirs.insert(p.to_str().context("invalid path")?.to_owned());
        }
        // A recorded submodule is reproduced as an empty directory, never populated.
        if matches!(e.kind, EntryKind::Directory | EntryKind::Submodule) {
            dirs.insert(e.path.clone());
        }
    }
    let mut dirs = dirs.into_iter().collect::<Vec<_>>();
    dirs.sort_by_key(|d| d.matches('/').count());
    for dir in &dirs {
        let (fd, name) = parent_fd(root, dir)?;
        // SAFETY: descriptor/name are valid and confined to the extraction root.
        let result = unsafe { libc::mkdirat(fd.as_raw_fd(), name.as_ptr(), 0o700) };
        if result < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(e.into());
            }
            let _ = confined_directory_fd(root, dir)?;
        }
    }
    for entry in entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink))
    {
        let bytes = read(&entry.sha256)?;
        ensure!(
            digest(&bytes) == entry.sha256 && bytes.len() as u64 == entry.size,
            "stored content mismatch"
        );
        let (fd, name) = parent_fd(root, &entry.path)?;
        match entry.kind {
            EntryKind::File => {
                // SAFETY: exclusive no-follow creation under a confined directory descriptor.
                let raw = unsafe {
                    libc::openat(
                        fd.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_WRONLY
                            | libc::O_CREAT
                            | libc::O_EXCL
                            | libc::O_NOFOLLOW
                            | libc::O_CLOEXEC,
                        0o600,
                    )
                };
                if raw < 0 {
                    return Err(std::io::Error::last_os_error()).context("create extracted file");
                }
                // SAFETY: openat returned a uniquely owned new descriptor.
                let mut file = unsafe { File::from_raw_fd(raw) };
                file.write_all(&bytes)?;
                file.set_permissions(fs::Permissions::from_mode(entry.mode))?;
                file.sync_all()?;
            }
            EntryKind::Symlink => {
                let target = std::ffi::CString::new(bytes)?;
                // SAFETY: symlink target is data; no later extraction follows it.
                if unsafe { libc::symlinkat(target.as_ptr(), fd.as_raw_fd(), name.as_ptr()) } < 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("create extracted symlink");
                }
            }
            EntryKind::Directory | EntryKind::Submodule => {}
        }
    }
    for dir in dirs.iter().rev() {
        let mode = entries
            .iter()
            .find(|e| e.path == *dir && e.kind == EntryKind::Directory)
            .map(|e| e.mode)
            .unwrap_or(0o755);
        let fd = confined_directory_fd(root, dir)?;
        // SAFETY: directory descriptor remains owned and valid.
        if unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}
