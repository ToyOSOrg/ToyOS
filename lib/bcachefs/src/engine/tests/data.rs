use super::mount::{mounted, random, MEMORY, NOW, OWNER};
use super::*;
use super::super::read::Source;

#[test]
fn file_data_overwrites_sparse_ranges_and_truncation_survive_remount() {
    let dev = formatted(64 << 20, None);
    let mut want = vec![0u8; 180_003];
    let ino = mounted(&dev, async |m: &Mount<&Image>| {
        let ino = m.create(m.root(), b"data", New::File, OWNER, NOW).await.unwrap().ino;
        let content: Vec<u8> = (0..90_005).map(|i| (i * 73) as u8).collect();
        assert_eq!(m.write(ino, 1007, &content, NOW).await.unwrap(), content.len());
        want[1007..1007 + content.len()].copy_from_slice(&content);
        assert_eq!(m.write(ino, 4003, b"replacement", NOW).await.unwrap(), 11);
        want[4003..4014].copy_from_slice(b"replacement");
        m.truncate(ino, want.len() as u64, NOW).await.unwrap();
        let mut got = vec![0u8; want.len()];
        assert_eq!(m.read(ino, 0, &mut got).await.unwrap(), want.len());
        assert_eq!(got, want);
        m.truncate(ino, 41_003, NOW).await.unwrap();
        m.truncate(ino, want.len() as u64, NOW).await.unwrap();
        want[41_003..].fill(0);
        m.write(ino, 41_000, b"new tail", NOW).await.unwrap();
        want[41_000..41_008].copy_from_slice(b"new tail");
        m.check_metadata_budget().await;
        m.commit().await.unwrap();
        ino
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(read_all(&v, ino).unwrap(), want);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn orphan_release_reclaims_file_data_and_keeps_other_files() {
    let dev = formatted(64 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        let a = m.create(m.root(), b"a", New::File, OWNER, NOW).await.unwrap().ino;
        let b = m.create(m.root(), b"b", New::File, OWNER, NOW).await.unwrap().ino;
        m.write(a, 0, &[1; 4096], NOW).await.unwrap();
        m.write(b, 0, &[2; 8192], NOW).await.unwrap();
        m.unlink(m.root(), b"a", false, NOW).await.unwrap();
        let mut bytes = [0u8; 4096];
        assert_eq!(m.read(a, 0, &mut bytes).await.unwrap(), bytes.len());
        assert_eq!(bytes, [1; 4096]);
        m.release(a).await.unwrap();
        assert_eq!(m.inode(a).await, Err(Error::NotFound));
        m.check_metadata_budget().await;
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(read_all(&v, path(&v, "b")).unwrap(), [2; 8192]);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn encrypted_data_uses_authenticated_extents_and_durable_version_ranges() {
    let dev = formatted(64 << 20, Some(Encryption { passphrase: b"test", kdf: MIN_KDF }));
    let m = block_on_ready(Mount::unlock(&dev, b"test", LIMITS, MEMORY, random())).unwrap();
    let (ino, old_version) = block_on_ready(async {
        let ino = m.create(m.root(), b"secret", New::File, OWNER, NOW).await.unwrap().ino;
        m.write(ino, 0, &[0x45; 8192], NOW).await.unwrap();
        m.write(ino, 100, b"changed", NOW).await.unwrap();
        let old_version = super::super::read::Source::get(&m, Btree::Extents, Bpos::new(ino, 8, u32::MAX)).await.unwrap().unwrap().0.version.lo;
        m.truncate(ino, 200, NOW).await.unwrap();
        (ino, old_version)
    });
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::unlock(&dev, b"test", LIMITS)).unwrap();
    let mut want = vec![0x45; 200];
    want[100..107].copy_from_slice(b"changed");
    assert_eq!(read_all(&v, ino).unwrap(), want);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
    assert!(old_version > 0 && old_version <= v.key_version());
    drop(v);
    let m = block_on_ready(Mount::unlock(&dev, b"test", LIMITS, MEMORY, random())).unwrap();
    block_on_ready(m.write(ino, 0, b"next", NOW)).unwrap();
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::unlock(&dev, b"test", LIMITS)).unwrap();
    let first = block_on_ready(v.get(Btree::Extents, Bpos::new(ino, 8, u32::MAX))).unwrap().unwrap();
    assert!(first.0.version.lo > old_version);
}

fn interrupted_data(encrypted: bool) {
    use super::crash::Watched;
    use core::cell::Cell;
    use toyos_blockdev::model::{Model, Overlay};

    let dev = formatted(64 << 20, encrypted.then_some(Encryption { passphrase: b"test", kdf: MIN_KDF }));
    let initial = if encrypted { block_on_ready(Mount::unlock(&dev, b"test", LIMITS, MEMORY, random())).unwrap() } else { block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap() };
    let ino = block_on_ready(initial.create(initial.root(), b"file", New::File, OWNER, NOW)).unwrap().ino;
    block_on_ready(initial.write(ino, 0, &[0x33; 8192], NOW)).unwrap();
    block_on_ready(initial.unmount()).unwrap();
    let mut states = vec![vec![0x33; 8192]];
    let mut next = states[0].clone();
    next[100..4196].fill(0x75);
    states.push(next.clone());
    next.truncate(1003);
    states.push(next.clone());
    next.resize(8192, 0);
    states.push(next);
    let applied = Cell::new(0);
    let committed = Cell::new(0);
    let requests = Cell::new(0u64);
    let model = Model::new(dev.into_inner(), 512);
    let watched = Watched {
        model: &model,
        after: Box::new(|model| {
            requests.set(requests.get() + 1);
            model
                .check(16, requests.get(), |bytes| {
                    let dev = Overlay::new(bytes);
                    let m = if encrypted { block_on_ready(Mount::unlock(&dev, b"test", LIMITS, MEMORY, random())) } else { block_on_ready(Mount::open(&dev, MEMORY, random())) }
                        .map_err(|e| format!("mount: {e:?}"))?;
                    let size = block_on_ready(m.inode(ino)).map_err(|e| format!("inode: {e:?}"))?.size;
                    let mut got = vec![0; size as usize];
                    block_on_ready(m.read(ino, 0, &mut got)).map_err(|e| format!("read: {e:?}"))?;
                    if !(committed.get()..=applied.get()).any(|i| states[i] == got) {
                        return Err(format!("unexpected bytes at request {} ({} bytes), applied {}, committed {}", requests.get(), got.len(), applied.get(), committed.get()));
                    }
                    block_on_ready(m.unmount()).map_err(|e| format!("unmount: {e:?}"))?;
                    let v = if encrypted { block_on_ready(Volume::unlock(&dev, b"test", LIMITS)) } else { block_on_ready(Volume::open(&dev)) }.map_err(|e| format!("reopen: {e:?}"))?;
                    let findings = block_on_ready(fsck(&v)).map_err(|e| format!("fsck: {e:?}"))?;
                    if !findings.is_empty() {
                        return Err(format!("fsck: {findings:?}"));
                    }
                    Ok(())
                })
                .unwrap_or_else(|e| panic!("{e}"));
        }),
    };
    let m = if encrypted { block_on_ready(Mount::unlock(&watched, b"test", LIMITS, MEMORY, random())).unwrap() } else { block_on_ready(Mount::open(&watched, MEMORY, random())).unwrap() };
    applied.set(1);
    block_on_ready(m.write(ino, 100, &[0x75; 4096], NOW)).unwrap();
    block_on_ready(m.commit()).unwrap();
    committed.set(1);
    applied.set(2);
    block_on_ready(m.truncate(ino, 1003, NOW)).unwrap();
    block_on_ready(m.commit()).unwrap();
    committed.set(2);
    applied.set(3);
    block_on_ready(m.truncate(ino, 8192, NOW)).unwrap();
    block_on_ready(m.commit()).unwrap();
    committed.set(3);
    block_on_ready(m.unmount()).unwrap();
    assert!(requests.get() > 10);
}

#[test]
fn data_and_logged_truncate_survive_torn_reordered_writes() {
    interrupted_data(false);
}

#[test]
fn encrypted_data_and_logged_truncate_survive_torn_reordered_writes() {
    interrupted_data(true);
}

#[test]
fn reserved_blocks_write_after_capacity_refusal_and_dropped_tokens_return_capacity() {
    let dev = formatted(64 << 20, None);
    let ino = mounted(&dev, async |m: &Mount<&Image>| {
        let ino = m.create(m.root(), b"reserved", New::File, OWNER, NOW).await.unwrap().ino;
        let mut tokens = Vec::new();
        loop {
            match m.reserve(ino, tokens.len() as u64 * BLOCK as u64, BLOCK).await {
                Ok(token) => tokens.push(token),
                Err(Error::NoSpace(_)) => break,
                Err(e) => panic!("reserve: {e:?}"),
            }
            assert!(tokens.len() < dev.blocks() as usize);
        }
        assert!(tokens.len() >= 64, "an ordinary 256 KiB buffered write fits a 64 MiB volume");
        let count = tokens.len();
        std::println!("64 MiB volume accepts {count} pending 4 KiB blocks before refusal");
        drop(tokens);
        let mut tokens = Vec::new();
        for i in 0..count {
            tokens.push(m.reserve(ino, i as u64 * BLOCK as u64, BLOCK).await.unwrap());
        }
        for (i, token) in tokens.iter_mut().enumerate().rev() {
            assert_eq!(m.write_reserved(token, i as u64 * BLOCK as u64, &[i as u8; BLOCK], NOW).await.unwrap(), BLOCK);
        }
        m.commit().await.unwrap();
        ino
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let bytes = read_all(&v, ino).unwrap();
    for (i, block) in bytes.as_chunks::<BLOCK>().0.iter().enumerate() {
        assert_eq!(block, &[i as u8; BLOCK]);
    }
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn a_reserved_slot_survives_its_bucket_emptying_and_generation_change() {
    let dev = formatted(64 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        let a = m.create(m.root(), b"a", New::File, OWNER, NOW).await.unwrap().ino;
        let b = m.create(m.root(), b"b", New::File, OWNER, NOW).await.unwrap().ino;
        let mut ta = m.reserve(a, 0, BLOCK).await.unwrap();
        let mut tb = m.reserve(b, 0, BLOCK).await.unwrap();
        m.write_reserved(&mut ta, 0, &[1; BLOCK], NOW).await.unwrap();
        m.unlink(m.root(), b"a", false, NOW).await.unwrap();
        m.release(a).await.unwrap();
        m.commit().await.unwrap();
        m.write_reserved(&mut tb, 0, &[2; BLOCK], NOW).await.unwrap();
        m.check_metadata_budget().await;
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(read_all(&v, path(&v, "b")).unwrap(), [2; BLOCK]);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn symlink_targets_are_file_data_and_read_after_remount() {
    let dev = formatted(64 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        let ino = m.create(m.root(), b"link", New::Symlink(b"../target".to_vec()), OWNER, NOW).await.unwrap().ino;
        assert_eq!(m.readlink(ino).await.unwrap(), b"../target");
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.readlink(path(&v, "link"))).unwrap(), b"../target");
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn partial_truncate_needs_no_new_data_block_at_capacity() {
    let dev = formatted(64 << 20, None);
    let ino = mounted(&dev, async |m: &Mount<&Image>| {
        let ino = m.create(m.root(), b"f", New::File, OWNER, NOW).await.unwrap().ino;
        m.write(ino, 0, &[0x77; 8192], NOW).await.unwrap();
        let mut tokens = Vec::new();
        loop {
            match m.reserve(ino, 0, BLOCK).await {
                Ok(token) => tokens.push(token),
                Err(Error::NoSpace(_)) => break,
                Err(e) => panic!("reserve: {e:?}"),
            }
        }
        m.truncate(ino, 700, NOW).await.unwrap();
        drop(tokens);
        m.truncate(ino, 8192, NOW).await.unwrap();
        ino
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let mut want = vec![0; 8192];
    want[..700].fill(0x77);
    assert_eq!(read_all(&v, ino).unwrap(), want);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn a_reservation_past_the_maximum_file_size_is_refused_without_overflow() {
    let dev = formatted(64 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        assert!(matches!(m.reserve(m.root(), u64::MAX - 1, 1).await, Err(Error::Invalid(_))));
    });
}

#[test]
fn a_failed_reserved_data_write_keeps_the_old_contents_and_reports_failure() {
    use core::cell::Cell;
    use toyos_blockdev::Block;
    struct Failing<'a> {
        dev: &'a Image,
        attempts: Cell<usize>,
    }
    impl BlockDev for Failing<'_> {
        fn blocks(&self) -> u64 {
            self.dev.blocks()
        }
        async fn read(&self, lba: u64, out: &mut [Block]) -> core::result::Result<(), toyos_blockdev::Error> {
            self.dev.read(lba, out).await
        }
        async fn write(&self, lba: u64, data: &[Block]) -> core::result::Result<(), toyos_blockdev::Error> {
            self.dev.write(lba, data).await
        }
        async fn flush(&self) -> core::result::Result<(), toyos_blockdev::Error> {
            self.dev.flush().await
        }
        async fn write_data(&self, _: u64, _: &[Block]) -> core::result::Result<(), toyos_blockdev::Error> {
            self.attempts.set(self.attempts.get() + 1);
            Err(toyos_blockdev::Error::Device)
        }
    }
    let dev = formatted(64 << 20, None);
    let ino = mounted(&dev, async |m: &Mount<&Image>| {
        let ino = m.create(m.root(), b"f", New::File, OWNER, NOW).await.unwrap().ino;
        m.write(ino, 0, &[0x22; BLOCK], NOW).await.unwrap();
        ino
    });
    let failing = Failing { dev: &dev, attempts: Cell::new(0) };
    let m = block_on_ready(Mount::open(&failing, MEMORY, random())).unwrap();
    let mut token = block_on_ready(m.reserve(ino, 0, BLOCK)).unwrap();
    assert_eq!(block_on_ready(m.write_reserved(&mut token, 0, &[0x44; BLOCK], NOW)), Err(Error::Device(toyos_blockdev::Error::Device)));
    assert_eq!(failing.attempts.get(), 3);
    assert!(matches!(block_on_ready(m.write_reserved(&mut token, 0, &[0x44; BLOCK], NOW)), Err(Error::ReadOnly(_))));
    assert!(matches!(block_on_ready(m.commit()), Err(Error::ReadOnly(_))));
    let mut got = [0; BLOCK];
    block_on_ready(m.read(ino, 0, &mut got)).unwrap();
    assert_eq!(got, [0x22; BLOCK]);
}

#[test]
fn a_large_imported_extent_is_sliced_without_rewriting_its_neighbors() {
    let original: Vec<u8> = (0..50_000).map(|i| (i * 11) as u8).collect();
    let mut tree = Tree::new();
    tree.file(Tree::ROOT, b"f", &original, Meta { perm: 0o644, uid: 0, gid: 0 }).unwrap();
    let dev = populated(64 << 20, &tree, 13);
    let ino = mounted(&dev, async |m: &Mount<&Image>| {
        let ino = m.lookup(m.root(), b"f").await.unwrap().unwrap().ino;
        let mut token = m.reserve(ino, 4500, 100).await.unwrap();
        m.write_reserved(&mut token, 4500, &[0x66; 100], NOW).await.unwrap();
        ino
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let mut want = original;
    want[4500..4600].fill(0x66);
    assert_eq!(read_all(&v, ino).unwrap(), want);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn small_and_large_geometries_reserve_an_ordinary_dirty_window() {
    use super::buckets::Sparse;
    for bytes in [64 << 20, 4u64 << 30] {
        let dev = Sparse::new(bytes);
        block_on_ready(mkfs(&dev, &Options { label: b"data", time: NOW as u64, encryption: None }, &Tree::new(), &mut Seeded(7))).unwrap();
        let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
        let ino = block_on_ready(m.create(m.root(), b"f", New::File, OWNER, NOW)).unwrap().ino;
        let before = m.space().1;
        let mut tokens = Vec::new();
        for i in 0..64 { tokens.push(block_on_ready(m.reserve(ino, i * BLOCK as u64, BLOCK)).unwrap()); }
        let reserved = before - m.space().1;
        std::println!("{} MiB: pending 256 KiB consumes {reserved} reported available bytes", bytes >> 20);
        assert!(reserved < 16 << 20);
        for (i, token) in tokens.iter_mut().enumerate() {
            block_on_ready(m.write_reserved(token, (i * BLOCK) as u64, &[i as u8; BLOCK], NOW)).unwrap();
        }
        block_on_ready(m.check_metadata_budget());
        block_on_ready(m.unmount()).unwrap();
        let v = block_on_ready(Volume::open(&dev)).unwrap();
        let got = read_all(&v, ino).unwrap();
        assert_eq!(got.len(), 64 * BLOCK);
        for (i, block) in got.as_chunks::<BLOCK>().0.iter().enumerate() { assert_eq!(block, &[i as u8; BLOCK]); }
        assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
    }
}

#[test]
fn metadata_bucket_evacuation_returns_sparse_bucket_capacity() {
    use super::buckets::Sparse;
    let dev = Sparse::new(4 << 30);
    block_on_ready(mkfs(&dev, &Options { label: b"gc", time: NOW as u64, encryption: None }, &Tree::new(), &mut Seeded(7))).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let bucket = u64::from(v.superblock().member.bucket_sectors);
    let mut roots = std::collections::BTreeMap::<u64, Vec<_>>::new();
    for root in v.roots() { roots.entry(root.ptr.ptr.sector / bucket).or_default().push(*root); }
    let (victim, sources) = roots.into_iter().max_by_key(|(_, roots)| roots.len()).unwrap();
    assert!(sources.len() >= 4);
    drop(v);
    let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    for root in sources.iter().take(sources.len() / 2) {
        assert!(block_on_ready(m.relocate_node(root.btree, root.level, Bpos::MAX, root.ptr.ptr.sector)).unwrap());
    }
    let pos = Bpos::new(0, victim, 0);
    let mut moved = 0;
    while block_on_ready(m.get(Btree::Alloc, pos)).unwrap().is_some_and(|(_, v)| value::AllocV4::decode(&v).unwrap().data_type == value::data_type::BTREE) {
        assert!(block_on_ready(m.compact_metadata()).unwrap(), "a sparse closed bucket has a reachable victim");
        moved += 1;
        assert!(moved < 64);
    }
    assert!(moved > 0);
    block_on_ready(m.check_metadata_budget());
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn a_full_data_volume_can_release_a_large_orphan_and_use_its_capacity_again() {
    let dev = formatted(64 << 20, None);
    let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    let ino = block_on_ready(m.create(m.root(), b"full", New::File, OWNER, NOW)).unwrap().ino;
    let start = m.space().1;
    let bytes = vec![0x62; 1 << 20];
    let mut written = 0;
    loop {
        match block_on_ready(m.write(ino, written, &bytes, NOW)) {
            Ok(n) => written += n as u64,
            Err(Error::NoSpace(_)) => break,
            Err(error) => panic!("fill: {error:?}"),
        }
        assert!(written <= 64 << 20);
    }
    assert!(written > 16 << 20);
    block_on_ready(m.commit()).unwrap();
    block_on_ready(m.unlink(m.root(), b"full", false, NOW)).unwrap();
    block_on_ready(m.release(ino)).unwrap();
    block_on_ready(m.commit()).unwrap();
    assert!(m.space().1 > start / 2);
    let ino = block_on_ready(m.create(m.root(), b"again", New::File, OWNER, NOW)).unwrap().ino;
    assert_eq!(block_on_ready(m.write(ino, 0, &bytes, NOW)).unwrap(), bytes.len());
    block_on_ready(m.check_metadata_budget());
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(read_all(&v, ino).unwrap(), bytes);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn retained_small_files_reclaim_their_truncated_data_buckets() {
    let dev = formatted(64 << 20, None);
    let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    let mut inodes = Vec::new();
    for i in 0..1600 {
        let ino = block_on_ready(m.create(m.root(), format!("f{i}").as_bytes(), New::File, OWNER, NOW)).unwrap().ino;
        let written = block_on_ready(m.write(ino, 0, &[i as u8; 32_768], NOW)).unwrap();
        assert_eq!(written, 32_768, "file {i}: {}", m.dump());
        block_on_ready(m.truncate(ino, 4096, NOW)).unwrap();
        inodes.push(ino);
        if i % 32 == 0 { block_on_ready(m.commit()).unwrap(); }
    }
    block_on_ready(m.check_metadata_budget());
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    for (i, ino) in inodes.into_iter().enumerate() { assert_eq!(read_all(&v, ino).unwrap(), [i as u8; 4096]); }
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn pending_allocation_failure_precedes_data_reservation_and_reserved_write_preparation() {
    let dev = formatted(64 << 20, None);
    let ino = mounted(&dev, async |m: &Mount<&Image>| {
        let ino = m.create(m.root(), b"retry", New::File, OWNER, NOW).await.unwrap().ino;
        assert_eq!(m.write(ino, 0, &[0x11; 4096], NOW).await.unwrap(), 4096);
        ino
    });
    // Remounted clean, so no reclaim runs before the preflights refused here.
    let mount = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    let before = mount.space();
    let out = super::super::pending::fail_pending_allocation_after(0, || block_on_ready(mount.reserve(ino, 0, 4096)));
    assert!(matches!(out, Err(Error::NoMemory)));
    assert_eq!(mount.space(), before, "a refused preflight takes no DATA slot");
    let mut reservation = block_on_ready(mount.reserve(ino, 0, 4096)).unwrap();
    block_on_ready(mount.set_mtime(ino, NOW + 1)).unwrap();
    // The change left nothing in pending, which keeps its workspace.
    mount.shrink_pending();
    let out = super::super::pending::fail_pending_allocation_after(0, || block_on_ready(mount.write_reserved(&mut reservation, 0, &[0x22; 4096], NOW)));
    assert_eq!(out, Err(Error::NoMemory));
    let mut old = [0; 4096];
    assert_eq!(block_on_ready(mount.read(ino, 0, &mut old)).unwrap(), old.len());
    assert_eq!(old, [0x11; 4096]);
    assert_eq!(block_on_ready(mount.write_reserved(&mut reservation, 0, &[0x22; 4096], NOW)).unwrap(), 4096);
    drop(reservation);
    block_on_ready(mount.check_metadata_budget());
    block_on_ready(mount.unmount()).unwrap();
    let volume = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(read_all(&volume, ino).unwrap(), [0x22; 4096]);
    assert_eq!(block_on_ready(fsck(&volume)).unwrap(), []);
}

fn interrupted_user_evacuation(encrypted: bool) {
    use core::cell::Cell;
    use toyos_blockdev::model::{Model, Overlay};
    use super::crash::Watched;
    let dev = formatted(64 << 20, encrypted.then_some(Encryption { passphrase: b"test", kdf: MIN_KDF }));
    let m = if encrypted { block_on_ready(Mount::unlock(&dev, b"test", LIMITS, MEMORY, random())).unwrap() } else { block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap() };
    let mut inodes = Vec::new();
    for i in 0..16 {
        let ino = block_on_ready(m.create(m.root(), format!("f{i}").as_bytes(), New::File, OWNER, NOW)).unwrap().ino;
        block_on_ready(m.write(ino, 0, &[i as u8; BLOCK], NOW)).unwrap();
        inodes.push(ino);
    }
    for &ino in &inodes { block_on_ready(m.truncate(ino, 512, NOW)).unwrap(); }
    block_on_ready(m.unmount()).unwrap();
    let model = Model::new(dev.into_inner(), 512);
    let requests = Cell::new(0u64);
    let watched = Watched { model: &model, after: Box::new(|model| {
        requests.set(requests.get() + 1);
        model.check(16, requests.get(), |bytes| {
            let dev = Overlay::new(bytes);
            let raw = if encrypted { block_on_ready(Volume::unlock(&dev, b"test", LIMITS)) } else { block_on_ready(Volume::open(&dev)) }.map_err(|e| format!("raw open: {e:?}"))?;
            let findings = block_on_ready(fsck(&raw)).map_err(|e| format!("raw fsck: {e:?}"))?;
            if findings.iter().any(|finding| !matches!(finding, Finding::Copy(_))) { return Err(format!("raw fsck: {findings:?}")); }
            drop(raw);
            let m = if encrypted { block_on_ready(Mount::unlock(&dev, b"test", LIMITS, MEMORY, random())) } else { block_on_ready(Mount::open(&dev, MEMORY, random())) }.map_err(|e| format!("open: {e:?}"))?;
            for (i, &ino) in inodes.iter().enumerate() {
                let mut got = [0; 512];
                let n = block_on_ready(m.read(ino, 0, &mut got)).map_err(|e| format!("read: {e:?}"))?;
                if n != got.len() || got != [i as u8; 512] { return Err(format!("changed file {i}")); }
            }
            block_on_ready(m.unmount()).map_err(|e| format!("recover: {e:?}"))?;
            let v = if encrypted { block_on_ready(Volume::unlock(&dev, b"test", LIMITS)) } else { block_on_ready(Volume::open(&dev)) }.map_err(|e| format!("open: {e:?}"))?;
            let findings = block_on_ready(fsck(&v)).map_err(|e| format!("fsck: {e:?}"))?;
            if !findings.is_empty() { return Err(format!("fsck: {findings:?}")); }
            Ok(())
        }).unwrap_or_else(|e| panic!("{e}"));
    }) };
    let m = if encrypted { block_on_ready(Mount::unlock(&watched, b"test", LIMITS, MEMORY, random())).unwrap() } else { block_on_ready(Mount::open(&watched, MEMORY, random())).unwrap() };
    assert!(block_on_ready(m.test_compact_user()).unwrap());
    assert!(block_on_ready(m.test_compact_user()).unwrap());
    block_on_ready(m.check_metadata_budget());
    block_on_ready(m.unmount()).unwrap();
    assert!(requests.get() > 10);
}

#[test]
fn packed_live_sectors_survive_torn_reordered_evacuation() {
    interrupted_user_evacuation(false);
}

#[test]
fn authenticated_live_sectors_survive_torn_reordered_evacuation() {
    interrupted_user_evacuation(true);
}

#[test]
fn evacuation_keeps_small_reads_within_one_physical_block() {
    use core::cell::Cell;
    use toyos_blockdev::Block;
    struct Reads<'a> { dev: &'a Image, blocks: Cell<usize> }
    impl BlockDev for Reads<'_> {
        fn blocks(&self) -> u64 { self.dev.blocks() }
        async fn read(&self, lba: u64, out: &mut [Block]) -> core::result::Result<(), toyos_blockdev::Error> { self.dev.read(lba, out).await }
        async fn write(&self, lba: u64, bytes: &[Block]) -> core::result::Result<(), toyos_blockdev::Error> { self.dev.write(lba, bytes).await }
        async fn flush(&self) -> core::result::Result<(), toyos_blockdev::Error> { self.dev.flush().await }
        async fn read_data(&self, lba: u64, out: &mut [Block]) -> core::result::Result<(), toyos_blockdev::Error> {
            self.blocks.set(self.blocks.get() + out.len());
            self.dev.read_data(lba, out).await
        }
    }
    for encrypted in [false, true] {
        let dev = formatted(64 << 20, encrypted.then_some(Encryption { passphrase: b"test", kdf: MIN_KDF }));
        let counted = Reads { dev: &dev, blocks: Cell::new(0) };
        let m = if encrypted { block_on_ready(Mount::unlock(&counted, b"test", LIMITS, MEMORY, random())).unwrap() } else { block_on_ready(Mount::open(&counted, MEMORY, random())).unwrap() };
        let mut inodes = Vec::new();
        for i in 0..8 {
            let ino = block_on_ready(m.create(m.root(), format!("f{i}").as_bytes(), New::File, OWNER, NOW)).unwrap().ino;
            block_on_ready(m.write(ino, 0, &[i as u8; BLOCK], NOW)).unwrap();
            inodes.push(ino);
        }
        for &ino in &inodes[1..] { block_on_ready(m.truncate(ino, 512, NOW)).unwrap(); }
        assert!(block_on_ready(m.test_compact_user()).unwrap());
        for (i, &ino) in inodes.iter().enumerate() {
            let mut got = vec![0; if i == 0 { BLOCK } else { 512 }];
            counted.blocks.set(0);
            assert_eq!(block_on_ready(m.read(ino, 0, &mut got)).unwrap(), got.len());
            assert!(got.iter().all(|&byte| byte == i as u8));
            assert_eq!(counted.blocks.get(), 1);
        }
        block_on_ready(m.unmount()).unwrap();
    }
}

/// Every live extent of `ino`: its start and length in bytes.
fn extents(v: &Volume<&Image>, ino: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    block_on_ready(Source::walk(v, Btree::Extents, Bpos::new(ino, 0, 0), |k, _| {
        if k.pos.inode != ino { return Ok(false); }
        if k.kind == kind::EXTENT {
            out.push(((k.pos.offset - u64::from(k.size)) * SECTOR as u64, u64::from(k.size) * SECTOR as u64));
        }
        Ok(true)
    }))
    .unwrap();
    out
}

/// A write is an extent for each 64 KiB run, reserved or not: 256 KiB is
/// four, not a block each.
#[test]
fn a_write_is_one_extent_for_each_run_of_contiguous_blocks() {
    let dev = formatted(256 << 20, None);
    let data: Vec<u8> = (0..256 << 10).map(|i| (i * 7) as u8).collect();
    let (plain, reserved) = mounted(&dev, async |m: &Mount<&Image>| {
        let plain = m.create(m.root(), b"plain", New::File, OWNER, NOW).await.unwrap().ino;
        assert_eq!(m.write(plain, 0, &data, NOW).await.unwrap(), data.len());
        let reserved = m.create(m.root(), b"reserved", New::File, OWNER, NOW).await.unwrap().ino;
        let mut token = m.reserve(reserved, 0, data.len()).await.unwrap();
        assert_eq!(m.write_reserved(&mut token, 0, &data, NOW).await.unwrap(), data.len());
        (plain, reserved)
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    for ino in [plain, reserved] {
        assert_eq!(read_all(&v, ino).unwrap(), data);
        assert_eq!(extents(&v, ino), [(0, 65536), (65536, 65536), (131072, 65536), (196608, 65536)]);
    }
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

/// A write over more old extents than one transaction may remove goes in
/// parts, each removing at most two, and the file reads as written.
#[test]
fn an_overwrite_across_many_old_extents_goes_in_parts() {
    let dev = formatted(256 << 20, None);
    let ino = mounted(&dev, async |m: &Mount<&Image>| {
        let ino = m.create(m.root(), b"f", New::File, OWNER, NOW).await.unwrap().ino;
        // Every block its own extent: each written alone, in reverse.
        for b in (0..16u64).rev() { m.write(ino, b * 4096, &[b as u8; 4096], NOW).await.unwrap(); }
        let mut token = m.reserve(ino, 0, 64 << 10).await.unwrap();
        assert_eq!(m.write_reserved(&mut token, 0, &[0xab; 64 << 10], NOW).await.unwrap(), 64 << 10);
        ino
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(read_all(&v, ino).unwrap(), [0xab; 64 << 10]);
    let parts = extents(&v, ino);
    assert!(parts.len() >= 8 && parts.iter().all(|&(_, len)| len >= 2 * 4096), "{parts:?}");
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

/// Blocks of a reservation never written return when it is dropped: a
/// reservation of 64 KiB written 16 KiB and dropped, 2000 times over one
/// file, fits a 64 MiB volume that could not hold the blocks never written.
#[test]
fn a_dropped_reservation_returns_its_unwritten_blocks() {
    let dev = formatted(64 << 20, None);
    mounted(&dev, async |m: &Mount<&Image>| {
        let ino = m.create(m.root(), b"f", New::File, OWNER, NOW).await.unwrap().ino;
        for i in 0..2000u32 {
            let mut token = m.reserve(ino, 0, 64 << 10).await.unwrap_or_else(|e| panic!("reservation {i}: {e:?}"));
            assert_eq!(m.write_reserved(&mut token, 0, &[i as u8; 16 << 10], NOW).await.unwrap(), 16 << 10);
            if i % 64 == 0 { m.commit().await.unwrap(); }
        }
    });
}
