//! Empty, missing and occupied directories retain distinct served answers.

use std::fs;
use std::io::ErrorKind;

const EMPTY: &str = "/tmp/empty_dir_stat_empty";
const MISSING: &str = "/tmp/empty_dir_stat_missing";
const WITH_FILE: &str = "/tmp/empty_dir_stat_full";

fn listing(path: &str) -> std::io::Result<usize> {
    fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>().map(|entries| entries.len())
}

fn main() {
    fs::create_dir(EMPTY).expect("mkdir empty");
    fs::create_dir(WITH_FILE).expect("mkdir occupied");
    fs::write(format!("{WITH_FILE}/f"), b"x").expect("write file");
    assert_eq!(listing(EMPTY).expect("empty listing"), 0);
    assert_eq!(listing(MISSING).expect_err("missing listing").kind(), ErrorKind::NotFound);
    assert_eq!(listing(WITH_FILE).expect("occupied listing"), 1);

    let empty = fs::metadata(EMPTY).expect("empty metadata");
    assert!(empty.is_dir() && !empty.is_file());
    assert!(fs::metadata(WITH_FILE).expect("occupied metadata").is_dir());
    assert_eq!(fs::metadata(MISSING).expect_err("missing metadata").kind(), ErrorKind::NotFound);
    assert_eq!(fs::remove_dir(MISSING).expect_err("missing rmdir").kind(), ErrorKind::NotFound);
    assert!(fs::remove_dir(WITH_FILE).is_err(), "removed an occupied directory");

    fs::remove_file(format!("{WITH_FILE}/f")).expect("remove file");
    assert_eq!(listing(WITH_FILE).expect("emptied listing"), 0);
    assert!(fs::metadata(WITH_FILE).expect("emptied metadata").is_dir());
    fs::remove_dir(WITH_FILE).expect("remove emptied directory");
    assert_eq!(listing(WITH_FILE).expect_err("removed listing").kind(), ErrorKind::NotFound);
    fs::remove_dir(EMPTY).expect("remove empty directory");
    assert!(fs::remove_dir("/tmp").is_err(), "removed a capability root");

    println!("empty dir stat: empty stats as a directory, missing refuses, emptied stays one");
    println!("rmdir outcome: missing refuses, non-empty refuses, mount refuses, empty removed");
}
