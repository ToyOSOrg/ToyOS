//! `/tmp` holds files by what each costs: an empty file holds no page and a
//! few hundred bytes of its server's, so more of them than the machine has
//! mebibytes of memory are made, each opened to read and closed, which tells
//! its server its last holder went, and removed.

use std::fs;

const DIR: &str = "/tmp/tmp_empty_files";

/// Files made past one for each MiB of the machine's memory.
const MARGIN: usize = 1024;

fn name(i: usize) -> String {
    format!("{DIR}/f{i:07}")
}

fn main() {
    let files = (toyos::system::memory_total() >> 20) as usize + MARGIN;
    fs::create_dir(DIR).expect("tmp_empty_files: its directory");
    for i in 0..files {
        if let Err(e) = fs::File::create_new(name(i)).and_then(|_| fs::File::open(name(i))) {
            // What it made goes first: a `/tmp` left full fails every later
            // job of the boot that writes it.
            for made in 0..=i {
                let _ = fs::remove_file(name(made));
            }
            let _ = fs::remove_dir(DIR);
            panic!("tmp_empty_files: create and open file {i} of {files}: {e:?}");
        }
    }
    for i in 0..files {
        fs::remove_file(name(i)).unwrap_or_else(|e| panic!("tmp_empty_files: remove file {i}: {e:?}"));
    }
    fs::remove_dir(DIR).expect("tmp_empty_files: the emptied directory removed");
    println!("tmp_empty_files: {files} empty files, one per MiB of memory and {MARGIN} more, made, read-opened and removed");
}
