//! `cp`: a copy lands whole under its name or not at all, and leaves no
//! partial beside it.

use std::fs;
use std::path::Path;

use crate::cp::*;
use toyos_tmpdir::TempDir;

/// Every name in `dir` that marks a copy in progress.
fn partials(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".part"))
        .collect()
}

/// Larger than the flush interval twice over and no page multiple, so the
/// periodic flush and the partial tail both run; it lands whole under the
/// destination's name, and no partial is left beside it.
#[test]
fn a_copy_past_the_flush_interval_lands_whole_and_leaves_no_partial() {
    let dir = TempDir::new("cp-whole");
    let data: Vec<u8> = (0..2 * FLUSH_BYTES as usize + 137).map(|i| (i.wrapping_mul(31) ^ (i >> 9)) as u8).collect();
    let (source, dest) = (dir.join("source"), dir.join("dest"));
    fs::write(&source, &data).unwrap();
    copy(&source, &dest).unwrap();
    assert!(fs::read(&dest).unwrap() == data, "the copy is not the source byte for byte");
    assert_eq!(partials(&dir), Vec::<String>::new());
}

/// A refused copy leaves the destination as it was and no partial: a
/// missing source, a source that is a directory, a destination whose
/// directory is not there, and one the finished copy cannot be renamed
/// onto are each refused by name.
#[test]
fn a_refused_copy_leaves_the_destination_as_it_was_and_no_partial() {
    let dir = TempDir::new("cp-refused");
    let kept = dir.join("kept");
    fs::write(&kept, b"the destination, untouched").unwrap();

    let missing = copy(&dir.join("absent"), &kept).unwrap_err();
    assert!(missing.contains("absent"), "{missing}");
    let from_a_dir = copy(&dir, &dir.join("from-a-dir")).unwrap_err();
    assert!(from_a_dir.contains("is a directory"), "{from_a_dir}");
    let nowhere = copy(&kept, &dir.join("no-such-dir").join("dest")).unwrap_err();
    assert!(nowhere.contains("no-such-dir"), "{nowhere}");
    let occupied = dir.join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("inside"), b"x").unwrap();
    let onto_a_dir = copy(&kept, &occupied).unwrap_err();
    assert!(onto_a_dir.contains("occupied"), "{onto_a_dir}");
    assert!(occupied.join("inside").exists(), "a refused copy replaced a directory");

    assert_eq!(fs::read(&kept).unwrap(), b"the destination, untouched");
    assert!(!dir.join("from-a-dir").exists(), "a refused copy made its destination");
    assert_eq!(partials(&dir), Vec::<String>::new());
}

/// A destination that is a directory takes the source's own name.
#[test]
fn a_copy_into_a_directory_keeps_the_source_s_name() {
    let dir = TempDir::new("cp-into");
    let (source, into) = (dir.join("named"), dir.join("into"));
    fs::write(&source, b"x").unwrap();
    fs::create_dir(&into).unwrap();
    let dest = destination(&source, &into);
    assert_eq!(dest, into.join("named"));
    copy(&source, &dest).unwrap();
    assert_eq!(fs::read(dest).unwrap(), b"x");
}
