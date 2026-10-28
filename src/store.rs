//! Immutable, hash-addressed checkpoint storage. No source repository is written.
use crate::{
    fsx,
    model::{EntryKind, Manifest, SCHEMA},
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
    /// Present only on the handle a capture writes through: every object that run
    /// adds (not one that already existed) is appended to its incomplete marker.
    journal: Option<Arc<Journal>>,
}

/// Version of the object journal carried by incomplete markers. A marker header
/// without a `journal` key was written by an earlier Timber and has no journal.
pub const JOURNAL_FORMAT: u32 = 1;

/// The append-only object journal of one capture's incomplete marker. Each object
/// the run adds is written as one JSON line after the header and synced before the
/// capture continues, so an aborted run leaves an accurate record of what it added.
#[derive(Debug)]
pub struct Journal {
    state: Mutex<JournalState>,
}

#[derive(Debug)]
struct JournalState {
    file: File,
    objects: u64,
    bytes: u64,
}

impl Journal {
    pub(crate) fn new(file: File) -> Self {
        Self {
            state: Mutex::new(JournalState {
                file,
                objects: 0,
                bytes: 0,
            }),
        }
    }

    fn record(&self, digest: &str, bytes: u64) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capture journal is unavailable"))?;
        let mut line = serde_json::to_vec(&serde_json::json!({"object": digest, "bytes": bytes}))?;
        line.push(b'\n');
        state
            .file
            .write_all(&line)
            .context("append to capture journal")?;
        state.file.sync_data().context("sync capture journal")?;
        state.objects += 1;
        state.bytes += bytes;
        Ok(())
    }

    /// Objects this run added to the store, and their bytes.
    pub fn totals(&self) -> (u64, u64) {
        self.state
            .lock()
            .map(|s| (s.objects, s.bytes))
            .unwrap_or((0, 0))
    }
}

/// An advisory lock on the store root, released when dropped. Captures hold it
/// shared for their whole run; `gc` holds it exclusively, so reclamation never
/// overlaps a capture in progress.
#[derive(Debug)]
pub struct StoreLock {
    _file: File,
}

/// Applies `flock` to an open file. `Ok(false)` means another holder conflicts.
pub(crate) fn try_flock(file: &File, exclusive: bool) -> Result<bool> {
    let operation = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    } | libc::LOCK_NB;
    // SAFETY: the descriptor is owned by `file` and stays open for the call.
    if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(false);
    }
    Err(error).context("lock store item")
}

/// Like [`try_flock`], with a short bounded wait (about one second) that covers a
/// holder just finishing, including a descriptor briefly duplicated into a child
/// process between its fork and exec.
pub(crate) fn flock_waiting(file: &File, exclusive: bool) -> Result<bool> {
    for attempt in 0..40 {
        if try_flock(file, exclusive)? {
            return Ok(true);
        }
        if attempt < 39 {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    Ok(false)
}

/// Every stored object a completed manifest depends on: captured file and symbolic
/// link contents, the history bundle, staged index blobs, and dependency bundles.
pub fn referenced_objects(manifest: &Manifest) -> BTreeSet<String> {
    let mut objects = BTreeSet::new();
    for entry in &manifest.entries {
        if matches!(entry.kind, EntryKind::File | EntryKind::Symlink) {
            objects.insert(entry.sha256.clone());
        }
    }
    if let Some(git) = &manifest.git {
        objects.insert(git.bundle.clone());
        for entry in &git.index {
            objects.insert(entry.blob.clone());
        }
    }
    for dependency in &manifest.captured_dependencies {
        objects.insert(dependency.bundle.clone());
    }
    objects
}

pub fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn manifest_id(manifest: &Manifest) -> Result<String> {
    let mut value = manifest.clone();
    value.id.clear();
    Ok(fsx::digest(&serde_json::to_vec(&value)?))
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "store directory must not be a symlink: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Err(error) => return Err(error.into()),
    }
    check_private_directory(path)
}

pub(crate) fn check_private_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "store directory must not be a symlink"
    );
    // SAFETY: geteuid has no arguments or side effects and returns the process user.
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "store directory must belong to the current user"
    );
    ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "store directory must be private (mode 0700): {} (a directory created beforehand with mkdir usually has mode 0755; run `chmod 700` on it, or let Timber create the store path itself)",
        path.display()
    );
    Ok(())
}

pub(crate) fn read_regular(path: &Path, maximum: Option<u64>) -> Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("open stored object {}", path.display()))?;
    let meta = file.metadata()?;
    ensure!(meta.is_file(), "stored item is not a regular file");
    if let Some(limit) = maximum {
        ensure!(meta.len() <= limit, "stored manifest exceeds limit");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

impl Store {
    /// Open an existing store without creating directories or modifying permissions.
    pub fn existing(path: &Path) -> Result<Self> {
        check_private_directory(path)?;
        let root = path.canonicalize()?;
        for name in ["objects", "checkpoints", "incomplete"] {
            check_private_directory(&root.join(name))?;
        }
        Ok(Self {
            root,
            journal: None,
        })
    }

    pub fn open(path: &Path) -> Result<Self> {
        private_directory(path)?;
        let root = path.canonicalize()?;
        for name in ["objects", "checkpoints", "incomplete"] {
            private_directory(&root.join(name))?;
        }
        Ok(Self {
            root,
            journal: None,
        })
    }

    /// A handle that journals every object it adds into the given capture journal.
    pub fn journaled(&self, journal: Arc<Journal>) -> Self {
        Self {
            root: self.root.clone(),
            journal: Some(journal),
        }
    }

    fn lock(&self, exclusive: bool) -> Result<Option<StoreLock>> {
        check_private_directory(&self.root)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(&self.root)
            .context("open store root for locking")?;
        Ok(flock_waiting(&file, exclusive)?.then_some(StoreLock { _file: file }))
    }

    /// Shared lock held by a capture for its whole run. Refused while `timber gc`
    /// is reclaiming this store.
    pub fn capture_lock(&self) -> Result<StoreLock> {
        self.lock(false)?
            .context("store is being reclaimed by `timber gc`; retry the capture after it finishes")
    }

    /// Exclusive lock for reclamation. Refused while any capture is in progress.
    pub fn reclaim_lock(&self) -> Result<StoreLock> {
        self.lock(true)?.context(
            "gc refused: a capture or another gc is in progress in this store; nothing was removed; retry after it finishes",
        )
    }

    /// Writes an immutable item; `Ok(true)` only when this call created it.
    fn write_immutable(&self, directory: &str, name: &str, bytes: &[u8]) -> Result<bool> {
        check_private_directory(&self.root)?;
        let parent = self.root.join(directory);
        check_private_directory(&parent)?;
        let meta = fs::symlink_metadata(&parent)?;
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "store directory changed"
        );
        let path = parent.join(name);
        if path.try_exists()? {
            ensure!(
                read_regular(&path, None)? == bytes,
                "immutable stored item differs: {name}"
            );
            return Ok(false);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&parent)?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        let created = match temporary.persist_noclobber(&path) {
            Ok(_) => true,
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    read_regular(&path, None)? == bytes,
                    "concurrent immutable stored item differs"
                );
                false
            }
            Err(error) => return Err(error.error.into()),
        };
        File::open(&parent)?.sync_all()?;
        Ok(created)
    }

    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        let hash = fsx::digest(bytes);
        if self.write_immutable("objects", &hash, bytes)? {
            if let Some(journal) = &self.journal {
                journal.record(&hash, bytes.len() as u64)?;
            }
        }
        Ok(hash)
    }

    pub fn get(&self, digest: &str) -> Result<Vec<u8>> {
        check_private_directory(&self.root)?;
        check_private_directory(&self.root.join("objects"))?;
        ensure!(valid_digest(digest), "invalid content digest");
        let bytes = read_regular(&self.root.join("objects").join(digest), None)?;
        ensure!(
            fsx::digest(&bytes) == digest,
            "stored object failed hash verification: {digest}"
        );
        Ok(bytes)
    }

    pub fn load(&self, id: &str) -> Result<Manifest> {
        check_private_directory(&self.root)?;
        check_private_directory(&self.root.join("checkpoints"))?;
        ensure!(valid_digest(id), "invalid checkpoint identity");
        let bytes = read_regular(
            &self.root.join("checkpoints").join(format!("{id}.json")),
            Some(64 * 1024 * 1024),
        )?;
        let manifest: Manifest =
            serde_json::from_slice(&bytes).context("invalid checkpoint manifest")?;
        validate_manifest(&manifest)?;
        ensure!(
            manifest.id == id && manifest_id(&manifest)? == id,
            "checkpoint manifest identity mismatch"
        );
        Ok(manifest)
    }

    pub fn list(&self) -> Result<Vec<Manifest>> {
        check_private_directory(&self.root)?;
        check_private_directory(&self.root.join("checkpoints"))?;
        let mut result = Vec::new();
        for entry in fs::read_dir(self.root.join("checkpoints"))? {
            let path = entry?.path();
            if path.extension().and_then(|p| p.to_str()) == Some("json") {
                let id = path
                    .file_stem()
                    .and_then(|p| p.to_str())
                    .context("non UTF-8 checkpoint filename")?;
                result.push(self.load(id)?);
            }
        }
        result.sort_by(|a, b| (a.created_unix, &a.id).cmp(&(b.created_unix, &b.id)));
        Ok(result)
    }

    pub fn publish(&self, manifest: &Manifest) -> Result<()> {
        validate_manifest(manifest)?;
        ensure!(
            manifest_id(manifest)? == manifest.id,
            "manifest digest does not match identity"
        );
        for object in referenced_objects(manifest) {
            self.get(&object)?;
        }
        self.write_immutable(
            "checkpoints",
            &format!("{}.json", manifest.id),
            &serde_json::to_vec_pretty(manifest)?,
        )
        .map(|_| ())
    }
}

pub fn validate_manifest(manifest: &Manifest) -> Result<()> {
    ensure!(manifest.schema == SCHEMA, "unsupported checkpoint schema");
    ensure!(valid_digest(&manifest.id), "invalid checkpoint identity");
    ensure!(
        manifest.source_kind == "worktree" || manifest.source_kind == "folder",
        "unknown source kind"
    );
    ensure!(
        manifest.capture.scans_matched,
        "inconsistent checkpoint cannot be completed"
    );
    ensure!(
        !manifest.capture.writer_exclusion_enforced,
        "v0.1 cannot claim enforced writer exclusion"
    );
    let source_path = Path::new(&manifest.source_path);
    ensure!(source_path.is_absolute(), "source root must be absolute");
    ensure!(
        source_path.components().all(|part| !matches!(
            part,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )),
        "source root must be normalized"
    );
    fsx::validate_entries(&manifest.entries)?;
    let mut submodule_paths = std::collections::BTreeSet::new();
    for entry in &manifest.entries {
        ensure!(entry.mode <= 0o777, "unsupported permission bits");
        match entry.kind {
            EntryKind::File | EntryKind::Symlink => {
                ensure!(valid_digest(&entry.sha256), "invalid entry digest")
            }
            EntryKind::Submodule => {
                ensure!(
                    entry.sha256.is_empty() && entry.size == 0 && entry.mode == 0,
                    "submodule entry carries content: {}",
                    entry.path
                );
                submodule_paths.insert(entry.path.as_str());
            }
            EntryKind::Directory => {}
        }
    }
    for exclusion in &manifest.excluded {
        fsx::validate_relative(&exclusion.path)?;
    }
    if let Some(git) = &manifest.git {
        ensure!(
            manifest.source_kind == "worktree",
            "folder cannot claim historical Git state"
        );
        ensure!(
            !git.protected_roots.is_empty(),
            "worktree checkpoint lacks protected roots"
        );
        let mut protected = std::collections::BTreeSet::new();
        for root in &git.protected_roots {
            let root = Path::new(root);
            ensure!(root.is_absolute(), "protected root must be absolute");
            ensure!(
                root.components().all(|part| !matches!(
                    part,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )),
                "protected root must be normalized"
            );
            ensure!(protected.insert(root), "duplicate protected root");
        }
        ensure!(
            protected.iter().any(|root| *root == source_path
                || match (root.canonicalize(), source_path.canonicalize()) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => false,
                }),
            "protected roots must include the selected source root"
        );
        ensure!(git.object_format == "sha1", "unsupported Git object format");
        ensure!(valid_oid(&git.head), "invalid HEAD object identity");
        ensure!(valid_digest(&git.bundle), "invalid bundle digest");
        let mut paths = std::collections::BTreeSet::new();
        let mut logical_files = Vec::new();
        for entry in &git.index {
            fsx::validate_relative(&entry.path)?;
            ensure!(paths.insert(&entry.path), "duplicate index path");
            ensure!(
                ["100644", "100755", "120000"].contains(&entry.mode.as_str()),
                "unsupported index mode"
            );
            ensure!(
                valid_oid(&entry.oid) && valid_digest(&entry.blob),
                "invalid index object identity"
            );
            logical_files.push(crate::model::Entry {
                path: entry.path.clone(),
                kind: if entry.mode == "120000" {
                    EntryKind::Symlink
                } else {
                    EntryKind::File
                },
                mode: 0o644,
                size: 0,
                sha256: entry.blob.clone(),
            });
        }
        fsx::validate_entries(&logical_files)?;
        let mut recorded = std::collections::BTreeSet::new();
        for submodule in &git.submodules {
            fsx::validate_relative(&submodule.path)?;
            ensure!(
                recorded.insert(submodule.path.as_str()),
                "duplicate submodule record: {}",
                submodule.path
            );
            ensure!(
                !paths.contains(&submodule.path),
                "submodule path is also an index file: {}",
                submodule.path
            );
            ensure!(
                valid_oid(&submodule.gitlink_commit)
                    && submodule
                        .checked_out_commit
                        .as_deref()
                        .is_none_or(valid_oid),
                "invalid submodule commit identity: {}",
                submodule.path
            );
        }
        ensure!(
            recorded == submodule_paths,
            "submodule records and submodule entries differ"
        );
    } else if manifest.source_kind == "worktree" {
        bail!("worktree checkpoint lacks Git state");
    } else {
        ensure!(
            submodule_paths.is_empty(),
            "folder imports cannot record submodules"
        );
    }
    if let Some(map) = &manifest.import_map {
        validate_import_map(map)?;
        ensure!(
            manifest.source_kind == "folder",
            "import mapping is only valid for folders"
        );
    }
    let mut dependency_groups = std::collections::BTreeSet::new();
    for captured in &manifest.captured_dependencies {
        ensure!(
            manifest.source_kind == "folder",
            "captured external dependencies belong to folder imports"
        );
        crate::dependencies::validate_declaration(&captured.dependency)?;
        ensure!(
            dependency_groups.insert(&captured.dependency.group),
            "duplicate captured dependency group"
        );
        ensure!(
            valid_digest(&captured.bundle),
            "invalid captured dependency bundle digest"
        );
        ensure!(
            !captured.protected_roots.is_empty(),
            "captured dependency lacks protected source roots"
        );
        for root in &captured.protected_roots {
            let root = Path::new(root);
            ensure!(
                root.is_absolute()
                    && root.components().all(|part| !matches!(
                        part,
                        std::path::Component::CurDir | std::path::Component::ParentDir
                    )),
                "captured dependency protected root must be absolute and normalized"
            );
        }
        let declaration = manifest
            .import_map
            .as_ref()
            .and_then(|map| {
                map.git_dependencies
                    .iter()
                    .find(|d| d.group == captured.dependency.group)
            })
            .context("captured dependency has no matching declaration")?;
        ensure!(
            declaration == &captured.dependency,
            "captured dependency differs from import declaration"
        );
    }
    Ok(())
}

pub fn valid_oid(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn validate_import_map(map: &crate::model::ImportMap) -> Result<()> {
    ensure!(map.schema == SCHEMA, "unsupported import map schema");
    let mut stored = std::collections::BTreeSet::new();
    let mut targets = std::collections::BTreeSet::new();
    for file in &map.files {
        fsx::validate_relative(&file.stored_path)?;
        fsx::validate_relative(&file.target_path)?;
        ensure!(
            stored.insert(&file.stored_path) && targets.insert(&file.target_path),
            "duplicate import mapping"
        );
        if let Some(base) = &file.base_sha256 {
            ensure!(valid_digest(base), "invalid base digest");
        }
    }
    let mut dependency_groups = std::collections::BTreeSet::new();
    for dependency in &map.git_dependencies {
        crate::dependencies::validate_declaration(dependency)?;
        ensure!(
            dependency_groups.insert(&dependency.group),
            "duplicate dependency group"
        );
    }
    Ok(())
}
