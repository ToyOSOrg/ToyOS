use super::mount::{mounted, random, MEMORY, NOW, OWNER};
use super::*;

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
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(read_all(&v, path(&v, "b")).unwrap(), [2; 8192]);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn encrypted_data_uses_authenticated_extents_and_durable_version_ranges() {
    let dev = formatted(64 << 20, Some(Encryption { passphrase: b"test", kdf: MIN_KDF }));
    let m = block_on_ready(Mount::unlock(&dev, b"test", LIMITS, MEMORY, random())).unwrap();
    let ino = block_on_ready(async {
        let ino = m.create(m.root(), b"secret", New::File, OWNER, NOW).await.unwrap().ino;
        m.write(ino, 0, &[0x45; 8192], NOW).await.unwrap();
        m.write(ino, 100, b"changed", NOW).await.unwrap();
        m.truncate(ino, 200, NOW).await.unwrap();
        ino
    });
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::unlock(&dev, b"test", LIMITS)).unwrap();
    let mut want = vec![0x45; 200];
    want[100..107].copy_from_slice(b"changed");
    assert_eq!(read_all(&v, ino).unwrap(), want);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
    let first = block_on_ready(v.get(Btree::Extents, Bpos::new(ino, 8, u32::MAX))).unwrap().unwrap();
    let old_version = first.0.version.lo;
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
            assert!(tokens.len() < 1000);
        }
        assert!(tokens.len() > 1);
        let count = tokens.len();
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
    for (i, block) in bytes.chunks_exact(BLOCK).enumerate() {
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
