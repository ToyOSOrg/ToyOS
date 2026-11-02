//! A served directory listing larger than one reply returns every entry once.

use std::collections::BTreeSet;
use std::fs;

const DIR: &str = "/tmp/readdir_bound";
const ENTRIES: usize = 6_000;

fn main() {
    fs::create_dir(DIR).expect("create listing directory");
    // The names alone exceed a 64 KiB reply, independently of wire overhead.
    for i in 0..ENTRIES {
        fs::write(format!("{DIR}/entry-{i:06}"), b"").expect("create file");
    }
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(DIR).expect("open listing") {
        let entry = entry.expect("read listing batch");
        assert!(entry.file_type().expect("entry type").is_file());
        let name = entry.file_name().into_string().expect("UTF-8 name");
        assert!(names.insert(name), "listing repeated an entry");
    }
    assert_eq!(names.len(), ENTRIES, "listing omitted entries");
    for i in 0..ENTRIES {
        let name = format!("entry-{i:06}");
        assert!(names.contains(&name), "listing omitted {name}");
        fs::remove_file(format!("{DIR}/{name}")).expect("remove file");
    }
    assert_eq!(fs::read_dir(DIR).expect("emptied listing").collect::<std::io::Result<Vec<_>>>().expect("empty batch").len(), 0);
    fs::remove_dir(DIR).expect("remove listing directory");
    println!("all readdir bound tests passed");
}
