//! A served file's `fstat` through a descriptor holding its paged object is
//! answered by the kernel from what the object publishes, without a request.
//! After a write, a truncate and a set time, it agrees with the server's
//! `stat` of the file's path on size and mtime, and on every field the open
//! answered.
//!
//! A guest test because the two answers come from two components, the
//! kernel's object and the file server's volume, which only a guest runs
//! together; each side's rule is host-tested (`fileserver` pages, `fat`).

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
    drop(f);
    fs::remove_file(FILE).unwrap();
    println!("a_held_files_fstat_is_its_servers_stat: passed");
}
