//! The reader against volumes upstream's tools wrote, and mkfs's volumes
//! against the reader and ToyOS's check.

use std::io::Read;
use std::vec::Vec;

use toyos_blockdev::{block_on_ready, BlockDev, Bytes, BLOCK};
use toyos_crypto::{scrypt::Limits, Random};

use super::bkey::Bpos;
use super::node::{self, Build};
use super::value::{Extent, InodeValue};
use super::*;

type Image = Bytes<Vec<u8>>;

fn fixture(name: &str) -> Image {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(&path).unwrap()).read_to_end(&mut raw).unwrap();
    Bytes::new(raw)
}

/// The passphrase the encrypted fixtures were formatted with; the tools
/// strip the newline its file ends in.
const PASSPHRASE: &[u8] = b"toyos-test-passphrase";
/// Upstream's default scrypt: N = 2^14, r = 8, p = 16.
const LIMITS: Limits = Limits { memory: 17 << 20, work: 1 << 21 };

/// `seq 1 40000`'s output.
fn seq() -> Vec<u8> {
    (1..=40000).flat_map(|n| format!("{n}\n").into_bytes()).collect()
}

fn path<D: BlockDev>(v: &Volume<D>, path: &str) -> u64 {
    block_on_ready(async {
        let mut ino = v.root();
        for name in path.split('/') {
            ino = v.lookup(ino, name.as_bytes()).await.unwrap().unwrap_or_else(|| panic!("{path}: no {name}")).ino;
        }
        ino
    })
}

fn names<D: BlockDev>(v: &Volume<D>, dir: u64) -> Vec<String> {
    let (entries, more) = block_on_ready(v.readdir(dir, 0, 100)).unwrap();
    assert!(!more);
    let mut names: Vec<String> = entries.into_iter().map(|e| String::from_utf8(e.name).unwrap()).collect();
    names.sort();
    names
}

fn read_all<D: BlockDev>(v: &Volume<D>, ino: u64) -> Result<Vec<u8>> {
    block_on_ready(async {
        let size = v.inode(ino).await?.size as usize;
        let mut out = vec![0u8; size];
        assert_eq!(v.read(ino, 0, &mut out).await?, size);
        Ok(out)
    })
}

#[test]
fn the_crc32c_fixture_reads_its_tree() {
    let dev = fixture("crc32c.img.gz");
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(v.root(), 4096);
    assert_eq!(names(&v, v.root()), ["Documents", "empty", "lost+found"]);
    assert_eq!(names(&v, path(&v, "Documents")), ["a.txt", "deep"]);
    assert_eq!(names(&v, path(&v, "Documents/deep")), ["link", "seq.txt"]);
    assert!(names(&v, path(&v, "empty")).is_empty());
    assert_eq!(read_all(&v, path(&v, "Documents/a.txt")).unwrap(), b"hello-toyos", "inline data");
    assert_eq!(block_on_ready(v.readlink(path(&v, "Documents/deep/link"))).unwrap(), b"../a.txt");
    let documents = block_on_ready(v.inode(path(&v, "Documents"))).unwrap();
    assert!(documents.is_dir());
    assert_eq!(documents.nlink, 3, "two names and one subdirectory");
    assert_eq!(block_on_ready(v.lookup(v.root(), b"absent")).unwrap(), None);
    // Its extents carry checksum type 5, whose seed and final XOR are not
    // established: refused by name, never read unverified.
    let seq_txt = path(&v, "Documents/deep/seq.txt");
    assert_eq!(block_on_ready(v.inode(seq_txt)).unwrap().size, seq().len() as u64);
    assert_eq!(read_all(&v, seq_txt), Err(Error::Unsupported("data checksum type")));
}

fn encrypted(name: &str) {
    let dev = fixture(name);
    assert!(matches!(block_on_ready(Volume::open(&dev)), Err(Error::Locked)));
    assert!(matches!(block_on_ready(Volume::unlock(&dev, b"toyos-test-passphrasf", LIMITS)), Err(Error::Passphrase)));
    let small = Limits { memory: 1 << 20, work: 1 << 21 };
    assert!(matches!(block_on_ready(Volume::unlock(&dev, PASSPHRASE, small)), Err(Error::Unsupported(_))), "scrypt past the caller's memory");
    let v = block_on_ready(Volume::unlock(&dev, PASSPHRASE, LIMITS)).unwrap();
    assert!(names(&v, v.root()).starts_with(&["a.txt".into()]));
    assert_eq!(read_all(&v, path(&v, "a.txt")).unwrap(), b"hello-toyos");
    assert_eq!(read_all(&v, path(&v, "seq.txt")).unwrap(), seq(), "every extent's MAC verified and decrypted");
    let mut middle = [0u8; 10];
    assert_eq!(block_on_ready(v.read(path(&v, "seq.txt"), 100_000, &mut middle)).unwrap(), 10);
    assert_eq!(middle, seq()[100_000..100_010]);
}

#[test]
fn the_80_bit_mac_fixture_unlocks_and_reads() {
    encrypted("encrypted.img.gz");
}

#[test]
fn the_128_bit_mac_fixture_unlocks_and_reads() {
    encrypted("encrypted-wide-macs.img.gz");
}

/// Flips a byte at `at` bytes into the device.
fn flip(dev: &Image, at: u64) {
    let mut block = [[0u8; BLOCK]];
    let lba = at / BLOCK as u64;
    block_on_ready(dev.read(lba, &mut block)).unwrap();
    block[0][(at % BLOCK as u64) as usize] ^= 0x40;
    block_on_ready(dev.write(lba, &block)).unwrap();
}

#[test]
fn a_flipped_byte_in_an_encrypted_extent_or_node_fails_its_mac() {
    for name in ["encrypted.img.gz", "encrypted-wide-macs.img.gz"] {
        let dev = fixture(name);
        let v = block_on_ready(Volume::unlock(&dev, PASSPHRASE, LIMITS)).unwrap();
        let ino = path(&v, "seq.txt");
        let mut first = None;
        block_on_ready(v.walk(Btree::Extents, Bpos::new(ino, 1, 0), |k, value| {
            first = Some(Extent::decode(value)?.ptr.sector);
            assert_eq!(k.kind, kind::EXTENT);
            Ok(false)
        }))
        .unwrap();
        flip(&dev, first.unwrap() * SECTOR as u64 + 100);
        assert_eq!(read_all(&v, ino), Err(Error::Checksum("extent")), "{name}");

        let inodes = v.roots().iter().find(|r| r.btree == Btree::Inodes).unwrap().ptr.ptr.sector;
        flip(&dev, inodes * SECTOR as u64 + 200);
        assert_eq!(block_on_ready(v.inode(4096)), Err(Error::Checksum("btree node")), "{name}");
    }
}

/// xorshift64*, for the tests' identifiers and keys.
struct Seeded(u64);

impl Random for Seeded {
    fn fill(&mut self, out: &mut [u8]) -> core::result::Result<(), toyos_crypto::Error> {
        for b in out {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            *b = (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 56) as u8;
        }
        Ok(())
    }
}

/// A volume secret as DATA's passphrase: 64 lowercase hex characters, here
/// of bytes 00–1f.
fn hex_secret() -> Vec<u8> {
    (0u8..32).flat_map(|b| format!("{b:02x}").into_bytes()).collect()
}

fn formatted(bytes: usize, encryption: Option<Encryption>) -> Image {
    let dev = Bytes::new(vec![0u8; bytes]);
    let options = Options { label: b"toyos", time: 1_761_177_600_000_000_000, encryption };
    block_on_ready(mkfs(&dev, &options, &mut Seeded(0x5eed))).unwrap();
    dev
}

fn empty_and_clean<D: BlockDev>(v: &Volume<D>) {
    assert_eq!(v.root(), 4096);
    assert!(names(v, v.root()).is_empty(), "no lost+found");
    let root = block_on_ready(v.inode(v.root())).unwrap();
    assert_eq!((root.mode, root.nlink, root.size), (0o40755, 2, 0));
    assert_eq!(root.mtime, 1_761_177_600_000_000_000, "the time base");
    assert_eq!(block_on_ready(fsck(v)).unwrap(), []);
}

#[test]
fn an_unencrypted_volume_reads_back_empty_and_checks_clean() {
    // 8 MiB: the fewest buckets, 512. 32 MiB: 16 KiB buckets and nodes, so
    // alloc spans three leaves under an interior root. 1 GiB: 512 KiB
    // buckets of two 256 KiB nodes, so a half-filled bucket takes a
    // fragmentation entry.
    for size in [8 << 20, 32 << 20, 1 << 30] {
        let dev = formatted(size, None);
        empty_and_clean(&block_on_ready(Volume::open(&dev)).unwrap());
    }
}

#[test]
fn an_encrypted_volume_unlocks_from_its_hex_secret_and_checks_clean() {
    let secret = hex_secret();
    let dev = formatted(64 << 20, Some(Encryption { passphrase: &secret, kdf: MIN_KDF }));
    assert!(matches!(block_on_ready(Volume::open(&dev)), Err(Error::Locked)));
    let tiny = Limits { memory: 1024, work: 2 };
    assert!(matches!(block_on_ready(Volume::unlock(&dev, b"0001", tiny)), Err(Error::Passphrase)));
    let v = block_on_ready(Volume::unlock(&dev, &secret, tiny)).unwrap();
    assert!(v.superblock().encrypted());
    empty_and_clean(&v);
}

#[test]
fn mkfs_is_a_function_of_its_random_source_and_time() {
    let a = formatted(32 << 20, None).into_inner();
    let b = formatted(32 << 20, None).into_inner();
    assert!(a == b);
}

#[test]
fn a_lost_primary_superblock_is_read_from_another_copy_and_found() {
    let dev = formatted(32 << 20, None);
    block_on_ready(dev.write(1, &[[0u8; BLOCK]])).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(names(&v, v.root()), Vec::<String>::new());
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), [Finding::Copy(8)]);
}

/// Rewrites the inodes root of an unencrypted volume, its bset stamped
/// `bset_seq` and the root directory `inode_seq`.
fn stamp(dev: &Image, bset_seq: u64, inode_seq: u64) {
    let v = block_on_ready(Volume::open(dev)).unwrap();
    let root = *v.roots().iter().find(|r| r.btree == Btree::Inodes).unwrap();
    let node = block_on_ready(v.node(Btree::Inodes, root.level, &root.ptr, root.key.pos)).unwrap();
    let keys: Vec<Vec<u8>> = node
        .keys
        .iter()
        .map(|(k, val)| {
            let mut inode = InodeValue::decode(val).unwrap();
            inode.journal_seq = inode_seq;
            k.encode(&inode.encode())
        })
        .collect();
    let build = Build { btree: Btree::Inodes, level: 0, seq: root.ptr.seq, min: Bpos::MIN, max: Bpos::MAX, journal_seq: bset_seq, keys: &keys };
    let bytes = node::encode(node::magic(&v.superblock().internal_uuid()), BLOCK, None, &build).unwrap();
    let mut blocks = vec![[0u8; BLOCK]; bytes.len() / BLOCK];
    blocks.as_flattened_mut().copy_from_slice(&bytes);
    block_on_ready(dev.write(root.ptr.ptr.sector / 8, &blocks)).unwrap();
}

#[test]
fn a_journal_sequence_stamped_past_clean_is_found() {
    let dev = formatted(32 << 20, None);
    stamp(&dev, 0, 0);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), [], "the rewrite alone changes nothing");
    let sector = v.roots().iter().find(|r| r.btree == Btree::Inodes).unwrap().ptr.ptr.sector;

    stamp(&dev, 1, 0);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), [Finding::BsetAboveClean { btree: Btree::Inodes, sector, seq: 1 }]);

    stamp(&dev, 0, 1);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), [Finding::InodeAboveClean { ino: 4096, seq: 1 }]);
}

#[test]
fn a_node_without_a_mac_on_an_encrypted_volume_is_refused() {
    let secret = hex_secret();
    let dev = formatted(32 << 20, Some(Encryption { passphrase: &secret, kdf: MIN_KDF }));
    let tiny = Limits { memory: 1024, work: 2 };
    let v = block_on_ready(Volume::unlock(&dev, &secret, tiny)).unwrap();
    let root = *v.roots().iter().find(|r| r.btree == Btree::Inodes).unwrap();
    let node = block_on_ready(v.node(Btree::Inodes, 0, &root.ptr, root.key.pos)).unwrap();
    let keys: Vec<Vec<u8>> = node.keys.iter().map(|(k, val)| k.encode(val)).collect();
    // The same keys, checksummed crc32c instead of MACed.
    let build = Build { btree: Btree::Inodes, level: 0, seq: root.ptr.seq, min: Bpos::MIN, max: Bpos::MAX, journal_seq: 0, keys: &keys };
    let bytes = node::encode(node::magic(&v.superblock().internal_uuid()), BLOCK, None, &build).unwrap();
    let mut blocks = vec![[0u8; BLOCK]; bytes.len() / BLOCK];
    blocks.as_flattened_mut().copy_from_slice(&bytes);
    block_on_ready(dev.write(root.ptr.ptr.sector / 8, &blocks)).unwrap();
    assert_eq!(block_on_ready(v.inode(4096)), Err(Error::Corrupt("unauthenticated btree node on an encrypted volume")));
}

#[test]
fn a_device_under_512_buckets_is_refused() {
    let dev = Bytes::new(vec![0u8; 4 << 20]);
    let options = Options { label: b"", time: 0, encryption: None };
    assert_eq!(block_on_ready(mkfs(&dev, &options, &mut Seeded(1))), Err(Error::Invalid("device smaller than 512 buckets")));
}

#[test]
fn a_scrypt_cost_past_any_limit_is_refused_at_mkfs() {
    let dev = Bytes::new(vec![0u8; 8 << 20]);
    let kdf = Kdf { log_n: 63, log_r: 31, log_p: 31 };
    let options = Options { label: b"", time: 0, encryption: Some(Encryption { passphrase: b"x", kdf }) };
    assert!(matches!(block_on_ready(mkfs(&dev, &options, &mut Seeded(1))), Err(Error::Unsupported(_))));
}
