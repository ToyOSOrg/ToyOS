//! `mv`: a rename refused is the move refused, with nothing copied behind it.

use std::fs;

use crate::mv::*;
use toyos_tmpdir::TempDir;

/// A rename that is refused is the move refused, by name and with what to
/// do instead, and nothing is copied behind the caller's back: the source
/// stays, and nothing lands at the destination.
#[test]
fn a_move_the_rename_refuses_copies_nothing() {
    let dir = TempDir::new("mv-refused");
    let source = dir.join("stays");
    fs::write(&source, b"this file stays").unwrap();
    let dest = dir.join("elsewhere").join("moved");
    let refusal = move_file(&source, &dest).unwrap_err();
    assert!(refusal.contains("different mounts") && refusal.contains("cp then rm"), "{refusal}");
    assert_eq!(fs::read(&source).unwrap(), b"this file stays");
    assert!(!dest.exists(), "a refused move put the file at its destination");
}

#[test]
fn a_missing_source_is_refused_before_anything_is_renamed() {
    let dir = TempDir::new("mv-missing");
    let refusal = move_file(&dir.join("absent"), &dir.join("dest")).unwrap_err();
    assert!(refusal.starts_with("mv: ") && refusal.contains("absent"), "{refusal}");
    assert!(!refusal.contains("different mounts"), "a missing source was refused as a rename: {refusal}");
    assert!(!dir.join("dest").exists());
}

/// A move into a directory keeps the source's name, and the old name is
/// gone.
#[test]
fn a_move_into_a_directory_keeps_its_name_and_leaves_no_old_one() {
    let dir = TempDir::new("mv-into");
    let (source, into) = (dir.join("named"), dir.join("into"));
    fs::write(&source, b"moved, not copied").unwrap();
    fs::create_dir(&into).unwrap();
    move_file(&source, &into).unwrap();
    assert!(!source.exists(), "the old name is still there");
    assert_eq!(fs::read(into.join("named")).unwrap(), b"moved, not copied");
}
