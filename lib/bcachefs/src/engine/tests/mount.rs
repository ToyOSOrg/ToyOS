//! The read-write mount: transactions, the journal they write, reclaim and
//! a clean unmount, against the reader and ToyOS's check.

use std::boxed::Box;

use toyos_blockdev::executor::Executor;
use toyos_blockdev::block_on_ready;

use super::*;
use super::super::bkey::Key;

pub(super) const MEMORY: MountOptions = MountOptions { cache_nodes: 256 };
pub(super) const OWNER: Owner = Owner { perm: 0o644, uid: 0, gid: 0 };
/// 2025-10-23, nanoseconds: every operation's time.
pub(super) const NOW: i64 = 1_761_177_600_000_000_000;

pub(super) fn random() -> Box<dyn Random> {
    Box::new(Seeded(0x0b5e_ed05))
}

/// `f` against `dev` mounted read-write; a stall, every task waiting on what
/// none will do, panics. Then a clean unmount.
pub(super) fn mounted<T>(dev: &Image, f: impl AsyncFnOnce(&Mount<&Image>) -> T) -> T {
    let mount = block_on_ready(Mount::open(dev, MEMORY, random())).unwrap();
    let out = Executor::new().block_on(f(&mount), || panic!("every task of the mount stalled; {}", mount.dump()));
    block_on_ready(mount.unmount()).unwrap();
    out
}

#[test]
fn files_and_directories_created_read_back_after_a_clean_unmount_and_check_clean() {
    let dev = formatted(32 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        let d = m.create(m.root(), b"dir", New::Dir, OWNER, NOW).await.unwrap();
        m.create(d.ino, b"inner", New::File, OWNER, NOW).await.unwrap();
        for i in 0..50 {
            m.create(m.root(), format!("f{i}").as_bytes(), New::File, OWNER, NOW).await.unwrap();
        }
        assert_eq!(m.create(m.root(), b"dir", New::File, OWNER, NOW).await.err(), Some(Error::Exists));
        m.commit().await.unwrap();
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let mut got = names(&v, v.root());
    got.sort();
    let mut want: Vec<String> = (0..50).map(|i| format!("f{i}")).collect();
    want.push("dir".into());
    want.sort();
    assert_eq!(got, want);
    assert_eq!(names(&v, path(&v, "dir")), ["inner"]);
    assert_eq!(block_on_ready(v.inode(v.root())).unwrap().nlink, 3);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

/// Enough names that the inodes and dirents leaves split, and their roots
/// with them: the new nodes take free buckets, and every allocator and
/// accounting key the nodes change derives.
#[test]
fn leaves_and_roots_split_and_their_buckets_and_accounting_derive() {
    let dev = formatted(32 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        let mut dirs = alloc::vec![m.root()];
        for i in 0..1500 {
            let parent = dirs[i % dirs.len()];
            let new = if i % 10 == 0 { New::Dir } else { New::File };
            let made = m.create(parent, format!("n{i}").as_bytes(), new, OWNER, NOW).await.unwrap();
            if made.is_dir() {
                dirs.push(made.ino);
            }
            if i % 200 == 0 {
                m.commit().await.unwrap();
            }
        }
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.names()).unwrap().len(), 1500);
    for btree in [Btree::Inodes, Btree::Dirents] {
        assert!(v.roots().iter().any(|r| r.btree == btree && r.level > 0), "{btree:?} split");
    }
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

/// The upstream volume left mid-write (BB-A2-1), mounted read-write: its
/// window replayed, the journal moved on by the post-crash skip with the
/// skipped sequences blacklisted, every node it writes a fresh one, then
/// unmounted clean.
#[test]
fn the_unclean_fixture_mounts_read_write_past_the_skip_and_unmounts_clean() {
    let dev = fixture("unclean.img.gz");
    mounted(&dev, async |m: &Mount<&Image>| {
        for i in 0..20 {
            m.create(m.root(), format!("toyos{i}").as_bytes(), New::File, OWNER, NOW).await.unwrap();
        }
        m.commit().await.unwrap();
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let sb = v.superblock();
    assert!(sb.clean_flag());
    assert_eq!(sb.blacklist, [(63, 63 + 4096)], "F was 62, the newest entry found 62");
    assert!(sb.clean.as_ref().unwrap().journal_seq > 63 + 4096);
    let (entries, _) = block_on_ready(v.readdir(v.root(), 0, 1000)).unwrap();
    assert_eq!(entries.len(), 200 + 20 + 1, "the fixture's 200, these 20 and lost+found");
    for i in 1..=200 {
        assert_eq!(read_all(&v, path(&v, &format!("f{i}"))).unwrap(), format!("{i}\n").into_bytes(), "f{i}");
    }
    let found = block_on_ready(fsck(&v)).unwrap();
    assert_eq!(found, UPSTREAM_ONLY, "only what upstream itself writes otherwise");
}

/// What this writer does not handle in full is refused read-write by name
/// (§3.1): a superblock field of a type v1.39.4 does not define, and a
/// logged operation it cannot resume.
#[test]
fn a_volume_this_writer_cannot_keep_is_refused_read_write() {
    let open = |dev: &Image| block_on_ready(Mount::open(dev, MEMORY, random())).err();
    let dev = formatted(32 << 20, None);
    rewrite_superblock(&dev, |_, fields| fields.push(super::super::superblock::Field { kind: 18, body: vec![0; 8] }));
    assert_eq!(open(&dev), Some(Error::Unsupported("writing a volume with a superblock field ToyOS does not keep")));
    let dev = formatted(32 << 20, None);
    rewrite_leaf(&dev, Btree::LoggedOps, Bpos::MIN, 0, |keys| {
        let pos = Bpos::new(0, 0, 0);
        keys.push((Key::new(kind::LOGGED_OP_TRUNCATE, pos), vec![0; 24]));
    });
    assert_eq!(open(&dev), Some(Error::Unsupported("writing a volume with a logged operation to resume")));
    assert!(block_on_ready(Volume::open(&dev)).is_ok(), "still read");
}

/// A key of a type v1.39.4 does not define, met by a read while mounted, is
/// refused by name, and the mount stops writing (§3.1).
#[test]
fn a_key_of_an_unknown_type_met_while_mounted_stops_writing() {
    let dev = formatted(32 << 20, None);
    rewrite_leaf(&dev, Btree::Dirents, Bpos::MIN, 0, |keys| {
        keys.push((Key::new(kind::UNKNOWN, Bpos::new(mkfs::ROOT_INODE, 77, u32::MAX)), vec![0; 8]));
    });
    let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    assert_eq!(block_on_ready(m.readdir(m.root(), 0, 10)).err(), Some(Error::Unsupported("key type v1.39.4 does not define")));
    assert_eq!(block_on_ready(m.create(m.root(), b"f", New::File, OWNER, NOW)).err(), Some(Error::ReadOnly("key type v1.39.4 does not define")));
}
