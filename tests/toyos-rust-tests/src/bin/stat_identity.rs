//! A file's identity through std (`std::os::toyos::fs::MetadataExt`): on a
//! kernel mount (`/tmp`), on DATA (`/home`) and on FAT (`/log`), one file is
//! one `(dev, ino)` through every path, a symlink, its open handle, its
//! directory's listing and a rename; two files are two, as are two
//! directories; a symlink is one of its own, across a rename; a kernel
//! directory that ends gives its number up, and one that stands keeps it; and
//! the three are three volumes, each served one with
//! `toyos::fs::SERVED_VOLUME` set.

use std::fs::{self, File, Metadata};
use std::os::toyos::fs::{symlink, MetadataExt};

fn id(m: &Metadata) -> (u64, u64) {
    (m.dev(), m.ino())
}

fn of(path: &str) -> (u64, u64) {
    let found = id(&fs::metadata(path).unwrap_or_else(|e| panic!("stat {path}: {e}")));
    assert!(found.0 != 0 && found.1 != 0, "{path} has no identity: {found:?}");
    found
}

fn write(path: &str) {
    fs::write(path, b"x").unwrap_or_else(|e| panic!("write {path}: {e}"));
}

/// Judges `dir`; `spelling` is another name for `dir/a`. Returns its volume.
fn judge(dir: &str, spelling: &str, links: bool) -> u64 {
    let (a, b) = (format!("{dir}/stat-identity-a"), format!("{dir}/stat-identity-b"));
    write(&a);
    write(&b);
    let ida = of(&a);
    assert_ne!(ida, of(&b), "on {dir} two files are one");
    assert_eq!(of(spelling), ida, "on {dir} {spelling} is not {a}");

    let handle = File::open(&a).unwrap_or_else(|e| panic!("open {a}: {e}"));
    assert_eq!(id(&handle.metadata().unwrap()), ida, "on {dir} the open handle is another file");

    let listed = fs::read_dir(dir)
        .unwrap()
        .map(Result::unwrap)
        .find(|e| e.file_name() == "stat-identity-a")
        .unwrap_or_else(|| panic!("{a} is not in its listing"));
    assert_eq!(id(&listed.metadata().unwrap()), ida, "on {dir} the listing's entry is another file");

    if links {
        let link = format!("{dir}/stat-identity-link");
        symlink(&a, &link).unwrap_or_else(|e| panic!("symlink {link}: {e}"));
        assert_eq!(of(&link), ida, "on {dir} a symlink to {a} is another file");
        let own = id(&fs::symlink_metadata(&link).unwrap());
        assert!(own.0 == ida.0 && own.1 != 0 && own.1 != ida.1, "on {dir} the link itself is {own:?}");
        let renamed = format!("{dir}/stat-identity-link2");
        fs::rename(&link, &renamed).unwrap_or_else(|e| panic!("rename {link}: {e}"));
        assert_eq!(id(&fs::symlink_metadata(&renamed).unwrap()), own, "on {dir} a rename made another link");
        fs::remove_file(&renamed).unwrap();
    }

    let moved = format!("{dir}/stat-identity-moved");
    fs::rename(&a, &moved).unwrap_or_else(|e| panic!("rename {a}: {e}"));
    assert_eq!(of(&moved), ida, "on {dir} a rename made another file");
    assert_eq!(id(&handle.metadata().unwrap()), ida, "on {dir} the handle is another file after the rename");

    let (d1, d2) = (format!("{dir}/stat-identity-d1"), format!("{dir}/stat-identity-d2"));
    fs::create_dir(&d1).unwrap();
    fs::create_dir(&d2).unwrap();
    assert_ne!(of(&d1), of(&d2), "on {dir} two directories are one");
    for d in [&d1, &d2] {
        fs::remove_dir(d).unwrap();
    }
    for f in [&moved, &b] {
        fs::remove_file(f).unwrap();
    }
    println!("stat_identity: {dir} is volume {:#x}, and its files are told apart", ida.0);
    ida.0
}

/// On a kernel mount a directory that ends gives its number up, so the one
/// made there next is another: by `rmdir`, and by the unlink or the rename of
/// the file that alone made it one, but not by that file's truncating open.
fn an_ended_directory_is_forgotten() {
    const DIR: &str = "/tmp/stat-identity-ended";
    const FILE: &str = "/tmp/stat-identity-ended/f";
    const AWAY: &str = "/tmp/stat-identity-away";
    let again = |end: &str, make: &dyn Fn(), ends: &dyn Fn()| {
        make();
        let before = of(DIR);
        ends();
        make();
        assert_ne!(of(DIR), before, "{end} left {DIR}'s number to the directory made there next");
        ends();
    };
    let mkdir = || fs::create_dir(DIR).unwrap();
    let file = || write(FILE);
    again("rmdir", &mkdir, &|| fs::remove_dir(DIR).unwrap());
    again("an unlink", &file, &|| fs::remove_file(FILE).unwrap());
    again("a rename", &file, &|| {
        fs::rename(FILE, AWAY).unwrap();
        fs::remove_file(AWAY).unwrap();
    });

    // A truncating open puts its file back at once: the directory never ended.
    file();
    let kept = of(DIR);
    file();
    assert_eq!(of(DIR), kept, "a truncating open of its one file renumbered {DIR}");
    fs::remove_file(FILE).unwrap();
}

/// On a kernel mount a directory that still stands after a removal beneath it
/// keeps its number, as `/tmp` above it does: one a second file still makes a
/// directory, and one `mkdir` made whose only file goes.
fn a_standing_directory_keeps_its_number() {
    const DIR: &str = "/tmp/stat-identity-stands";
    const ONE: &str = "/tmp/stat-identity-stands/one";
    const TWO: &str = "/tmp/stat-identity-stands/two";
    let tmp = of("/tmp");

    write(ONE);
    write(TWO);
    let before = of(DIR);
    fs::remove_file(ONE).unwrap();
    assert_eq!(of(DIR), before, "an unlink beside a second file renumbered {DIR}");
    assert_eq!(of("/tmp"), tmp, "an unlink beneath it renumbered /tmp");
    fs::remove_file(TWO).unwrap();

    fs::create_dir(DIR).unwrap();
    write(ONE);
    let before = of(DIR);
    fs::remove_file(ONE).unwrap();
    assert_eq!(of(DIR), before, "an unlink of its only file renumbered the made {DIR}");
    fs::remove_dir(DIR).unwrap();
}

fn main() {
    an_ended_directory_is_forgotten();
    a_standing_directory_keeps_its_number();
    let tmp = judge("/tmp", "/tmp/../tmp/./stat-identity-a", true);
    let home = judge("/home", "/home/./stat-identity-a", true);
    // FAT names one entry by either case, and has no symlinks.
    let log = judge("/log", "/log/STAT-IDENTITY-A", false);

    assert_eq!(tmp & toyos::fs::SERVED_VOLUME, 0, "a kernel mount answered a served volume");
    for (dir, volume) in [("/home", home), ("/log", log)] {
        assert_ne!(volume & toyos::fs::SERVED_VOLUME, 0, "{dir}'s volume {volume:#x} lacks the served bit");
    }
    assert!(tmp != home && home != log && tmp != log, "two mounts are one volume");
    println!("stat_identity: ok");
}
