//! `/tmp` holds files by what each costs: an empty file holds no page and a
//! few hundred bytes of its server's, so many more of them than the guest has
//! mebibytes are made, each opened to read and closed, which tells its server
//! its last holder went, and removed.

use std::fs;

const DIR: &str = "/tmp/tmp_empty_files";

/// More than one for each MiB of the 4 GiB guest its row boots.
const FILES: usize = 10_000;

fn name(i: usize) -> String {
    format!("{DIR}/f{i:05}")
}

fn main() {
    fs::create_dir(DIR).expect("tmp_empty_files: its directory");
    for i in 0..FILES {
        if let Err(e) = fs::File::create_new(name(i)).and_then(|_| fs::File::open(name(i))) {
            // What it made goes first: a `/tmp` left full fails every later
            // job of the boot that writes it.
            for made in 0..=i {
                let _ = fs::remove_file(name(made));
            }
            let _ = fs::remove_dir(DIR);
            panic!("tmp_empty_files: create and open file {i} of {FILES}: {e:?}");
        }
    }
    for i in 0..FILES {
        fs::remove_file(name(i)).unwrap_or_else(|e| panic!("tmp_empty_files: remove file {i}: {e:?}"));
    }
    fs::remove_dir(DIR).expect("tmp_empty_files: the emptied directory removed");
    println!("tmp_empty_files: {FILES} empty files made, read-opened and removed");
}
