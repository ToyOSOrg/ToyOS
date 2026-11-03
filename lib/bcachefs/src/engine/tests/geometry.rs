//! Every geometry the writer accepts, down to a node of one block and a
//! bucket of one node, unmounts clean: its terminal publication writes
//! each changed node once, the keys those writes derive included, and
//! leaves nothing behind for upstream's check or ours.

use std::collections::BTreeMap;

use toyos_blockdev::block_on_ready;
use toyos_blockdev::executor::Executor;

use super::super::mkfs::{mkfs_shaped, Shape};
use super::mount::{random, MEMORY, NOW, OWNER};

/// The cheapest scrypt the fixtures here are formatted with.
const MIN_LIMITS: toyos_crypto::scrypt::Limits = toyos_crypto::scrypt::Limits { memory: 1 << 20, work: 1 << 20 };
use super::*;

/// A volume of `bytes` at `block` and `node` sectors, a bucket taking
/// mkfs's size for the device.
pub(super) fn shaped(bytes: usize, block: u64, node: u64, encryption: Option<Encryption>) -> Image {
    let dev = Bytes::new(vec![0u8; bytes]);
    let options = Options { label: b"toyos", time: 1_761_177_600_000_000_000, encryption };
    block_on_ready(mkfs_shaped(&dev, &options, &Tree::new(), &mut Seeded(0x5eed), Shape { block, node: Some(node) })).unwrap();
    dev
}

/// Device bytes, block and node sectors: a node of one block filling its
/// bucket, a node of one block two to a bucket, a node of two blocks, and
/// the native node of eight.
pub(super) const GEOMETRIES: [(usize, u64, u64); 4] = [(64 << 20, 64, 64), (128 << 20, 64, 64), (128 << 20, 64, 128), (128 << 20, 8, 64)];

/// The names under each directory and every file's bytes, as a workload
/// leaves them.
#[derive(Default)]
struct Expect {
    dirs: BTreeMap<u64, BTreeMap<Vec<u8>, u64>>,
    data: BTreeMap<u64, Vec<u8>>,
}

/// Creates in eight directories, a fifth of the files with data of up to
/// three blocks; then a third unlinked and released, renames across
/// directories, data overwritten and truncated. A commit every 64 changes.
async fn churn<D: BlockDev>(m: &Mount<D>, seed: u64, files: usize, expect: &mut Expect) {
    let block = super::super::read::Source::superblock(m).block_bytes();
    let mut rng = Seeded(seed);
    let mut dirs = Vec::new();
    for d in 0..8 {
        let name = format!("d{seed}-{d}").into_bytes();
        let ino = m.create(m.root(), &name, New::Dir, OWNER, NOW).await.unwrap().ino;
        expect.dirs.entry(m.root()).or_default().insert(name, ino);
        dirs.push(ino);
    }
    let mut made = Vec::new();
    for i in 0..files {
        let dir = dirs[below(&mut rng, dirs.len())];
        let name = format!("f{seed}-{i}-{}", "y".repeat(below(&mut rng, 120))).into_bytes();
        let ino = m.create(dir, &name, New::File, OWNER, NOW).await.unwrap().ino;
        expect.dirs.entry(dir).or_default().insert(name.clone(), ino);
        if i % 5 == 0 {
            let bytes: Vec<u8> = (0..block * (1 + below(&mut rng, 3)) - below(&mut rng, 100)).map(|j| (i * 31 + j) as u8).collect();
            m.write(ino, 0, &bytes, NOW).await.unwrap();
            expect.data.insert(ino, bytes);
        }
        made.push((dir, name, ino));
        if i % 64 == 63 {
            m.commit().await.unwrap();
        }
    }
    for (i, (dir, name, ino)) in made.iter().enumerate() {
        match i % 9 {
            0 | 3 | 6 => {
                let removed = m.unlink(*dir, name, false, NOW).await.unwrap();
                m.release(removed.node).await.unwrap();
                expect.dirs.get_mut(dir).unwrap().remove(name);
                expect.data.remove(ino);
            }
            1 => {
                let to = dirs[(i / 9) % dirs.len()];
                let mut new = name.clone();
                new.extend_from_slice(b"-moved");
                m.rename((*dir, name), (to, &new), Rename::Replace, NOW).await.unwrap();
                expect.dirs.get_mut(dir).unwrap().remove(name);
                expect.dirs.entry(to).or_default().insert(new, *ino);
            }
            5 if expect.data.contains_key(ino) => {
                m.truncate(*ino, (block / 2) as u64, NOW).await.unwrap();
                expect.data.get_mut(ino).unwrap().truncate(block / 2);
                m.write(*ino, 7, b"overwritten", NOW).await.unwrap();
                expect.data.get_mut(ino).unwrap()[7..18].copy_from_slice(b"overwritten");
            }
            _ => {}
        }
        if i % 64 == 63 {
            m.commit().await.unwrap();
        }
    }
    m.commit().await.unwrap();
}

/// The volume holds exactly what `expect` says, and checks clean.
fn holds_and_checks(dev: &Image, passphrase: Option<&[u8]>, expect: &Expect) {
    let v = match passphrase {
        Some(p) => block_on_ready(Volume::unlock(dev, p, MIN_LIMITS)).unwrap(),
        None => block_on_ready(Volume::open(dev)).unwrap(),
    };
    for (dir, want) in &expect.dirs {
        let (entries, more) = block_on_ready(v.readdir(*dir, 0, usize::MAX)).unwrap();
        assert!(!more);
        let got: BTreeMap<Vec<u8>, u64> = entries.into_iter().filter(|e| e.name != b"lost+found").map(|e| (e.name, e.ino)).collect();
        assert_eq!(&got, want, "directory {dir}");
    }
    for (ino, want) in &expect.data {
        assert_eq!(&read_all(&v, *ino).unwrap(), want, "file {ino}");
    }
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

fn open<'d>(dev: &'d Image, passphrase: Option<&[u8]>, options: MountOptions) -> Mount<&'d Image> {
    match passphrase {
        Some(p) => block_on_ready(Mount::unlock(dev, p, MIN_LIMITS, options, random())),
        None => block_on_ready(Mount::open(dev, options, random())),
    }
    .unwrap()
}

/// Two mounts of churn on every accepted geometry, each unmounted clean:
/// the second rewrites what the first one's publication wrote. Red with
/// unmount's old reclaim-only drain: on a node of one block every write
/// is a rewrite whose keys dirty another node, and the drain stops the
/// mount when it exhausts reclaim's journal share.
#[test]
fn every_accepted_geometry_unmounts_clean_after_churn() {
    for (bytes, block, node) in GEOMETRIES {
        let dev = shaped(bytes, block, node, None);
        let mut expect = Expect::default();
        for seed in [1, 2] {
            let m = open(&dev, None, MEMORY);
            Executor::new().block_on(churn(&m, seed, 600, &mut expect), || panic!("stalled; {}", m.dump()));
            block_on_ready(m.unmount()).unwrap_or_else(|e| panic!("block {block} node {node}: unmount {e:?}"));
            holds_and_checks(&dev, None, &expect);
        }
    }
}

/// The same on an encrypted volume of a node per block and bucket: every
/// new node and appended bset under a nonce of its own.
#[test]
fn an_encrypted_node_of_one_block_unmounts_clean() {
    use super::super::crypt::oracle;
    let secret = hex_secret();
    let dev = shaped(64 << 20, 64, 64, Some(Encryption { passphrase: &secret, kdf: MIN_KDF }));
    let mut expect = Expect::default();
    let m = open(&dev, Some(&secret), MEMORY);
    oracle::reset();
    Executor::new().block_on(async {
        for i in 0..400 {
            let name = format!("e{i}").into_bytes();
            let ino = m.create(m.root(), &name, New::File, OWNER, NOW).await.unwrap().ino;
            expect.dirs.entry(m.root()).or_default().insert(name, ino);
            if i % 50 == 49 { m.commit().await.unwrap(); }
        }
    }, || panic!("stalled; {}", m.dump()));
    block_on_ready(m.unmount()).unwrap();
    holds_and_checks(&dev, Some(&secret), &expect);
    assert!(oracle::nonces() > 20, "{} nonces recorded", oracle::nonces());
    assert_eq!(oracle::reused(), Vec::<[u32; 4]>::new(), "nonces used over two plaintexts");
}

/// Everything committed written by the terminal publication alone, on a
/// node of one block: an inode changed in each of hundreds of leaves, so
/// hundreds of members, whose new nodes' keys land together.
#[test]
fn one_publication_writes_hundreds_of_scattered_changes() {
    let mut tree = Tree::new();
    let meta = Meta { perm: 0o644, uid: 0, gid: 0 };
    let files = 20_000;
    for i in 0..files {
        tree.file(Tree::ROOT, format!("f{i:05}").as_bytes(), &[], meta).unwrap();
    }
    let dev = Bytes::new(vec![0u8; 512 << 20]);
    let options = Options { label: b"toyos", time: 1_761_177_600_000_000_000, encryption: None };
    block_on_ready(mkfs_shaped(&dev, &options, &tree, &mut Seeded(0x5eed), Shape { block: 64, node: Some(64) })).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let inodes: Vec<u64> = (0..files).step_by(97).map(|i| path(&v, &format!("f{i:05}"))).collect();
    drop(v);
    let m = open(&dev, None, MountOptions { cache_nodes: 4096 });
    Executor::new().block_on(async {
        for &ino in &inodes {
            m.set_mtime(ino, NOW + 5).await.unwrap();
        }
    }, || panic!("stalled; {}", m.dump()));
    let published = block_on_ready(m.unmount_in_one_publication()).unwrap();
    assert!(published.members > 100, "{published:?}");
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    for &ino in &inodes {
        assert_eq!(block_on_ready(v.inode(ino)).unwrap().mtime, NOW + 5);
    }
    assert_eq!(block_on_ready(v.names()).unwrap().len(), files);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

/// A publication whose new nodes take a sixth of a node, so what it
/// planned overfills: bounds added, the nodes they add and the keys those
/// derive joining, until it settles into a volume that still checks clean.
#[test]
fn a_publication_split_again_by_its_own_keys_settles_self_describing() {
    for (bytes, block, node) in GEOMETRIES {
        let dev = shaped(bytes, block, node, None);
        let mut expect = Expect::default();
        let m = open(&dev, None, MEMORY);
        Executor::new().block_on(churn(&m, 4, 300, &mut expect), || panic!("stalled; {}", m.dump()));
        m.limit_publication_groups(node as usize * SECTOR / 6);
        let published = block_on_ready(m.unmount_in_one_publication()).unwrap();
        assert!(published.regrouped > 0, "block {block} node {node}: {published:?}");
        holds_and_checks(&dev, None, &expect);
    }
}

/// A crash at every request of a mixed workload on a node of one block,
/// the unmount's publication and clean entry included: each state mounts,
/// holds a prefix of the names no shorter than committed, unmounts clean
/// and checks clean.
#[test]
fn a_node_of_one_block_survives_a_crash_at_every_request() {
    let image = shaped(64 << 20, 64, 64, None).into_inner();
    super::crash::ops_survive_a_crash_on(image, None, MEMORY, &super::crash::mixed(80), 6, 8);
}

#[test]
fn an_encrypted_node_of_one_block_survives_a_crash_at_every_request() {
    let secret = hex_secret();
    let image = shaped(64 << 20, 64, 64, Some(Encryption { passphrase: &secret, kdf: MIN_KDF })).into_inner();
    super::crash::ops_survive_a_crash_on(image, Some(&secret), MEMORY, &super::crash::mixed(70), 5, 8);
}
