//! A served descriptor and its path agree on metadata after writes,
//! truncation and explicit timestamps. An unlinked descriptor retains its
//! identity while its link count changes.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::toyos::fs::MetadataExt;
use std::time::{Duration, SystemTime};

const FILE: &str = "/home/a-held-files-fstat";

fn agree(f: &File, what: &str) {
    let held = f.metadata().expect("fstat");
    let served = fs::metadata(FILE).expect("stat");
    let fields = |m: &fs::Metadata| (m.len(), m.modified().ok(), m.is_file(), m.ino(), m.dev(), m.nlink());
    assert_eq!(fields(&held), fields(&served), "after {what}: the held fstat, then the server's stat");
}

fn main() {
    let _ = fs::remove_file(FILE);
    let mut f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(FILE).expect("open");
    agree(&f, "the open");
    f.write_all(&[7u8; 5000]).expect("write");
    agree(&f, "a write");
    assert_eq!(f.metadata().unwrap().len(), 5000, "the write's size");
    f.set_len(100).expect("truncate");
    agree(&f, "a truncate");
    let then = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    f.set_modified(then).expect("set the mtime");
    agree(&f, "a set time");
    assert_eq!(f.metadata().unwrap().modified().unwrap(), then, "the time set");
    fs::remove_file(FILE).unwrap();
    assert_eq!(f.metadata().unwrap().nlink(), 0, "unlink updates an already open descriptor");
    assert_eq!(f.metadata().unwrap().len(), 100);
    drop(f);
    println!("a_held_files_fstat_is_its_servers_stat: passed");
}
