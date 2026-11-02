//! Two real boots: write and sync through std, then verify after DATA remounts.
//! The host compares the random token from both boots, rejecting stale evidence.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::toyos::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

const ROOT: &str = "/home/toyos-data-roundtrip";
const LENGTH: usize = (256 << 10) + 123;
const MTIME: u64 = 1_700_000_000;

fn bytes(token: &[u8; 16], length: usize) -> Vec<u8> {
    let mut out: Vec<_> = (0..length)
        .map(|i| (i as u8).wrapping_mul(73) ^ token[i % token.len()])
        .collect();
    out[..16].copy_from_slice(token);
    out
}

fn file(root: &Path, name: &str, data: &[u8]) -> File {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(name))
        .expect("create a new round-trip file");
    file.write_all(data).expect("write");
    file.sync_all().expect("sync contents and metadata");
    file
}

fn report(phase: &str, token: &[u8; 16]) {
    let token: String = token.iter().map(|b| format!("{b:02x}")).collect();
    println!("DATA_ROUNDTRIP {phase} {token}");
}

fn write(root: &Path) {
    // Refuse a previous incomplete run rather than erasing its evidence.
    fs::create_dir(root)
        .expect("create round-trip directory (remove a previous failed run explicitly)");
    let mut token = [0; 16];
    getrandom::getrandom(&mut token).expect("random round-trip token");
    let data = file(root, "before-rename", &bytes(&token, 1 << 20));
    data.set_len(LENGTH as u64)
        .expect("truncate an allocated file");
    data.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(MTIME))
        .expect("set mtime");
    data.sync_all().expect("sync truncation and mtime");
    let ino = data.metadata().expect("metadata").ino();
    drop(data);
    fs::rename(root.join("before-rename"), root.join("payload")).expect("rename");
    drop(file(root, "identity", &ino.to_le_bytes()));
    drop(file(
        root,
        "removed",
        b"this name must not return after reboot",
    ));
    fs::remove_file(root.join("removed")).expect("unlink");
    fs::create_dir(root.join("empty")).expect("mkdir");
    File::open(root)
        .expect("open directory")
        .sync_all()
        .expect("sync directory");
    File::open("/home")
        .expect("open parent")
        .sync_all()
        .expect("sync parent");
    report("written", &token);
}

fn read(root: &Path) {
    let payload = fs::read(root.join("payload")).expect("read payload after reboot");
    assert_eq!(payload.len(), LENGTH, "the truncated length persisted");
    let token: [u8; 16] = payload[..16].try_into().unwrap();
    assert_eq!(payload, bytes(&token, LENGTH), "every byte persisted");
    let identity = fs::read(root.join("identity")).expect("read inode witness");
    let ino = u64::from_le_bytes(identity.try_into().expect("one inode number"));
    let meta = fs::metadata(root.join("payload")).expect("metadata after reboot");
    assert_eq!(
        meta.ino(),
        ino,
        "rename and remount preserve inode identity"
    );
    assert_eq!(meta.nlink(), 1);
    assert_eq!(
        meta.modified().unwrap(),
        SystemTime::UNIX_EPOCH + Duration::from_secs(MTIME)
    );
    for missing in ["before-rename", "removed"] {
        assert_eq!(
            fs::metadata(root.join(missing)).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }
    assert!(fs::metadata(root.join("empty")).unwrap().is_dir());
    let mut names: Vec<_> = fs::read_dir(root)
        .expect("readdir")
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["empty", "identity", "payload"]);
    fs::remove_dir_all(root).expect("remove verified fixture");
    File::open("/home")
        .expect("open parent")
        .sync_all()
        .expect("sync cleanup");
    report("verified", &token);
}

fn main() {
    let root = Path::new(ROOT);
    match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [phase] if phase == "write" => write(root),
        [phase] if phase == "read" => read(root),
        _ => panic!("expected data_roundtrip write or read"),
    }
}
