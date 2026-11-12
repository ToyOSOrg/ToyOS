//! A served directory listing larger than one reply returns every entry once.

use std::collections::BTreeSet;
use std::fs;

const DIR: &str = "/tmp/readdir_bound";
/// Long names, so few files make a listing of several replies: the names
/// alone are 128,000 bytes, nearly twice a 64 KiB reply.
const ENTRIES: usize = 640;
const NAME_BYTES: usize = 200;
const _: () = assert!(ENTRIES * NAME_BYTES > 64 * 1024);

fn name(i: usize) -> String {
    let mut name = format!("entry-{i:06}-");
    name.extend(std::iter::repeat_n('x', NAME_BYTES - name.len()));
    name
}

fn main() {
    fs::create_dir(DIR).expect("create listing directory");
    for i in 0..ENTRIES {
        if let Err(e) = fs::write(format!("{DIR}/{}", name(i)), b"") {
            // What it made goes first: a `/tmp` left full fails every later
            // job of the boot that writes it.
            for made in 0..i {
                let _ = fs::remove_file(format!("{DIR}/{}", name(made)));
            }
            let _ = fs::remove_dir(DIR);
            panic!("create file {i} of {ENTRIES}: {e:?}");
        }
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
        let name = name(i);
        assert!(names.contains(&name), "listing omitted {name}");
        fs::remove_file(format!("{DIR}/{name}")).expect("remove file");
    }
    assert_eq!(fs::read_dir(DIR).expect("emptied listing").collect::<std::io::Result<Vec<_>>>().expect("empty batch").len(), 0);
    fs::remove_dir(DIR).expect("remove listing directory");
    println!("all readdir bound tests passed");
}
