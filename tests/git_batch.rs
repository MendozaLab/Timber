use std::{fs, path::Path};
use timber_worktrees::{fsx, git};
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git::init(dir.path()).unwrap();
    dir
}
fn blob(path: &Path, bytes: &[u8]) -> String {
    String::from_utf8(
        git::run_input(
            path,
            &["hash-object", "-w", "--stdin", "--no-filters"],
            bytes,
        )
        .unwrap(),
    )
    .unwrap()
    .trim()
    .to_owned()
}
#[test]
fn batch_preserves_binary_boundaries_and_deduplicates_objects() {
    let dir = fixture();
    let a = b"\0binary\nheader-looking data\n";
    let b = vec![b'x'; 200_000];
    let oid_a = blob(dir.path(), a);
    let oid_b = blob(dir.path(), &b);
    let oids = vec![oid_b.clone(), oid_a.clone(), oid_a.clone()];
    let hashes = git::batch_blob_fingerprints(dir.path(), &oids).unwrap();
    assert_eq!(hashes.len(), 2);
    assert_eq!(hashes[&oid_a], (fsx::digest(a), a.len() as u64));
    assert_eq!(hashes[&oid_b], (fsx::digest(&b), b.len() as u64));
    let raw = git::batch_blobs(dir.path(), &oids).unwrap();
    assert_eq!(raw[&oid_a], a);
    assert_eq!(raw[&oid_b], b);
}
#[test]
fn missing_and_nonidentity_inputs_fail_without_fabricating_matches() {
    let dir = fixture();
    for ids in [
        vec!["0".repeat(40)],
        vec!["HEAD\n--batch-all-objects".into()],
        vec!["HEAD".into()],
    ] {
        assert!(git::batch_blob_fingerprints(dir.path(), &ids).is_err());
        assert!(git::batch_blobs(dir.path(), &ids).is_err());
    }
    assert!(
        git::batch_blob_fingerprints(dir.path(), &[])
            .unwrap()
            .is_empty()
    );
}
#[test]
fn streaming_fingerprint_matches_contents_and_refuses_parent_symlinks() {
    use timber_worktrees::model::{Entry, EntryKind};
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("safe")).unwrap();
    let data = vec![42; 3 * 1024 * 1024];
    fs::write(dir.path().join("safe/file"), &data).unwrap();
    let entry = Entry {
        path: "safe/file".into(),
        kind: EntryKind::File,
        mode: 0o600,
        size: 0,
        sha256: String::new(),
    };
    assert_eq!(
        fsx::fingerprint(dir.path(), &entry).unwrap(),
        (fsx::digest(&data), data.len() as u64)
    );
    std::os::unix::fs::symlink(dir.path().join("safe"), dir.path().join("alias")).unwrap();
    assert!(
        fsx::fingerprint(
            dir.path(),
            &Entry {
                path: "alias/file".into(),
                ..entry
            }
        )
        .is_err()
    );
}
