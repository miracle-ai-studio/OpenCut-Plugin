#![cfg(unix)]
use opencut_storage_budget::AttemptLease;
use std::{fs, io, os::unix::fs::symlink, path::Path};

fn reference(lease: &AttemptLease, path: &Path) -> io::Result<bool> {
    if fs::canonicalize(path)? != path {
        return Ok(false);
    }
    lease.still_names(path)
}
fn matches_reference(lease: &AttemptLease, path: &Path, expected: bool) {
    assert_eq!(reference(lease, path).unwrap(), expected);
    assert_eq!(lease.still_names_canonical(path).unwrap(), expected);
}
fn same_lookup_error(lease: &AttemptLease, path: &Path) {
    assert_eq!(
        lease.still_names_canonical(path).unwrap_err().kind(),
        reference(lease, path).unwrap_err().kind()
    );
}

#[test]
fn canonical_attempt_follows_live_names_without_accepting_replacements_or_ancestor_aliases() {
    let temporary = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temporary.path()).unwrap();
    let parent = root.join("parent");
    let attempt = parent.join("attempt");
    fs::create_dir_all(&attempt).unwrap();
    let lease = AttemptLease::acquire(&attempt).unwrap();
    matches_reference(&lease, &attempt, true);
    // Path equality, as used by the original fence, ignores an interior '.'.
    matches_reference(&lease, &parent.join(".").join("attempt"), true);
    matches_reference(
        &lease,
        &parent.join("..").join("parent").join("attempt"),
        false,
    );

    let moved = root.join("moved");
    fs::rename(&parent, &moved).unwrap();
    same_lookup_error(&lease, &attempt);
    matches_reference(&lease, &moved.join("attempt"), true);
    fs::create_dir_all(&attempt).unwrap();
    matches_reference(&lease, &attempt, false);
    fs::remove_dir(&attempt).unwrap();
    fs::remove_dir(&parent).unwrap();
    symlink(&moved, &parent).unwrap();
    // The old name reaches the held inode, but through a forbidden alias.
    assert!(lease.still_names(&attempt).unwrap());
    matches_reference(&lease, &attempt, false);
    fs::remove_file(&parent).unwrap();
    fs::rename(&moved, &parent).unwrap();
    matches_reference(&lease, &attempt, true);
    fs::remove_dir(&attempt).unwrap();
    same_lookup_error(&lease, &attempt);
    fs::create_dir(&attempt).unwrap();
    matches_reference(&lease, &attempt, false);
    fs::remove_dir(&attempt).unwrap();
    fs::write(&attempt, b"replacement after unlink").unwrap();
    matches_reference(&lease, &attempt, false);
}

#[test]
fn canonical_attempt_rejects_final_aliases_and_preserves_lookup_errors() {
    let temporary = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temporary.path()).unwrap();
    let attempt = root.join("attempt");
    fs::create_dir(&attempt).unwrap();
    let lease = AttemptLease::acquire(&attempt).unwrap();
    let alias = root.join("alias");
    symlink(&attempt, &alias).unwrap();
    matches_reference(&lease, &alias, false);
    fs::remove_file(&alias).unwrap();
    symlink(root.join("missing"), &alias).unwrap();
    same_lookup_error(&lease, &alias);
    fs::remove_file(&alias).unwrap();
    symlink(&alias, &alias).unwrap();
    same_lookup_error(&lease, &alias);
    fs::remove_file(&alias).unwrap();
    fs::rename(&attempt, root.join("held")).unwrap();
    fs::write(&attempt, b"not the held directory").unwrap();
    matches_reference(&lease, &attempt, false);
}

#[test]
fn canonical_attempt_preserves_native_path_bytes_and_open_child_lifetime() {
    #[cfg(not(target_os = "macos"))]
    use std::os::unix::ffi::OsStringExt;
    use std::{ffi::OsString, io::Read};
    let temporary = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temporary.path()).unwrap();
    #[cfg(target_os = "macos")]
    let name = OsString::from("attempt-素材");
    #[cfg(not(target_os = "macos"))]
    let name = OsString::from_vec(b"attempt-\xff".to_vec());
    let attempt = root.join(name);
    fs::create_dir(&attempt).unwrap();
    let lease = AttemptLease::acquire(&attempt).unwrap();
    matches_reference(&lease, &attempt, true);
    fs::write(attempt.join("output"), b"pinned original output").unwrap();
    let mut original = lease.open_readonly_child("output".as_ref()).unwrap();
    let moved = root.join("moved");
    fs::rename(&attempt, &moved).unwrap();
    fs::create_dir(&attempt).unwrap();
    fs::write(attempt.join("output"), b"replacement").unwrap();
    matches_reference(&lease, &attempt, false);
    assert!(AttemptLease::try_cleanup(&moved).unwrap().is_none());
    let mut bytes = Vec::new();
    original.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"pinned original output");
    drop(lease);
    assert!(AttemptLease::try_cleanup(&moved).unwrap().is_some());
}
