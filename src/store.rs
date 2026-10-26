//! Immutable, hash-addressed checkpoint storage. No source repository is written.
use crate::{
    fsx,
    model::{EntryKind, Manifest, SCHEMA},
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
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

fn check_private_directory(path: &Path) -> Result<()> {
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
        "store directory must be private (mode 0700): {}",
        path.display()
    );
    Ok(())
}

fn read_regular(path: &Path, maximum: Option<u64>) -> Result<Vec<u8>> {
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
        Ok(Self { root })
    }

    pub fn open(path: &Path) -> Result<Self> {
        private_directory(path)?;
        let root = path.canonicalize()?;
        for name in ["objects", "checkpoints", "incomplete"] {
            private_directory(&root.join(name))?;
        }
        Ok(Self { root })
    }

    fn write_immutable(&self, directory: &str, name: &str, bytes: &[u8]) -> Result<()> {
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
            return Ok(());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&parent)?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => (),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    read_regular(&path, None)? == bytes,
                    "concurrent immutable stored item differs"
                );
            }
            Err(error) => return Err(error.error.into()),
        }
        File::open(&parent)?.sync_all()?;
        Ok(())
    }

    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        let hash = fsx::digest(bytes);
        self.write_immutable("objects", &hash, bytes)?;
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
        for entry in &manifest.entries {
            if entry.kind != EntryKind::Directory {
                self.get(&entry.sha256)?;
            }
        }
        if let Some(git) = &manifest.git {
            self.get(&git.bundle)?;
            for entry in &git.index {
                self.get(&entry.blob)?;
            }
        }
        for dependency in &manifest.captured_dependencies {
            self.get(&dependency.bundle)?;
        }
        self.write_immutable(
            "checkpoints",
            &format!("{}.json", manifest.id),
            &serde_json::to_vec_pretty(manifest)?,
        )
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
    for entry in &manifest.entries {
        ensure!(entry.mode <= 0o777, "unsupported permission bits");
        if entry.kind != EntryKind::Directory {
            ensure!(valid_digest(&entry.sha256), "invalid entry digest");
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
    } else if manifest.source_kind == "worktree" {
        bail!("worktree checkpoint lacks Git state");
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
