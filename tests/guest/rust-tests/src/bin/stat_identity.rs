//! File and directory identities survive renames and held descriptions on
//! both the userland memory volume and writable DATA.

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
fn judge(dir: &str, spelling: &str) -> u64 {
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

    let link = format!("{dir}/stat-identity-link");
    symlink(&a, &link).unwrap_or_else(|e| panic!("symlink {link}: {e}"));
    assert_eq!(of(&link), ida, "on {dir} a symlink to {a} is another file");
    let own = id(&fs::symlink_metadata(&link).unwrap());
    assert!(own.0 == ida.0 && own.1 != 0 && own.1 != ida.1, "on {dir} the link itself is {own:?}");
    let renamed = format!("{dir}/stat-identity-link2");
    fs::rename(&link, &renamed).unwrap_or_else(|e| panic!("rename {link}: {e}"));
    assert_eq!(id(&fs::symlink_metadata(&renamed).unwrap()), own, "on {dir} a rename made another link");
    fs::remove_file(&renamed).unwrap();

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

/// Removing a child never removes or renumbers its directory. Removing and
/// recreating that directory must assign a new identity.
fn directory_lifetimes(root: &str) {
    let dir = format!("{root}/stat-identity-lifetime");
    let file = format!("{dir}/file");
    let away = format!("{root}/stat-identity-away");
    fs::create_dir(&dir).unwrap();
    let before = of(&dir);
    write(&file);
    fs::remove_file(&file).unwrap();
    assert_eq!(of(&dir), before, "unlink removed or renumbered its parent");
    write(&file);
    fs::rename(&file, &away).unwrap();
    assert_eq!(of(&dir), before, "rename removed or renumbered its parent");
    fs::remove_file(&away).unwrap();
    fs::remove_dir(&dir).unwrap();
    fs::create_dir(&dir).unwrap();
    assert_ne!(of(&dir), before, "a recreated directory reused a live identity");
    fs::remove_dir(&dir).unwrap();
}

fn directory_replacement(root: &str) {
    use toyos::fs::{Base, Dir, O_DIRECTORY, O_READ};
    let source = format!("{root}/stat-identity-source");
    let target = format!("{root}/stat-identity-target");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();
    write(&format!("{source}/file"));
    let mut directory = Dir::connect(toyos::endow::namespace().unwrap(), &format!("fs:{root}")).unwrap();
    let original = directory.open(Base::Root, "stat-identity-source", O_DIRECTORY | O_READ).unwrap().fid;
    let alias = directory.dup(original).unwrap();
    let replaced = directory.open(Base::Root, "stat-identity-target", O_DIRECTORY | O_READ).unwrap().fid;
    fs::rename(&source, &target).unwrap();
    let now = of(&format!("{target}/file"));
    for fid in [original, alias] {
        assert_eq!(directory.stat(Base::Fid(fid), "file", true).unwrap().node, now.1,
            "a directory alias lost its identity while its name moved");
    }
    assert!(directory.stat(Base::Fid(replaced), "file", true).is_err(),
        "a replaced directory description acquired the replacing directory's contents");
    for fid in [original, alias, replaced] { directory.close(fid); }
    fs::remove_file(format!("{target}/file")).unwrap();
    fs::remove_dir(&target).unwrap();
}

fn main() {
    for root in ["/tmp", "/home"] {
        directory_lifetimes(root);
        directory_replacement(root);
        let spelling = format!("{root}/../{}/./stat-identity-a", root.trim_start_matches('/'));
        let volume = judge(root, &spelling);
        assert_ne!(volume & toyos::fs::SERVED_VOLUME, 0, "a userland volume answered an unserved identity");
    }
    println!("stat_identity: ok");
}
