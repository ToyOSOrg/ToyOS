use super::mount::{mounted, random, MEMORY, NOW, OWNER};
use super::*;

#[test]
fn deleting_all_files_leaves_only_the_inode_spine_and_reuses_its_room() {
    let dev = formatted(64 << 20, None);
    for round in 0..3 {
        mounted(&dev, async |m: &Mount<&Image>| {
            let mut inodes = Vec::new();
            for i in 0..800 {
                inodes.push(m.create(m.root(), format!("f{i}").as_bytes(), New::File, OWNER, NOW).await.unwrap().ino);
            }
            m.commit().await.unwrap();
            for (i, ino) in inodes.into_iter().enumerate() {
                m.unlink(m.root(), format!("f{i}").as_bytes(), false, NOW).await.unwrap();
                m.release(ino).await.unwrap();
            }
        });
        let v = block_on_ready(Volume::open(&dev)).unwrap();
        let root = v.roots().iter().find(|r| r.btree == Btree::Inodes).unwrap();
        assert_eq!(root.level, 1, "round {round}");
        let node = block_on_ready(super::super::read::read_node(&dev, v.superblock(), None, Btree::Inodes, root.level, &root.ptr, Bpos::MAX)).unwrap();
        assert_eq!(node.keys.len(), 1, "all inode siblings merged in round {round}");
        assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
    }
}

#[test]
fn a_clean_node_relocation_keeps_its_keys_and_updates_its_pointer() {
    let dev = formatted(64 << 20, None);
    let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    let ino = block_on_ready(m.create(m.root(), b"dependency", New::File, OWNER, NOW)).unwrap().ino;
    block_on_ready(m.commit()).unwrap();
    let first = block_on_ready(m.inode(ino)).unwrap().journal_seq;
    block_on_ready(m.set_mtime(ino, NOW + 17)).unwrap();
    let latest = block_on_ready(m.inode(ino)).unwrap().journal_seq;
    assert!(latest > first);
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let root = *v.roots().iter().find(|r| r.btree == Btree::Inodes).unwrap();
    let stored = block_on_ready(super::super::read::read_node(&dev, v.superblock(), None, Btree::Inodes, root.level, &root.ptr, Bpos::MAX)).unwrap();
    assert!(stored.bset_seqs.iter().copied().max().unwrap() >= latest, "a replacement represents its latest journal record, not its first pin");
    drop(v);
    let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    assert!(block_on_ready(m.relocate_node(Btree::Inodes, root.level, Bpos::MAX, root.ptr.ptr.sector)).unwrap());
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let moved = v.roots().iter().find(|r| r.btree == Btree::Inodes).unwrap();
    assert_ne!(moved.ptr.ptr.sector, root.ptr.ptr.sector);
    let stored = block_on_ready(super::super::read::read_node(&dev, v.superblock(), None, Btree::Inodes, moved.level, &moved.ptr, Bpos::MAX)).unwrap();
    assert!(stored.bset_seqs.iter().copied().max().unwrap() >= latest, "a clean relocation preserves the existing dependency");
    assert_eq!(block_on_ready(v.inode(ino)).unwrap().mtime, NOW + 17);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn a_change_after_sibling_bytes_are_written_survives_their_pointer_update() {
    let mut tree = Tree::new();
    for i in 0..1200 { tree.file(Tree::ROOT, format!("f{i}").as_bytes(), &[], Meta { perm: 0o644, uid: 0, gid: 0 }).unwrap(); }
    let dev = populated(64 << 20, &tree, 1);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let inodes: Vec<_> = (0..1200).map(|i| path(&v, &format!("f{i}"))).collect();
    drop(v);
    let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    let mut changed = None;
    for (i, &ino) in inodes.iter().enumerate().take(1100) {
        block_on_ready(m.unlink(m.root(), format!("f{i}").as_bytes(), false, NOW)).unwrap();
        block_on_ready(m.release(ino)).unwrap();
        let plans = block_on_ready(m.write_batch(super::super::mount::Order::LeavesFirst)).unwrap();
        let target = inodes[i + 1..].iter().find(|&&ino| plans.iter().any(|p| p.merged_contains(&m, Btree::Inodes, 0, Bpos::new(0, ino, u32::MAX)))).copied();
        if let Some(ino) = target {
            block_on_ready(m.set_mtime(ino, NOW + 123)).unwrap();
            changed = Some(ino);
        }
        block_on_ready(m.point_batch(plans)).unwrap();
        if changed.is_some() { break; }
    }
    let ino = changed.expect("a deletion merged inode siblings");
    assert_eq!(block_on_ready(m.inode(ino)).unwrap().mtime, NOW + 123);
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.inode(ino)).unwrap().mtime, NOW + 123);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn sibling_merges_recover_after_torn_reordered_writes() {
    use core::cell::Cell;
    use toyos_blockdev::model::{Model, Overlay};
    use super::crash::Watched;

    const FILES: usize = 600;
    let mut tree = Tree::new();
    for i in 0..FILES { tree.file(Tree::ROOT, format!("f{i}").as_bytes(), &[], Meta { perm: 0o644, uid: 0, gid: 0 }).unwrap(); }
    let dev = populated(64 << 20, &tree, 3);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert!(v.roots().iter().find(|r| r.btree == Btree::Inodes).unwrap().level > 0);
    let inodes: Vec<_> = (0..FILES).map(|i| path(&v, &format!("f{i}"))).collect();
    drop(v);
    let model = Model::new(dev.into_inner(), 512);
    let applied = Cell::new(0);
    let committed = Cell::new(0);
    let judged = Cell::new(0u64);
    let watched = Watched {
        model: &model,
        after: Box::new(|model| {
            if applied.get() < FILES / 2 { return; }
            judged.set(judged.get() + 1);
            model.check(8, judged.get(), |bytes| {
                let dev = Overlay::new(bytes);
                let m = block_on_ready(Mount::open(&dev, MEMORY, random())).map_err(|e| format!("mount: {e:?}"))?;
                let (entries, more) = block_on_ready(m.readdir(m.root(), 0, FILES + 1)).map_err(|e| format!("list: {e:?}"))?;
                if more || !(FILES - applied.get()..=FILES - committed.get()).contains(&entries.len()) {
                    return Err(format!("{} names after {} applied/{} committed deletions", entries.len(), applied.get(), committed.get()));
                }
                block_on_ready(m.unmount()).map_err(|e| format!("unmount: {e:?}"))?;
                let v = block_on_ready(Volume::open(&dev)).map_err(|e| format!("open: {e:?}"))?;
                let found = block_on_ready(fsck(&v)).map_err(|e| format!("fsck: {e:?}"))?;
                if !found.is_empty() { return Err(format!("fsck: {found:?}")); }
                Ok(())
            }).unwrap_or_else(|e| panic!("{e}"));
        }),
    };
    let m = block_on_ready(Mount::open(&watched, MEMORY, random())).unwrap();
    for (i, ino) in inodes.into_iter().enumerate() {
        applied.set(i + 1);
        block_on_ready(m.unlink(m.root(), format!("f{i}").as_bytes(), false, NOW)).unwrap();
        block_on_ready(m.release(ino)).unwrap();
        if (i + 1) % 16 == 0 {
            block_on_ready(m.commit()).unwrap();
            committed.set(i + 1);
        }
    }
    block_on_ready(m.unmount()).unwrap();
    assert!(judged.get() > 10);
}
