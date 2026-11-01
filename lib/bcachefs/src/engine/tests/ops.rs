//! The namespace operations, each read back through the reader after a
//! clean unmount and checked by fsck, and refused where §2.3 refuses.

use std::vec::Vec;

use toyos_blockdev::block_on_ready;

use super::mount::{mounted, NOW, OWNER};
use super::*;

fn names_of<D: BlockDev>(v: &Volume<D>, dir: u64) -> Vec<String> {
    names(v, dir)
}

fn nlink<D: BlockDev>(v: &Volume<D>, p: &str) -> u32 {
    block_on_ready(v.inode(path(v, p))).unwrap().nlink
}

fn checked(dev: &Image) -> Volume<&Image> {
    let v = block_on_ready(Volume::open(dev)).unwrap();
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
    v
}

#[test]
fn hard_links_count_their_names_and_the_last_unlink_leaves_an_orphan_its_release_frees() {
    let dev = formatted(64 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        let f = m.create(m.root(), b"f", New::File, OWNER, NOW).await.unwrap();
        let d = m.create(m.root(), b"d", New::Dir, OWNER, NOW).await.unwrap();
        assert_eq!(m.link(f.ino, d.ino, b"g", Link::New, NOW).await.unwrap(), None);
        assert_eq!(m.link(f.ino, m.root(), b"h", Link::New, NOW).await.unwrap(), None);
        assert_eq!(m.inode(f.ino).await.unwrap().nlink, 3);
        assert_eq!(m.link(d.ino, m.root(), b"d2", Link::New, NOW).await, Err(Error::IsDirectory));
        assert_eq!(m.link(f.ino, m.root(), b"f", Link::New, NOW).await, Err(Error::Exists));
        assert_eq!(m.unlink(m.root(), b"f", false, NOW).await, Ok(Removed { node: f.ino, last_link: false }));
        assert_eq!(m.unlink(m.root(), b"h", false, NOW).await, Ok(Removed { node: f.ino, last_link: false }));
        assert_eq!(m.unlink(d.ino, b"g", false, NOW).await, Ok(Removed { node: f.ino, last_link: true }));
        // Unnamed and unreleased: an orphan, which the unmount releases.
        assert_eq!(m.inode(f.ino).await.unwrap().nlink, 0);
        let kept = m.create(m.root(), b"kept", New::File, OWNER, NOW).await.unwrap();
        m.link(kept.ino, d.ino, b"kept2", Link::New, NOW).await.unwrap();
        m.unlink(m.root(), b"kept", false, NOW).await.unwrap();
        m.commit().await.unwrap();
    });
    let v = checked(&dev);
    assert_eq!(names_of(&v, v.root()), ["d"]);
    assert_eq!(names_of(&v, path(&v, "d")), ["kept2"]);
    assert_eq!(nlink(&v, "d/kept2"), 1);
}

#[test]
fn removing_a_directory_deletes_it_with_its_name_and_refuses_one_that_holds_names() {
    let dev = formatted(64 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        let a = m.create(m.root(), b"a", New::Dir, OWNER, NOW).await.unwrap();
        m.create(a.ino, b"inner", New::File, OWNER, NOW).await.unwrap();
        assert_eq!(m.unlink(m.root(), b"a", true, NOW).await, Err(Error::NotEmpty));
        assert_eq!(m.unlink(m.root(), b"a", false, NOW).await, Err(Error::IsDirectory));
        assert_eq!(m.unlink(a.ino, b"inner", true, NOW).await, Err(Error::NotDirectory));
        assert_eq!(m.unlink(m.root(), b"none", false, NOW).await, Err(Error::NotFound));
        m.unlink(a.ino, b"inner", false, NOW).await.unwrap();
        assert_eq!(m.unlink(m.root(), b"a", true, NOW).await, Ok(Removed { node: a.ino, last_link: true }));
        assert_eq!(m.inode(a.ino).await, Err(Error::NotFound), "deleted in its removing transaction");
        assert_eq!(m.inode(m.root()).await.unwrap().nlink, 2);
    });
    let v = checked(&dev);
    assert!(names_of(&v, v.root()).is_empty());
}

#[test]
fn renames_move_names_across_directories_replace_and_exchange() {
    let dev = formatted(64 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        let r = m.root();
        let a = m.create(r, b"a", New::Dir, OWNER, NOW).await.unwrap().ino;
        let b = m.create(a, b"b", New::Dir, OWNER, NOW).await.unwrap().ino;
        let f = m.create(r, b"f", New::File, OWNER, NOW).await.unwrap().ino;
        let g = m.create(r, b"g", New::File, OWNER, NOW).await.unwrap().ino;
        m.create(r, b"empty", New::Dir, OWNER, NOW).await.unwrap();
        // Within a directory, across directories, a directory across.
        assert_eq!(m.rename((r, b"f"), (r, b"f2"), Rename::NoReplace, NOW).await, Ok(None));
        assert_eq!(m.rename((r, b"f2"), (b, b"f3"), Rename::NoReplace, NOW).await, Ok(None));
        assert_eq!(m.rename((a, b"b"), (r, b"b"), Rename::NoReplace, NOW).await, Ok(None));
        // Never beneath itself.
        assert_eq!(m.rename((r, b"a"), (a, b"inside"), Rename::NoReplace, NOW).await, Err(Error::Invalid("a directory moved beneath itself")));
        m.rename((r, b"b"), (a, b"b"), Rename::NoReplace, NOW).await.unwrap();
        assert_eq!(m.rename((r, b"a"), (b, b"a"), Rename::NoReplace, NOW).await, Err(Error::Invalid("a directory moved beneath itself")));
        // Over a file, which loses the name; over an empty directory, which
        // goes; refused over what exists, or a file over a directory.
        assert_eq!(m.rename((r, b"g"), (b, b"f3"), Rename::NoReplace, NOW).await, Err(Error::Exists));
        assert_eq!(m.rename((r, b"g"), (b, b"f3"), Rename::Replace, NOW).await, Ok(Some(Removed { node: f, last_link: true })));
        assert_eq!(m.rename((b, b"f3"), (r, b"empty"), Rename::Replace, NOW).await, Err(Error::IsDirectory));
        let x = m.create(r, b"x", New::Dir, OWNER, NOW).await.unwrap().ino;
        let empty = m.lookup(r, b"empty").await.unwrap().unwrap().ino;
        assert_eq!(m.rename((r, b"x"), (r, b"empty"), Rename::Replace, NOW).await, Ok(Some(Removed { node: empty, last_link: true })));
        // A file and a directory swapped across directories.
        assert_eq!(m.rename((b, b"f3"), (r, b"empty"), Rename::Exchange, NOW).await, Ok(None));
        assert_eq!(m.lookup(b, b"f3").await.unwrap().unwrap().ino, x);
        assert_eq!(m.lookup(r, b"empty").await.unwrap().unwrap().ino, g);
        assert_eq!(m.rename((r, b"none"), (r, b"y"), Rename::Exchange, NOW).await, Err(Error::NotFound));
    });
    let v = checked(&dev);
    assert_eq!(names_of(&v, v.root()), ["a", "empty"]);
    assert_eq!(names_of(&v, path(&v, "a")), ["b"]);
    assert_eq!(names_of(&v, path(&v, "a/b")), ["f3"]);
    assert!(block_on_ready(v.inode(path(&v, "a/b/f3"))).unwrap().is_dir());
    assert_eq!(nlink(&v, "a/b"), 3, "the directory swapped in");
    assert_eq!(nlink(&v, "a"), 3);
    assert_eq!(block_on_ready(v.inode(v.root())).unwrap().nlink, 3);
}

/// Safe replacement (§2.3): a new file written unnamed takes the document's
/// name from the node the writer opened; a writer whose node no longer holds
/// it is answered `Stale`.
#[test]
fn an_unnamed_file_replaces_the_document_its_writer_opened_or_is_stale() {
    let dev = formatted(64 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        let r = m.root();
        let doc = m.create(r, b"doc", New::File, OWNER, NOW).await.unwrap().ino;
        let draft = m.create_unnamed(r, OWNER, NOW).await.unwrap().ino;
        assert_eq!(m.inode(draft).await.unwrap().nlink, 0);
        let other = m.create_unnamed(r, OWNER, NOW).await.unwrap().ino;
        assert_eq!(m.link(draft, r, b"doc", Link::Replace { expected: doc }, NOW).await, Ok(Some(Removed { node: doc, last_link: true })));
        assert_eq!(m.link(other, r, b"doc", Link::Replace { expected: doc }, NOW).await, Err(Error::Stale));
        assert_eq!(m.link(other, r, b"gone", Link::Replace { expected: draft }, NOW).await, Err(Error::Stale));
        m.release(doc).await.unwrap();
        assert_eq!(m.inode(doc).await, Err(Error::NotFound));
        assert_eq!(m.inode(draft).await.unwrap().nlink, 1);
    });
    let v = checked(&dev);
    assert_eq!(names_of(&v, v.root()), ["doc"]);
}

/// An orphan left by a crash, before its release: valid on the unclean
/// volume, and released after the next mount.
#[test]
fn an_orphan_a_crash_leaves_checks_valid_and_is_released_after_mounting() {
    let dev = formatted(64 << 20, None);
    let m = block_on_ready(Mount::open(&dev, super::mount::MEMORY, super::mount::random())).unwrap();
    let ino = block_on_ready(m.create_unnamed(m.root(), OWNER, NOW)).unwrap().ino;
    block_on_ready(m.commit()).unwrap();
    drop(m);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert!(!v.superblock().clean_flag());
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), [], "an orphan with its key, on an unclean volume");
    assert_eq!(block_on_ready(v.inode(ino)).unwrap().nlink, 0);
    drop(v);
    let m = block_on_ready(Mount::open(&dev, super::mount::MEMORY, super::mount::random())).unwrap();
    assert_eq!(block_on_ready(m.release_orphans()).unwrap(), 1);
    block_on_ready(m.unmount()).unwrap();
    let v = checked(&dev);
    assert_eq!(block_on_ready(v.inode(ino)), Err(Error::NotFound));
}

/// A clean unmount releases every orphan before it writes `clean`, so the
/// clean volume holds none (A8.14). Red when `clean` is written with one
/// pending: fsck finds its key and its unlinked inode on a clean volume.
#[test]
fn a_clean_unmount_releases_every_orphan_first() {
    let dev = formatted(64 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        m.create_unnamed(m.root(), OWNER, NOW).await.unwrap();
        let f = m.create(m.root(), b"f", New::File, OWNER, NOW).await.unwrap();
        m.unlink(m.root(), b"f", false, NOW).await.unwrap();
        assert_eq!(m.inode(f.ino).await.unwrap().nlink, 0, "an orphan, held");
    });
    let v = checked(&dev);
    assert!(v.superblock().clean_flag());
}

/// Two names in a directory hashing 31 bits wide to one slot: the second
/// takes the slot after. The first removed leaves a hash whiteout there, so
/// the second is still found; the first made again takes the whiteout's
/// slot; and the directory removed takes its whiteouts with it.
#[test]
fn a_removed_name_a_later_one_hashes_past_leaves_a_whiteout() {
    let dev = formatted(64 << 20, None);
    let dir = mounted(&dev, async |m: &Mount<&Image>| m.create(m.root(), b"d", New::Dir, OWNER, NOW).await.unwrap().ino);
    edit_inode(&dev, dir, |i| i.flags |= super::value::inode_flag::DIRENT_31_BIT);
    let inode = {
        let v = block_on_ready(Volume::open(&dev)).unwrap();
        let (_, value) = block_on_ready(v.get(Btree::Inodes, Bpos::new(0, dir, u32::MAX))).unwrap().unwrap();
        InodeValue::decode(&value).unwrap()
    };
    // Names until two share a 31-bit hash.
    let mut seen = std::collections::BTreeMap::new();
    let (first, second) = (0..).find_map(|i| {
        let name = format!("n{i}").into_bytes();
        let h = super::read::hash(&inode, &name).unwrap();
        seen.insert(h, name.clone()).map(|earlier| (earlier, name))
    }).unwrap();
    mounted(&dev, async |m: &Mount<&Image>| {
        m.create(dir, &first, New::File, OWNER, NOW).await.unwrap();
        let kept = m.create(dir, &second, New::File, OWNER, NOW).await.unwrap().ino;
        m.unlink(dir, &first, false, NOW).await.unwrap();
        assert_eq!(m.lookup(dir, &second).await.unwrap().map(|e| e.ino), Some(kept), "found past the whiteout");
        m.commit().await.unwrap();
    });
    let v = checked(&dev);
    let slot = super::read::hash(&inode, &first).unwrap();
    let (k, _) = block_on_ready(v.get(Btree::Dirents, Bpos::new(dir, slot, u32::MAX))).unwrap().unwrap();
    assert_eq!(k.kind, kind::HASH_WHITEOUT);
    drop(v);
    mounted(&dev, async |m: &Mount<&Image>| {
        let again = m.create(dir, &first, New::File, OWNER, NOW).await.unwrap().ino;
        let (k, value) = super::read::Source::get(m, Btree::Dirents, Bpos::new(dir, slot, u32::MAX)).await.unwrap().unwrap();
        assert_eq!((k.kind, super::value::Dirent::decode(&value).unwrap().target), (kind::DIRENT, again), "the whiteout's slot reused");
        m.unlink(dir, &first, false, NOW).await.unwrap();
        m.unlink(dir, &second, false, NOW).await.unwrap();
        m.unlink(m.root(), b"d", true, NOW).await.unwrap();
    });
    let v = checked(&dev);
    let mut left = 0;
    block_on_ready(super::read::Source::walk(&v, Btree::Dirents, Bpos::new(dir, 0, 0), |k, _| {
        left += usize::from(k.pos.inode == dir);
        Ok(k.pos.inode == dir)
    }))
    .unwrap();
    assert_eq!(left, 0, "the removed directory's whiteouts gone");
}

/// Counts a hostile volume holds at their edges are refused at the
/// operation, not wrapped: a link past what the reader can count, a
/// subdirectory removed from a directory counting none, a directory made
/// past the deepest depth; and back-references that loop end a rename's
/// ancestor walk.
#[test]
fn counts_at_their_edges_and_looping_back_references_are_refused() {
    use super::value::inode_field;
    let dev = formatted(64 << 20, None);
    let (f, d, a, b) = mounted(&dev, async |m: &Mount<&Image>| {
        let f = m.create(m.root(), b"f", New::File, OWNER, NOW).await.unwrap().ino;
        let d = m.create(m.root(), b"d", New::Dir, OWNER, NOW).await.unwrap().ino;
        m.create(d, b"s", New::Dir, OWNER, NOW).await.unwrap();
        let a = m.create(m.root(), b"a", New::Dir, OWNER, NOW).await.unwrap().ino;
        let b = m.create(m.root(), b"b", New::Dir, OWNER, NOW).await.unwrap().ino;
        m.create(m.root(), b"x", New::Dir, OWNER, NOW).await.unwrap();
        (f, d, a, b)
    });
    edit_inode(&dev, f, |i| i.set(inode_field::NLINK, u64::from(u32::MAX) - 1));
    edit_inode(&dev, d, |i| {
        i.set(inode_field::NLINK, 0);
        i.set(inode_field::DEPTH, u64::from(u32::MAX));
    });
    edit_inode(&dev, a, |i| i.set(inode_field::DIR, b));
    edit_inode(&dev, b, |i| i.set(inode_field::DIR, a));
    mounted(&dev, async |m: &Mount<&Image>| {
        assert_eq!(m.link(f, m.root(), b"g", Link::New, NOW).await, Err(Error::NoSpace("a link count at its largest")));
        assert_eq!(m.unlink(d, b"s", true, NOW).await, Err(Error::Corrupt("a directory counting fewer subdirectories than it holds")));
        assert_eq!(m.create(d, b"t", New::Dir, OWNER, NOW).await.err(), Some(Error::NoSpace("a directory depth at its largest")));
        assert_eq!(m.rename((m.root(), b"x"), (a, b"x"), Rename::NoReplace, NOW).await, Err(Error::Corrupt("directory back-references that loop")));
    });
}

/// A rename across a populated volume whose inodes and dirents fill many
/// leaves: the two files', both directories' inodes and both dirents each in
/// a leaf of its own, beside the replaced file's `deleted_inodes` key and the
/// accounting. The transaction changes no more nodes than its grant, which
/// the mount asserts at every apply.
#[test]
fn a_rename_across_many_leaves_dirties_no_more_nodes_than_its_grant() {
    let mut tree = Tree::new();
    let meta = Meta { perm: 0o755, uid: 0, gid: 0 };
    for d in 0..64 {
        let dir = tree.dir(Tree::ROOT, format!("d{d}").as_bytes(), meta).unwrap();
        for f in 0..100 {
            tree.file(dir, format!("f{f}").as_bytes(), b"", meta).unwrap();
        }
    }
    let dev = populated(64 << 20, &tree, 7);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let leaf = |p: &str| leaf_of(&v, Btree::Inodes, Bpos::new(0, path(&v, p), u32::MAX)).0.ptr.sector;
    // Two directories, a file in each, all four inodes in leaves apart.
    let mut found = None;
    'search: for i in 0..64 {
        for j in (i + 1)..64 {
            let (da, db) = (format!("d{i}"), format!("d{j}"));
            for f in 0..100 {
                let (fa, fb) = (format!("{da}/f{f}"), format!("{db}/f{f}"));
                let leaves: std::collections::BTreeSet<u64> = [leaf(&da), leaf(&db), leaf(&fa), leaf(&fb)].into();
                if leaves.len() == 4 {
                    found = Some((path(&v, &da), path(&v, &db), format!("f{f}")));
                    break 'search;
                }
            }
        }
    }
    let (a, b, name) = found.expect("four inodes in leaves apart");
    let dirent_leaves: std::collections::BTreeSet<u64> = [a, b].iter().map(|&d| leaf_of(&v, Btree::Dirents, Bpos::new(d, 0, u32::MAX)).0.ptr.sector).collect();
    assert_eq!(dirent_leaves.len(), 2, "the two directories' names in leaves apart");
    drop(v);
    let replaced = mounted(&dev, async |m: &Mount<&Image>| m.rename((a, name.as_bytes()), (b, name.as_bytes()), Rename::Replace, NOW).await.unwrap());
    assert!(replaced.is_some_and(|r| r.last_link));
    let v = checked(&dev);
    assert!(names_of(&v, b).contains(&name) && !names_of(&v, a).contains(&name));
}
