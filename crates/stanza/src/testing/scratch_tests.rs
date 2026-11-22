use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use super::scratch::{Scratch, candidate};

fn mode(path: &str) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn two_guards_get_different_paths() {
    let first = Scratch::new("guard");
    let second = Scratch::new("guard");
    assert_ne!(first.path(), second.path());
    assert!(Path::new(first.path()).is_dir());
    assert!(Path::new(second.path()).is_dir());
}

#[test]
fn a_dropped_guard_removes_its_tree() {
    let scratch = Scratch::new("dropped");
    scratch.write("a/b/c.ts", "export {};\n");
    let root = scratch.path().to_owned();

    drop(scratch);
    assert!(fs::symlink_metadata(root).is_err());
}

#[test]
fn a_guard_dropped_while_a_test_panics_removes_its_tree() {
    let mut root = String::new();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let scratch = Scratch::new("unwound");
        scratch.write("a.ts", "export {};\n");
        root = scratch.path().to_owned();
        panic!("a failed assertion");
    }));

    assert!(unwound.is_err());
    assert!(!root.is_empty());
    assert!(fs::symlink_metadata(root).is_err());
}

#[test]
fn cleanup_opens_locked_folders_and_never_follows_a_link() {
    let other = Scratch::new("linked");
    other.write("kept/a.ts", "kept\n");
    let kept = other.join("kept");
    fs::set_permissions(&kept, fs::Permissions::from_mode(0o500)).unwrap();

    let scratch = Scratch::new("locked");
    scratch.write("locked/inner/b.ts", "gone\n");
    symlink(&kept, scratch.join("locked/link")).unwrap();
    fs::set_permissions(scratch.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
    let root = scratch.path().to_owned();

    let swapped = Scratch::new("swapped");
    fs::remove_dir(swapped.path()).unwrap();
    symlink(&kept, swapped.path()).unwrap();

    drop(scratch);
    drop(swapped);
    assert!(fs::symlink_metadata(root).is_err());
    assert_eq!(mode(&kept), 0o500);
    assert_eq!(fs::read(other.join("kept/a.ts")).unwrap(), b"kept\n");
}

#[test]
fn a_taken_candidate_is_skipped_and_left_intact() {
    let taken = Scratch::new("taken");
    taken.write("a.ts", "taken\n");
    let candidates = [PathBuf::from(taken.path())]
        .into_iter()
        .chain(std::iter::repeat_with(|| candidate("retried")));

    let retried = Scratch::first_free(candidates);
    assert_ne!(retried.path(), taken.path());

    drop(retried);
    assert_eq!(fs::read(taken.join("a.ts")).unwrap(), b"taken\n");
}
