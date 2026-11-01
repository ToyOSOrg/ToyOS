//! The reader against volumes upstream's tools wrote, and mkfs's volumes
//! against the reader and ToyOS's check.

use std::collections::BTreeMap;
use std::io::Read;
use std::vec::Vec;

use toyos_blockdev::{block_on_ready, BlockDev, Bytes, BLOCK};
use toyos_crypto::{scrypt::Limits, Random};

use super::bkey::Bpos;
use super::node::{self, Build};
use super::bkey::Key;
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
    // Fourteen 32-sector extents, each checksummed type 5.
    let seq_txt = path(&v, "Documents/deep/seq.txt");
    assert_eq!(read_all(&v, seq_txt).unwrap(), seq(), "every extent verified");
}

/// Type 5 is CRC-32C's register with no initial or final inversion: the
/// fixture's first extent stores 0x7080C241, which standard CRC-32C would
/// give as 0xE4E4C9C4, and its first two extents give 0x96C86455.
#[test]
fn the_fixtures_type_5_values_are_the_uninverted_crc32c() {
    let dev = fixture("crc32c.img.gz");
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let ino = path(&v, "Documents/deep/seq.txt");
    let mut first = None;
    block_on_ready(super::read::Source::walk(&v, Btree::Extents, Bpos::new(ino, 1, 0), |_, value| {
        first = Some(Extent::decode(value)?.crc.unwrap());
        Ok(false)
    }))
    .unwrap();
    let crc = first.unwrap();
    assert_eq!((crc.csum_type, crc.compressed), (5, 32));
    assert_eq!(crc.csum[..4], 0x7080_C241u32.to_le_bytes());
    let seq = seq();
    use crate::crc32c::{crc32c, crc32c_zero};
    assert_eq!(crc32c_zero(&seq[..16384]), 0x7080_C241);
    assert_eq!(crc32c(&seq[..16384]), 0xE4E4_C9C4);
    assert_eq!(crc32c_zero(&seq[..32768]), 0x96C8_6455);
    assert_eq!(crc32c_zero(&seq[..512]), 0xE5BA_59C6);
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
        block_on_ready(super::read::Source::walk(&v, Btree::Extents, Bpos::new(ino, 1, 0), |k, value| {
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
#[derive(Clone)]
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
    block_on_ready(mkfs(&dev, &options, &Tree::new(), &mut Seeded(0x5eed))).unwrap();
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

/// The leaf of `btree` whose range holds `at`: its pointer and max key.
fn leaf_of<D: BlockDev>(v: &Volume<D>, btree: Btree, at: Bpos) -> (super::value::NodePtr, Bpos) {
    let root = *v.roots().iter().find(|r| r.btree == btree).unwrap();
    let (mut level, mut ptr, mut max) = (root.level, root.ptr, root.key.pos);
    while level > 0 {
        let node = block_on_ready(v.node(btree, level, &ptr, max)).unwrap();
        let (k, val) = node.keys.iter().find(|(k, _)| k.pos >= at).unwrap();
        (ptr, max, level) = (super::value::NodePtr::decode(val).unwrap(), k.pos, level - 1);
    }
    (ptr, max)
}

/// Rewrites the leaf of `btree` holding `at` on an unencrypted volume with
/// its keys as `edit` leaves them, the bset stamped `bset_seq`. The keys
/// bypass the writer's type check, as a hostile image's would.
fn rewrite_leaf(dev: &Image, btree: Btree, at: Bpos, bset_seq: u64, edit: impl FnOnce(&mut Vec<(Key, Vec<u8>)>)) {
    let v = block_on_ready(Volume::open(dev)).unwrap();
    let (ptr, max) = leaf_of(&v, btree, at);
    let mut keys = block_on_ready(v.node(btree, 0, &ptr, max)).unwrap().keys;
    edit(&mut keys);
    keys.sort_by_key(|(k, _)| k.pos);
    let encoded: Vec<Vec<u8>> = keys.iter().map(|(k, val)| k.encode(val)).collect();
    // A placeholder bset, then the keys put in raw, so the writer's own
    // assertions never see them.
    let build = Build { btree, level: 0, seq: ptr.seq, min: ptr.min, max, journal_seq: bset_seq, keys: &[] };
    let mut bytes = node::encode(node::magic(&v.superblock().internal_uuid()), BLOCK, None, &build).unwrap();
    let flat = encoded.concat();
    let end = 160 + flat.len();
    bytes.resize(end.next_multiple_of(BLOCK), 0);
    bytes[160..end].copy_from_slice(&flat);
    bytes[158..160].copy_from_slice(&((flat.len() / 8) as u16).to_le_bytes());
    let crc = crate::crc32c::crc32c(&bytes[16..end]);
    bytes[..4].copy_from_slice(&crc.to_le_bytes());
    assert!(bytes.len() <= usize::from(ptr.written) * 512, "the rewrite outgrew the node's written sectors");
    // The written sectors past it, where an upstream node may hold later
    // bsets, each a later bset restating the first key: every record up to
    // the pointer's count is the node's.
    while bytes.len() < usize::from(ptr.written) * 512 {
        let at = bytes.len();
        let later = node::Later { btree, level: 0, seq: ptr.seq, offset: at, journal_seq: bset_seq, keys: &encoded[..1] };
        bytes.extend(node::encode_later(BLOCK, None, &later).unwrap());
    }
    let mut blocks = vec![[0u8; BLOCK]; bytes.len() / BLOCK];
    blocks.as_flattened_mut().copy_from_slice(&bytes);
    block_on_ready(dev.write(ptr.ptr.sector / 8, &blocks)).unwrap();
}

/// The inodes root stamped `bset_seq`, the root directory `inode_seq`.
fn stamp(dev: &Image, bset_seq: u64, inode_seq: u64) {
    rewrite_leaf(dev, Btree::Inodes, Bpos::MIN, bset_seq, |keys| {
        for (_, val) in keys.iter_mut() {
            let mut inode = InodeValue::decode(val).unwrap();
            inode.journal_seq = inode_seq;
            *val = inode.encode();
        }
    });
}

#[test]
fn a_journal_sequence_stamped_past_clean_is_found() {
    let dev = formatted(32 << 20, None);
    stamp(&dev, 0, 0);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), [], "the rewrite alone changes nothing");
    let sector = v.roots().iter().find(|r| r.btree == Btree::Inodes).unwrap().ptr.ptr.sector;

    // mkfs's volume is clean at its journal's one entry, sequence 1.
    stamp(&dev, 2, 0);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), [Finding::BsetPastJournal { btree: Btree::Inodes, sector, seq: 2 }]);

    stamp(&dev, 0, 2);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), [Finding::InodePastJournal { ino: 4096, seq: 2 }]);
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
    assert_eq!(block_on_ready(mkfs(&dev, &options, &Tree::new(), &mut Seeded(1))), Err(Error::Small { blocks: 2048 }));
}

#[test]
fn a_scrypt_cost_past_any_limit_is_refused_at_mkfs() {
    let dev = Bytes::new(vec![0u8; 8 << 20]);
    let kdf = Kdf { log_n: 63, log_r: 31, log_p: 31 };
    let options = Options { label: b"", time: 0, encryption: Some(Encryption { passphrase: b"x", kdf }) };
    assert!(matches!(block_on_ready(mkfs(&dev, &options, &Tree::new(), &mut Seeded(1))), Err(Error::Unsupported(_))));
}

/// The primary copy re-encoded after `edit`, written over every copy.
fn rewrite_superblock(dev: &Image, edit: impl FnOnce(&mut super::superblock::Header, &mut Vec<super::superblock::Field>)) {
    let mut block = [[0u8; BLOCK]];
    block_on_ready(dev.read(1, &mut block)).unwrap();
    let sb = super::superblock::Superblock::decode(&block[0], 8).unwrap();
    let (mut header, mut fields) = (sb.header.clone(), sb.fields.clone());
    edit(&mut header, &mut fields);
    for &offset in &sb.header.layout.offsets {
        let bytes = super::superblock::Superblock::encode(&header, &fields, offset);
        let mut blocks = vec![[0u8; BLOCK]; bytes.len().div_ceil(BLOCK)];
        blocks.as_flattened_mut()[..bytes.len()].copy_from_slice(&bytes);
        block_on_ready(dev.write(offset / 8, &blocks)).unwrap();
    }
}

fn open_err(dev: &Image) -> Error {
    match block_on_ready(Volume::open(dev)) {
        Ok(_) => panic!("opened"),
        Err(e) => e,
    }
}

#[test]
fn a_members_field_without_its_header_is_refused() {
    let dev = formatted(8 << 20, None);
    rewrite_superblock(&dev, |_, fields| fields.iter_mut().find(|f| f.kind == 11).unwrap().body.clear());
    assert_eq!(open_err(&dev), Error::Corrupt("members_v2 without its header"));
}

#[test]
fn journal_buckets_whose_counts_overflow_are_refused() {
    let dev = formatted(8 << 20, None);
    rewrite_superblock(&dev, |_, fields| {
        let body = [(1u64, 1u64 << 63), (2, 1 << 63)].iter().flat_map(|(a, b)| [a.to_le_bytes(), b.to_le_bytes()].concat()).collect();
        fields.iter_mut().find(|f| f.kind == 9).unwrap().body = body;
    });
    assert_eq!(open_err(&dev), Error::Corrupt("journal buckets"));
}

#[test]
fn a_superblock_copy_past_the_device_is_found_not_read() {
    let dev = formatted(8 << 20, None);
    rewrite_superblock(&dev, |header, _| header.layout.offsets[2] = 1 << 60);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert!(block_on_ready(fsck(&v)).unwrap().contains(&Finding::Copy(1 << 60)));
}

#[test]
fn a_link_count_past_32_bits_is_refused() {
    let dev = formatted(8 << 20, None);
    rewrite_leaf(&dev, Btree::Inodes, Bpos::MIN, 0, |keys| {
        let mut inode = InodeValue::decode(&keys[0].1).unwrap();
        inode.set(super::value::inode_field::NLINK, u64::from(u32::MAX));
        keys[0].1 = inode.encode();
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.inode(4096)), Err(Error::Corrupt("inode link count past 2^32")));
}

/// A dirent value: target, type, name.
fn dirent(target: u64, kind: u8, name: &[u8]) -> Vec<u8> {
    [&target.to_le_bytes()[..], &[kind], name].concat()
}

#[test]
fn a_dirent_at_the_last_offset_is_refused() {
    let dev = formatted(8 << 20, None);
    rewrite_leaf(&dev, Btree::Dirents, Bpos::MIN, 0, |keys| keys.push((Key::new(kind::DIRENT, Bpos::new(4096, u64::MAX, u32::MAX)), dirent(4096 + 1, 8, b"x"))));
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.readdir(4096, 0, 10)).err(), Some(Error::Corrupt("dirent at the last offset")));
}

#[test]
fn a_symlink_past_4_kib_is_refused_before_it_is_allocated() {
    let dev = formatted(8 << 20, None);
    rewrite_leaf(&dev, Btree::Inodes, Bpos::MIN, 0, |keys| {
        let mut link = InodeValue::decode(&keys[0].1).unwrap();
        link.flags = 0o120777 << 36;
        link.size = 1 << 40;
        keys.push((Key::new(kind::INODE_V3, Bpos::new(0, 1 << 31, u32::MAX)), link.encode()));
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.readlink(1 << 31)), Err(Error::Unsupported("symlink target past 4 KiB")));
}

#[test]
fn a_snapshotted_root_subvolume_is_refused() {
    let dev = formatted(8 << 20, None);
    rewrite_leaf(&dev, Btree::Subvolumes, Bpos::MIN, 0, |keys| keys[0].1 = super::value::subvolume(u32::MAX - 1, 4096).to_vec());
    assert_eq!(open_err(&dev), Error::Unsupported("a root subvolume that has been snapshotted"));
}

#[test]
fn a_strict_key_in_another_btrees_leaf_is_refused_and_a_set_key_is_found() {
    let dev = formatted(8 << 20, None);
    rewrite_leaf(&dev, Btree::Inodes, Bpos::MIN, 0, |keys| keys.push((Key::new(kind::ALLOC_V4, Bpos::new(0, 5000, u32::MAX)), vec![0; 64])));
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.inode(4096)).err(), Some(Error::Corrupt("key of a type its btree does not hold")));

    // A set key is non-strict: upstream reads it anywhere, and refuses only
    // its commit. The reader passes it; ToyOS's check finds it.
    let dev = formatted(8 << 20, None);
    let pos = Bpos::new(0, 5000, u32::MAX);
    rewrite_leaf(&dev, Btree::Inodes, Bpos::MIN, 0, |keys| keys.push((Key::new(kind::SET, pos), vec![])));
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert!(block_on_ready(v.inode(4096)).is_ok());
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), [Finding::KeyType { btree: Btree::Inodes, level: 0, kind: kind::SET, pos }]);
}

#[test]
#[should_panic(expected = "a key of type 27 in a Inodes node")]
fn the_writer_refuses_a_key_its_btree_does_not_hold() {
    let key = Key::new(kind::ALLOC_V4, Bpos::new(0, 1, 0)).encode(&[0; 64]);
    let build = Build { btree: Btree::Inodes, level: 0, seq: 1, min: Bpos::MIN, max: Bpos::MAX, journal_seq: 0, keys: &[key] };
    let _ = node::encode(0, BLOCK, None, &build);
}

#[test]
fn a_passphrase_upstream_would_cut_or_strip_is_refused_at_mkfs() {
    for passphrase in [&b"secret\n"[..], b"sec\0ret"] {
        let dev = Bytes::new(vec![0u8; 8 << 20]);
        let options = Options { label: b"", time: 0, encryption: Some(Encryption { passphrase, kdf: MIN_KDF }) };
        assert_eq!(block_on_ready(mkfs(&dev, &options, &Tree::new(), &mut Seeded(1))), Err(Error::Invalid("passphrase holding NUL or ending in a newline")));
    }
}

/// What the reader and the check do over a volume, bounded so a mutated
/// tree's loops and sizes end: open, check and list the root, then with
/// `deep` every directory to depth 8, every file's first 64 KiB and every
/// symlink.
fn exercise(dev: &Image, unlock: Option<&[u8]>, deep: bool) {
    let opened = match unlock {
        Some(passphrase) => block_on_ready(Volume::unlock(dev, passphrase, Limits { memory: 17 << 20, work: 1 << 21 })),
        None => block_on_ready(Volume::open(dev)),
    };
    let Ok(v) = opened else { return };
    let _ = block_on_ready(fsck(&v));
    let mut pending = vec![(v.root(), 0)];
    let mut seen = 0;
    while let Some((dir, depth)) = pending.pop() {
        let Ok((entries, _)) = block_on_ready(v.readdir(dir, 0, 64)) else { continue };
        for e in entries {
            seen += 1;
            if seen > 256 {
                return;
            }
            let _ = block_on_ready(v.lookup(dir, &e.name));
            if !deep {
                continue;
            }
            match e.kind {
                DirentType::Directory if depth < 8 => pending.push((e.ino, depth + 1)),
                DirentType::Symlink => drop(block_on_ready(v.readlink(e.ino))),
                _ => drop(block_on_ready(v.read(e.ino, 0, &mut [0u8; 65536]))),
            }
        }
    }
}

/// Every node's place and written length.
fn nodes_of(dev: &Image, unlock: Option<&[u8]>) -> Vec<(usize, usize)> {
    let v = match unlock {
        Some(p) => block_on_ready(Volume::unlock(dev, p, LIMITS)).unwrap(),
        None => block_on_ready(Volume::open(dev)).unwrap(),
    };
    let mut out = Vec::new();
    for root in v.roots() {
        let mut pending = vec![(root.level, root.ptr, root.key.pos)];
        while let Some((level, ptr, max)) = pending.pop() {
            out.push((ptr.ptr.sector as usize * 512, usize::from(ptr.written) * 512));
            let node = block_on_ready(v.node(root.btree, level, &ptr, max)).unwrap();
            if level > 0 {
                for (k, val) in &node.keys {
                    pending.push((level - 1, super::value::NodePtr::decode(val).unwrap(), k.pos));
                }
            }
        }
    }
    out
}

fn le(b: &[u8], at: usize, n: usize) -> usize {
    b[at..at + n].iter().rev().fold(0usize, |a, &x| a << 8 | usize::from(x))
}

/// The crc32c of a superblock copy at `at`, recomputed so a mutation
/// reaches the decoders behind it.
fn reseal_superblock(b: &mut [u8], at: usize, span: usize) {
    let total = 752 + 8 * le(b, at + 124, 4);
    if b[at + 144] >> 2 & 0x3f == 1 && total <= span {
        let crc = crate::crc32c::crc32c(&b[at + 16..at + total]);
        b[at..at + 4].copy_from_slice(&crc.to_le_bytes());
    }
}

/// The same for a node's first record, where it is checksummed crc32c.
fn reseal_node(b: &mut [u8], at: usize, len: usize) {
    let end = 160 + 8 * le(b, at + 158, 2);
    if b[at + 152] & 0xf == 1 && end <= len {
        let crc = crate::crc32c::crc32c(&b[at + 16..at + end]);
        b[at..at + 4].copy_from_slice(&crc.to_le_bytes());
    }
}

/// Writes `bytes`' blocks `[at, at + len)`.
fn put(dev: &Image, bytes: &[u8], at: usize, len: usize) {
    let (first, last) = (at / BLOCK, (at + len).div_ceil(BLOCK));
    let blocks: Vec<[u8; BLOCK]> = bytes[first * BLOCK..last * BLOCK].as_chunks::<BLOCK>().0.to_vec();
    block_on_ready(dev.write(first as u64, &blocks)).unwrap();
}

const BOUNDARY: [u64; 6] = [0, 1, u64::MAX, u64::MAX - 1, 1 << 63, u32::MAX as u64];

/// Structured mutations of upstream's and mkfs's volumes, checksums
/// resealed so each reaches what lies behind them. Every aligned word of the
/// superblock is set to each boundary value in every copy at once, so copy
/// selection cannot step around it; nodes take seeded random bytes and
/// boundary words. Whatever the reader and the check answer, neither panics.
#[test]
fn mutated_volumes_are_refused_without_a_panic() {
    let secret = hex_secret();
    // The encrypted fixtures' scrypt is upstream's default, far too costly
    // per round; mkfs's encrypted volume takes the same paths at N = 2.
    // A populated volume's and the links fixture's inodes, dirents and
    // extents span interior nodes, beside every derived btree B3 writes.
    // Their superblocks hold nothing the others' do not, so only their
    // nodes are mutated.
    let data = contents(7);
    let tree = generate(&data, 12, 200).tree;
    let images: [(Image, Option<&[u8]>, bool, usize); 5] = [
        (fixture("crc32c.img.gz"), None, true, 1500),
        (fixture("links.img.gz"), None, false, 400),
        (formatted(8 << 20, None), None, true, 1500),
        (populated(8 << 20, &tree, 12), None, false, 400),
        (formatted(8 << 20, Some(Encryption { passphrase: &secret, kdf: MIN_KDF })), Some(&secret), true, 300),
    ];
    let mut rng = Seeded(0xf022);
    let mut draw = |n: usize| {
        let mut b = [0u8; 8];
        rng.fill(&mut b).unwrap();
        (u64::from_le_bytes(b) % n as u64) as usize
    };
    for (dev, unlock, sweep, rounds) in images {
        let nodes = nodes_of(&dev, unlock);
        let pristine = dev.get().clone();
        let used = 752 + 8 * le(&pristine, 4096 + 124, 4);
        let span = used.next_multiple_of(BLOCK);
        let primary = super::superblock::Superblock::decode(&pristine[4096..4096 + span], 8).unwrap();
        let copies: Vec<usize> = primary.header.layout.offsets.iter().map(|&o| o as usize * 512).collect();
        // The header and each field's first 128 bytes: the rest of a large
        // field is records the sweep's offsets already exercise.
        let mut offsets: Vec<usize> = (0..752).collect();
        let mut field = 752;
        while field + 8 <= used {
            let len = (8 * le(&pristine, 4096 + field, 4)).max(8);
            offsets.extend(field..(field + len.min(128)).min(used));
            field += len;
        }
        for width in [4, 8].into_iter().filter(|_| sweep) {
            for at in offsets.iter().copied().filter(|at| at % width == 0) {
                for value in BOUNDARY {
                    let mut bytes = pristine.clone();
                    for &copy in &copies {
                        bytes[copy + at..copy + at + width].copy_from_slice(&value.to_le_bytes()[..width]);
                        reseal_superblock(&mut bytes, copy, span);
                        put(&dev, &bytes, copy, span);
                    }
                    exercise(&dev, unlock, false);
                    for &copy in &copies {
                        put(&dev, &pristine, copy, span);
                    }
                }
            }
        }
        for _ in 0..rounds {
            let (at, len) = nodes[draw(nodes.len())];
            let mut bytes = pristine[at..at + len].to_vec();
            for _ in 0..1 + draw(3) {
                if draw(2) == 0 {
                    bytes[draw(len)] = draw(256) as u8;
                } else {
                    let width = [2, 4, 8][draw(3)];
                    let off = draw(len / width) * width;
                    bytes[off..off + width].copy_from_slice(&BOUNDARY[draw(BOUNDARY.len())].to_le_bytes()[..width]);
                }
            }
            reseal_node(&mut bytes, 0, len);
            let mut image = pristine.clone();
            image[at..at + len].copy_from_slice(&bytes);
            put(&dev, &image, at, len);
            exercise(&dev, unlock, true);
            put(&dev, &pristine, at, len);
        }
    }
}

/// Every entry of `dir`, a page at a time.
fn listing<D: BlockDev>(v: &Volume<D>, dir: u64) -> Vec<DirEntry> {
    let mut out = Vec::new();
    let mut cursor = 0;
    loop {
        let (entries, more) = block_on_ready(v.readdir(dir, cursor, 64)).unwrap();
        cursor = entries.last().map_or(cursor, |e| e.next);
        out.extend(entries);
        if !more {
            return out;
        }
    }
}

/// What a generated tree holds at one path.
#[derive(Clone, Debug)]
enum Want<'a> {
    Dir,
    /// Its bytes, and which file of the tree it is: names of one file
    /// share an inode.
    File(&'a [u8], usize),
    Symlink(Vec<u8>),
}

/// A generated tree and, for every path, what it holds and its metadata.
struct Generated<'a> {
    tree: Tree<'a>,
    paths: Vec<(String, Want<'a>, Meta)>,
}

/// File sizes around every boundary the writer splits at: a block, a
/// 16 KiB bucket, a crc32 entry's 64 KiB, the 256 KiB encoded-extent
/// maximum.
const SIZES: [usize; 14] = [0, 1, 11, 4095, 4096, 4097, 16384, 16385, 65535, 65536, 65537, 262144, 262145, 1 << 20 | 3];

/// One file of each of [`SIZES`]; every third all zeros, whose type-5
/// checksum is 0.
fn contents(seed: u64) -> Vec<Vec<u8>> {
    let mut rng = Seeded(seed);
    SIZES
        .iter()
        .enumerate()
        .map(|(i, &n)| {
            let mut v = vec![0u8; n];
            if i % 3 != 2 {
                rng.fill(&mut v).unwrap();
            }
            v
        })
        .collect()
}

fn below(rng: &mut Seeded, n: usize) -> usize {
    let mut b = [0u8; 8];
    rng.fill(&mut b).unwrap();
    (u64::from_le_bytes(b) % n as u64) as usize
}

/// A chain of directories twelve deep, a directory of `wide` small files,
/// every one of `data` somewhere, files linked into other directories, and
/// symlinks up to the longest target.
fn generate(data: &[Vec<u8>], seed: u64, wide: usize) -> Generated<'_> {
    let mut rng = Seeded(seed);
    let file = Meta { perm: 0o644, uid: 1000, gid: 1000 };
    let dir = Meta { perm: 0o755, uid: 0, gid: 0 };
    let symlink = Meta { perm: 0o777, ..file };
    let mut tree = Tree::new();
    let mut paths = Vec::new();
    let mut dirs = vec![(Tree::ROOT, String::new())];
    let join = |at: &str, name: &str| if at.is_empty() { name.to_string() } else { format!("{at}/{name}") };
    let mut at = (Tree::ROOT, String::new());
    for i in 0..12 {
        let name = format!("level-{i}");
        at = (tree.dir(at.0, name.as_bytes(), dir).unwrap(), join(&at.1, &name));
        paths.push((at.1.clone(), Want::Dir, dir));
        dirs.push(at.clone());
    }
    let many = (tree.dir(Tree::ROOT, b"wide", dir).unwrap(), "wide".to_string());
    paths.push(("wide".into(), Want::Dir, dir));
    dirs.push(many.clone());
    let mut files = 0;
    for i in 0..wide {
        let name = format!("f{i:05}");
        tree.file(many.0, name.as_bytes(), &data[i % 3], file).unwrap();
        paths.push((join("wide", &name), Want::File(&data[i % 3], files), file));
        files += 1;
    }
    let mut sized = Vec::new();
    for bytes in data {
        let (d, path) = &dirs[below(&mut rng, dirs.len())];
        let name = format!("size-{}", bytes.len());
        sized.push((tree.file(*d, name.as_bytes(), bytes, file).unwrap(), bytes.as_slice(), files));
        paths.push((join(path, &name), Want::File(bytes, files), file));
        files += 1;
    }
    for (i, &(node, bytes, id)) in sized.iter().enumerate().step_by(3) {
        for j in 0..1 + below(&mut rng, 3) {
            let (d, path) = &dirs[below(&mut rng, dirs.len())];
            let name = format!("link-{i}-{j}");
            tree.link(*d, name.as_bytes(), node).unwrap();
            paths.push((join(path, &name), Want::File(bytes, id), file));
        }
    }
    let longest = vec![b'x'; 4095];
    for (d, path, name, target) in [
        (Tree::ROOT, String::new(), "to-deep", b"level-0/level-1/level-2/size-1".to_vec()),
        (at.0, at.1, "up", b"../../..".to_vec()),
        (many.0, many.1, "longest", longest),
    ] {
        tree.symlink(d, name.as_bytes(), &target, symlink).unwrap();
        paths.push((join(&path, name), Want::Symlink(target), symlink));
    }
    Generated { tree, paths }
}

/// The volume holds exactly what `g` generated: every path's kind,
/// metadata, bytes or target, every file's names on one inode with that
/// link count, and every directory listing only its entries.
fn holds<D: BlockDev>(v: &Volume<D>, g: &Generated) {
    let mut names: BTreeMap<usize, u32> = BTreeMap::new();
    for (_, want, _) in &g.paths {
        if let Want::File(_, id) = want {
            *names.entry(*id).or_default() += 1;
        }
    }
    let mut inos: BTreeMap<usize, u64> = BTreeMap::new();
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    children.insert(String::new(), Vec::new());
    for (p, want, meta) in &g.paths {
        let (parent, name) = p.rsplit_once('/').unwrap_or(("", p));
        children.get_mut(parent).unwrap().push(name.to_string());
        let ino = path(v, p);
        let inode = block_on_ready(v.inode(ino)).unwrap();
        assert_eq!((inode.mode & 0o7777, inode.uid, inode.gid), (meta.perm, meta.uid, meta.gid), "{p}");
        match want {
            Want::Dir => {
                assert!(inode.is_dir(), "{p}");
                children.insert(p.clone(), Vec::new());
            }
            Want::File(bytes, id) => {
                assert_eq!(inode.mode >> 12, 0o10, "{p}");
                assert!(read_all(v, ino).unwrap() == *bytes, "{p}: contents");
                assert_eq!(inode.nlink, names[id], "{p}");
                assert_eq!(*inos.entry(*id).or_insert(ino), ino, "{p}: a link on its file's inode");
            }
            Want::Symlink(target) => assert_eq!(&block_on_ready(v.readlink(ino)).unwrap(), target, "{p}"),
        }
    }
    for (dir, mut want) in children {
        let ino = if dir.is_empty() { v.root() } else { path(v, &dir) };
        let mut have: Vec<String> = listing(v, ino).into_iter().map(|e| String::from_utf8(e.name).unwrap()).collect();
        have.sort();
        want.sort();
        assert_eq!(have, want, "listing of /{dir}");
    }
}

fn populated(bytes: usize, tree: &Tree, seed: u64) -> Image {
    let dev = Bytes::new(vec![0u8; bytes]);
    let options = Options { label: b"toyos", time: 1_761_177_600_000_000_000, encryption: None };
    block_on_ready(mkfs(&dev, &options, tree, &mut Seeded(seed))).unwrap();
    dev
}

#[test]
fn populated_trees_read_back_and_check_clean() {
    // 8 MiB: the fewest buckets, 16 KiB each, so every extent fits a crc32
    // entry. 32 MiB: inodes, dirents and extents span leaves under interior
    // roots. 1 GiB: 512 KiB buckets and 256 KiB nodes, extents up to 256
    // KiB in crc64 entries, and a directory wide enough to split them.
    let data = contents(7);
    for (size, wide, seed) in [(8 << 20, 40, 11), (32 << 20, 700, 12), (1 << 30, 3000, 13)] {
        let g = generate(&data, seed, wide);
        let dev = populated(size, &g.tree, seed);
        let v = block_on_ready(Volume::open(&dev)).unwrap();
        holds(&v, &g);
        assert_eq!(block_on_ready(fsck(&v)).unwrap(), [], "{size}");
        if size == 32 << 20 {
            for btree in [Btree::Inodes, Btree::Dirents, Btree::Extents] {
                assert!(v.roots().iter().any(|r| r.btree == btree && r.level > 0), "{btree:?} spans leaves");
            }
        }
    }
}

#[test]
fn a_populated_volume_is_a_function_of_its_tree_random_source_and_time() {
    let data = contents(7);
    let g = generate(&data, 12, 100);
    assert!(populated(32 << 20, &g.tree, 1).into_inner() == populated(32 << 20, &g.tree, 1).into_inner());
}

#[test]
fn a_tree_past_the_device_is_refused() {
    // 8 MiB: data from bucket 137, the end copy from bucket 448, one node
    // per bucket. 512 buckets of data put the first node at 649; 310 put
    // the 15 roots at 447–461. Each needs those and the end copy's 64.
    for (bytes, blocks) in [(8 << 20, (649 + 1 + 64) * 4), (310 << 14, (461 + 1 + 64) * 4)] {
        let data = vec![1u8; bytes];
        let mut tree = Tree::new();
        tree.file(Tree::ROOT, b"big", &data, Meta { perm: 0o644, uid: 0, gid: 0 }).unwrap();
        let dev = Bytes::new(vec![0u8; 8 << 20]);
        let options = Options { label: b"", time: 0, encryption: None };
        assert_eq!(block_on_ready(mkfs(&dev, &options, &tree, &mut Seeded(1))), Err(Error::Small { blocks }));
    }
}

#[test]
fn an_encrypted_volume_takes_directories_but_no_data() {
    let secret = hex_secret();
    let meta = Meta { perm: 0o700, uid: 1000, gid: 1000 };
    let mut tree = Tree::new();
    let home = tree.dir(Tree::ROOT, b"home", meta).unwrap();
    tree.dir(home, b"user", meta).unwrap();
    let dev = Bytes::new(vec![0u8; 32 << 20]);
    let options = Options { label: b"", time: 0, encryption: Some(Encryption { passphrase: &secret, kdf: MIN_KDF }) };
    block_on_ready(mkfs(&dev, &options, &tree, &mut Seeded(1))).unwrap();
    let v = block_on_ready(Volume::unlock(&dev, &secret, Limits { memory: 1024, work: 2 })).unwrap();
    assert_eq!(names(&v, path(&v, "home")), ["user"]);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);

    tree.symlink(home, b"link", b"user", meta).unwrap();
    assert_eq!(block_on_ready(mkfs(&dev, &options, &tree, &mut Seeded(1))), Err(Error::Unsupported("file data or a symlink on an encrypted volume")));
}

/// The superblock rules ToyOS writes by and upstream's volumes do not:
/// upstream sets inline data, discard and rotational, names the device
/// and keeps its counters, errors and recovery-pass fields.
const UPSTREAM_ONLY: [Finding; 7] = [
    Finding::Superblock("format's feature set"),
    Finding::Superblock("member discard off"),
    Finding::Superblock("member not rotational, and known"),
    Finding::Superblock("member device name, model and serial zero"),
    Finding::Superblock("no counters field"),
    Finding::Superblock("no errors_v2 field"),
    Finding::Superblock("no recovery_passes field"),
];

/// Upstream's tools wrote this volume (`NOTICE`): a file with three names
/// in three directories, two with two, a multi-extent file, a symlink, a
/// tree five deep and 600 small files, so inodes, dirents and extents span
/// leaves. The counts are what `find` reported in upstream's FUSE mount.
#[test]
fn the_links_fixture_reads_its_tree_and_derives_its_own_state() {
    let dev = fixture("links.img.gz");
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    for (p, ino, nlink, size) in [
        ("a", 2147483649, 3, 0),
        ("a/b", 2147483650, 3, 0),
        ("a/b/c", 2147483651, 3, 0),
        ("a/b/c/d", 2147483652, 3, 0),
        ("a/b/c/d/e", 2147483653, 2, 0),
        ("a/hello.txt", 2147483655, 3, 11),
        ("a/b/hello-link", 2147483655, 3, 11),
        ("a/b/c/d/e/hello-deep", 2147483655, 3, 11),
        ("a/b/c/empty-file", 2147483658, 1, 0),
        ("a/b/link", 2147483657, 1, 13),
        ("a/b/seq.txt", 2147483656, 2, 228894),
        ("seq-again", 2147483656, 2, 228894),
        ("empty", 2147483654, 2, 0),
        ("lost+found", 2147483648, 2, 0),
        ("many", 2147483659, 2, 0),
        ("many/f1", 2147483660, 2, 2),
        ("many/f1-twin", 2147483660, 2, 2),
    ] {
        let inode = block_on_ready(v.inode(path(&v, p))).unwrap();
        assert_eq!((inode.ino, inode.nlink, inode.size), (ino, nlink, size), "{p}");
    }
    assert_eq!(block_on_ready(v.inode(v.root())).unwrap().nlink, 6);
    assert_eq!(read_all(&v, path(&v, "a/b/c/d/e/hello-deep")).unwrap(), b"hello-toyos");
    assert_eq!(read_all(&v, path(&v, "seq-again")).unwrap(), seq());
    assert_eq!(block_on_ready(v.readlink(path(&v, "a/b/link"))).unwrap(), b"../hello.txt");
    let many = listing(&v, path(&v, "many"));
    assert_eq!(many.len(), 601);
    for e in many {
        let name = String::from_utf8(e.name).unwrap();
        let n = name.strip_prefix('f').unwrap().trim_end_matches("-twin");
        assert_eq!(read_all(&v, e.ino).unwrap(), format!("{n}\n").into_bytes(), "{name}");
    }
    for btree in [Btree::Inodes, Btree::Dirents, Btree::Extents] {
        assert!(v.roots().iter().any(|r| r.btree == btree && r.level > 0), "{btree:?} spans leaves");
    }
    // Every count fsck derives — link counts, sectors, alloc, freespace,
    // need_discard, bucket_gens, the LRU, backpointers, accounting —
    // agrees with what upstream stored.
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), UPSTREAM_ONLY);
}

#[test]
fn the_crc32c_fixture_derives_its_own_state() {
    let dev = fixture("crc32c.img.gz");
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), UPSTREAM_ONLY);
}

/// A small populated volume whose btrees are single leaves.
fn small() -> Image {
    let mut tree = Tree::new();
    let meta = Meta { perm: 0o644, uid: 0, gid: 0 };
    let d = tree.dir(Tree::ROOT, b"d", meta).unwrap();
    let f = tree.file(d, b"f", b"hello-toyos", meta).unwrap();
    tree.link(Tree::ROOT, b"g", f).unwrap();
    tree.file(Tree::ROOT, b"big", &[7u8; 100_000], meta).unwrap();
    populated(8 << 20, &tree, 3)
}

fn findings(dev: &Image) -> Vec<Finding> {
    block_on_ready(fsck(&block_on_ready(Volume::open(dev)).unwrap())).unwrap()
}

/// Rewrites every extent's checksum entry with `entry(data, extent)`,
/// where `data` is the extent's bytes on the device.
fn rewrite_extents(dev: &Image, entry: impl Fn(&[u8], &Extent) -> Vec<u8>) {
    let v = block_on_ready(Volume::open(dev)).unwrap();
    let mut data = Vec::new();
    block_on_ready(super::read::Source::walk(&v, Btree::Extents, Bpos::MIN, |k, value| {
        let e = Extent::decode(value)?;
        data.push((k.pos, block_on_ready(super::read::read_bytes(dev, e.ptr.sector * 512, e.crc.unwrap().compressed as usize * 512))?));
        Ok(true)
    }))
    .unwrap();
    rewrite_leaf(dev, Btree::Extents, Bpos::MIN, 0, |keys| {
        for (k, value) in keys.iter_mut() {
            let bytes = &data.iter().find(|(p, _)| *p == k.pos).unwrap().1;
            *value = entry(bytes, &Extent::decode(value).unwrap());
        }
    });
}

/// An extent value with no checksum entry: its pointer alone.
fn no_crc(_: &[u8], e: &Extent) -> Vec<u8> {
    e.ptr.encode().to_le_bytes().to_vec()
}

/// An extent checksummed type 1, the metadata type, as crc32c over it.
fn type_1(data: &[u8], e: &Extent) -> Vec<u8> {
    Extent::encode(e.crc.unwrap().compressed, 1, crate::crc32c::crc32c(data), e.ptr)
}

#[test]
fn an_extent_with_no_checksum_entry_or_the_metadata_type_is_found() {
    let check = Finding::Key { btree: Btree::Extents, pos: Bpos::new(FIRST + 2, 200, u32::MAX), rule: "data checksum type other than the volume's options imply" };
    let dev = small();
    rewrite_extents(&dev, type_1);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(read_all(&v, FIRST + 2).unwrap(), [7u8; 100_000], "a type-1 extent still verifies");
    assert!(block_on_ready(fsck(&v)).unwrap().contains(&check));

    let dev = small();
    rewrite_extents(&dev, no_crc);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(read_all(&v, FIRST + 2), Err(Error::Unsupported("extent without a checksum entry")));
    assert!(block_on_ready(fsck(&v)).unwrap().contains(&check));
}

const FIRST: u64 = 1 << 31;

/// Rewrites the inode `ino` as `edit` leaves it.
fn edit_inode(dev: &Image, ino: u64, edit: impl FnOnce(&mut InodeValue)) {
    rewrite_leaf(dev, Btree::Inodes, Bpos::new(0, ino, u32::MAX), 0, |keys| {
        let (_, value) = keys.iter_mut().find(|(k, _)| k.pos.offset == ino).unwrap();
        let mut inode = InodeValue::decode(value).unwrap();
        edit(&mut inode);
        *value = inode.encode();
    });
}

#[test]
fn a_link_count_or_size_its_keys_disagree_with_is_found() {
    use super::value::inode_field;
    let f = FIRST + 1;
    for (ino, edit, rule) in [
        (f, (|i: &mut InodeValue| i.set(inode_field::NLINK, 2)) as fn(&mut InodeValue), "link count other than its names"),
        (FIRST + 2, |i| i.sectors -= 8, "sectors other than its extents'"),
        (FIRST + 2, |i| i.size = 4096, "extent past its size"),
        (FIRST, |i| i.set(inode_field::NLINK, 1), "directory link count other than its subdirectories"),
        (FIRST, |i| i.set(inode_field::DIR_OFFSET, 3), "back-reference to no dirent naming it"),
    ] {
        let dev = small();
        edit_inode(&dev, ino, edit);
        assert!(findings(&dev).contains(&Finding::Inode { ino, rule }), "{rule}: {:?}", findings(&dev));
    }
}

#[test]
fn a_dirent_off_its_hash_slot_or_naming_a_second_directory_name_is_found() {
    let dev = small();
    let mut moved = 0;
    rewrite_leaf(&dev, Btree::Dirents, Bpos::MIN, 0, |keys| {
        let (k, _) = keys.iter_mut().find(|(k, _)| k.pos.inode == 4096).unwrap();
        k.pos.offset += 1;
        moved = k.pos.offset;
    });
    assert!(findings(&dev).contains(&Finding::Dirent { dir: 4096, offset: moved, rule: "away from its name's hash slot" }));

    let dev = small();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let slot = hash_of(&v, v.root(), b"d-again");
    rewrite_leaf(&dev, Btree::Dirents, Bpos::MIN, 0, |keys| keys.push((Key::new(kind::DIRENT, Bpos::new(4096, slot, u32::MAX)), super::value::dirent(FIRST, DirentType::Directory, b"d-again"))));
    let found = findings(&dev);
    assert!(found.contains(&Finding::Inode { ino: FIRST, rule: "directory not named once, by its back-reference" }), "{found:?}");
    assert!(found.contains(&Finding::Inode { ino: 4096, rule: "directory link count other than its subdirectories" }), "{found:?}");
}

/// The slot `name` hashes to in directory `dir`.
fn hash_of<D: BlockDev>(v: &Volume<D>, dir: u64, name: &[u8]) -> u64 {
    let mut value = None;
    block_on_ready(super::read::Source::walk(v, Btree::Inodes, Bpos::new(0, dir, u32::MAX), |_, val| {
        value = Some(InodeValue::decode(val)?);
        Ok(false)
    }))
    .unwrap();
    super::read::hash(&value.unwrap(), name).unwrap()
}

#[test]
fn a_derived_key_missing_or_off_by_one_is_found() {
    use super::value::counters;
    let dev = small();
    let mut gone = None;
    rewrite_leaf(&dev, Btree::Backpointers, Bpos::MIN, 0, |keys| gone = keys.pop().map(|(k, _)| k.pos));
    assert!(findings(&dev).contains(&Finding::Derived { btree: Btree::Backpointers, pos: gone.unwrap(), rule: "missing" }));

    let dev = small();
    let mut changed = None;
    rewrite_leaf(&dev, Btree::Accounting, Bpos::MIN, 0, |keys| {
        let (k, v) = &mut keys[0];
        let n = u64::from_le_bytes(v[..8].try_into().unwrap());
        *v = counters(&[n + 1]);
        changed = Some(k.pos);
    });
    assert!(findings(&dev).contains(&Finding::Derived { btree: Btree::Accounting, pos: changed.unwrap(), rule: "differs" }));

    // The 100 000-byte file's last bucket is part full: its LRU entry.
    let dev = small();
    let mut lru = None;
    rewrite_leaf(&dev, Btree::Lru, Bpos::MIN, 0, |keys| lru = keys.pop().map(|(k, _)| k.pos));
    assert!(findings(&dev).contains(&Finding::Derived { btree: Btree::Lru, pos: lru.unwrap(), rule: "missing" }));

    let dev = small();
    let mut free = None;
    rewrite_leaf(&dev, Btree::Freespace, Bpos::MIN, 0, |keys| free = keys.pop().map(|(k, _)| k.pos));
    assert!(findings(&dev).contains(&Finding::Derived { btree: Btree::Freespace, pos: free.unwrap(), rule: "missing" }));
}

#[test]
fn overlapping_journal_buckets_are_refused() {
    let dev = formatted(8 << 20, None);
    rewrite_superblock(&dev, |_, fields| {
        let body = [(20u64, 8u64), (24, 8)].iter().flat_map(|(a, b)| [a.to_le_bytes(), b.to_le_bytes()].concat()).collect();
        fields.iter_mut().find(|f| f.kind == 9).unwrap().body = body;
    });
    assert_eq!(open_err(&dev), Error::Corrupt("journal buckets"));
}

/// A member claiming more buckets than the device holds would have the
/// check walk them all.
#[test]
fn a_member_past_its_device_is_refused() {
    let dev = formatted(8 << 20, None);
    rewrite_superblock(&dev, |_, fields| {
        let members = fields.iter_mut().find(|f| f.kind == 11).unwrap();
        members.body[8 + 16..8 + 24].copy_from_slice(&((1u64 << 31) - 65).to_le_bytes());
    });
    assert_eq!(open_err(&dev), Error::Corrupt("member's buckets past the device"));
}

#[test]
fn a_directory_whose_back_references_loop_is_unreachable() {
    use super::value::inode_field;
    let dev = small();
    edit_inode(&dev, FIRST, |i| i.set(inode_field::DIR, FIRST));
    let found = findings(&dev);
    assert!(found.contains(&Finding::Inode { ino: FIRST, rule: "directory unreachable from the root" }), "{found:?}");
}

#[test]
fn an_inode_size_near_2_64_is_checked_without_overflow() {
    let dev = small();
    edit_inode(&dev, FIRST + 2, |i| i.size = u64::MAX);
    assert!(!findings(&dev).iter().any(|f| matches!(f, Finding::Inode { rule: "extent past its size", .. })));
}

/// ToyOS volumes hold no need_discard or bucket_gens key, so those
/// comparisons are held on the upstream fixture.
#[test]
fn an_alloc_need_discard_or_bucket_gens_key_off_its_derivation_is_found() {
    let dev = small();
    let mut changed = None;
    rewrite_leaf(&dev, Btree::Alloc, Bpos::MIN, 0, |keys| {
        let (k, v) = &mut keys[0];
        v[16] ^= 1; // dirty sectors
        changed = Some(k.pos);
    });
    assert!(findings(&dev).contains(&Finding::Derived { btree: Btree::Alloc, pos: changed.unwrap(), rule: "differs" }));

    let dev = fixture("links.img.gz");
    let mut gone = None;
    rewrite_leaf(&dev, Btree::NeedDiscard, Bpos::MIN, 0, |keys| gone = keys.pop().map(|(k, _)| k.pos));
    assert!(findings(&dev).contains(&Finding::Derived { btree: Btree::NeedDiscard, pos: gone.unwrap(), rule: "missing" }));

    let dev = fixture("links.img.gz");
    let mut changed = None;
    rewrite_leaf(&dev, Btree::BucketGens, Bpos::MIN, 0, |keys| {
        let (k, v) = &mut keys[0];
        let i = v.iter().position(|&g| g != 0).unwrap();
        v[i] += 1;
        changed = Some(k.pos);
    });
    assert!(findings(&dev).contains(&Finding::Derived { btree: Btree::BucketGens, pos: changed.unwrap(), rule: "differs" }));
}

/// The ranges' listed order is the journal's ring.
#[test]
fn journal_buckets_keep_their_listed_order() {
    let dev = formatted(8 << 20, None);
    rewrite_superblock(&dev, |_, fields| {
        let body = [(30u64, 4u64), (20, 8)].iter().flat_map(|(a, b)| [a.to_le_bytes(), b.to_le_bytes()].concat()).collect();
        fields.iter_mut().find(|f| f.kind == 9).unwrap().body = body;
    });
    assert_eq!(block_on_ready(Volume::open(&dev)).unwrap().superblock().journal, [(30, 4), (20, 8)]);
}

fn keys_of<D: BlockDev>(v: &Volume<D>, btree: Btree) -> usize {
    let mut n = 0;
    block_on_ready(super::read::Source::walk(v, btree, Bpos::MIN, |_, _| {
        n += 1;
        Ok(true)
    }))
    .unwrap();
    n
}

/// `fit`'s size is one mkfs fills: no free bucket, so no freespace key,
/// and a device one bucket smaller is refused. A tree the smallest device
/// holds gets that device.
#[test]
fn a_fitted_volume_has_no_free_bucket_and_a_bucket_less_is_refused() {
    let data = contents(7);
    let options = Options { label: b"toyos", time: 0, encryption: None };
    assert_eq!(fit(&options, &Tree::new(), &Seeded(20)), Ok(2048), "the smallest device, 512 buckets");
    // Past the smallest device, in 16 KiB buckets, then in 64 KiB ones.
    let big: Vec<Vec<u8>> = [9usize << 20, 30 << 20, 150 << 20].iter().map(|&n| (0..n).map(|i| (i / 4096) as u8).collect()).collect();
    for (wide, big, seed) in [(40, &big[0], 21), (3000, &big[1], 22), (700, &big[2], 23)] {
        let mut g = generate(&data, seed, wide);
        g.tree.file(Tree::ROOT, b"big", big, Meta { perm: 0o644, uid: 0, gid: 0 }).unwrap();
        g.paths.push(("big".into(), Want::File(big, usize::MAX), Meta { perm: 0o644, uid: 0, gid: 0 }));
        let tree = &g.tree;
        let blocks = fit(&options, tree, &Seeded(seed)).unwrap();
        let dev = Bytes::new(vec![0u8; blocks as usize * BLOCK]);
        block_on_ready(mkfs(&dev, &options, tree, &mut Seeded(seed))).unwrap();
        let v = block_on_ready(Volume::open(&dev)).unwrap();
        holds(&v, &g);
        assert_eq!(block_on_ready(fsck(&v)).unwrap(), [], "{wide}");
        assert_eq!(keys_of(&v, Btree::Freespace), 0, "{wide}: a free bucket at {blocks} blocks");
        let bucket = u64::from(v.superblock().member.bucket_sectors) / 8;
        let smaller = Bytes::new(vec![0u8; (blocks - bucket) as usize * BLOCK]);
        assert!(matches!(block_on_ready(mkfs(&smaller, &options, tree, &mut Seeded(seed))), Err(Error::Small { .. })), "{wide}");
    }
}

#[test]
fn every_name_is_listed_once_under_its_path_and_resolves_to_its_node() {
    let data = contents(7);
    let g = generate(&data, 25, 300);
    let dev = populated(32 << 20, &g.tree, 25);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let names = block_on_ready(v.names()).unwrap();
    let mut listed: Vec<String> = names.iter().map(|(p, _)| String::from_utf8(p.clone()).unwrap()).collect();
    listed.sort();
    let mut want: Vec<String> = g.paths.iter().map(|(p, _, _)| p.clone()).collect();
    want.sort();
    assert_eq!(listed, want);
    for (p, e) in &names {
        assert_eq!(block_on_ready(v.resolve(p)), Ok(e.ino));
        let parent = p.rsplitn(2, |&b| b == b'/').nth(1).unwrap_or(b"");
        assert!(block_on_ready(v.inode(block_on_ready(v.resolve(parent)).unwrap())).unwrap().is_dir());
    }
    assert_eq!(block_on_ready(v.resolve(b"")), Ok(v.root()));
    assert_eq!(block_on_ready(v.resolve(b"wide/absent")), Err(Error::NotFound));
    assert_eq!(block_on_ready(v.resolve(b"wide/f00000/under-a-file")), Err(Error::NotDirectory));
    let link = block_on_ready(v.resolve(b"to-deep")).unwrap();
    assert_eq!(block_on_ready(v.inode(link)).unwrap().kind(), Some(DirentType::Symlink), "a symlink is not followed");
}

/// A dirent naming an ancestor as a directory would make the listing a
/// cycle; it is refused.
#[test]
fn a_directory_named_twice_is_refused_by_the_listing() {
    let dev = formatted(8 << 20, None);
    rewrite_leaf(&dev, Btree::Dirents, Bpos::MIN, 0, |keys| keys.push((Key::new(kind::DIRENT, Bpos::new(4096, 77, u32::MAX)), dirent(4096, 4, b"loop"))));
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.names()), Err(Error::Corrupt("a directory with a second name")));
}

#[test]
fn read_file_reads_a_file_whole_and_refuses_other_nodes() {
    let dev = small();
    let bytes = dev.into_inner();
    assert_eq!(read_file(&bytes, b"d/f"), Ok(b"hello-toyos".to_vec()));
    assert_eq!(read_file(&bytes, b"big"), Ok(vec![7u8; 100_000]));
    assert_eq!(read_file(&bytes, b"d"), Err(Error::Invalid("a path naming no regular file")));
    assert_eq!(read_file(&bytes, b"d/absent"), Err(Error::NotFound));
    assert_eq!(read_file(&bytes[..bytes.len() - 512], b"d/f"), Err(Error::Corrupt("a volume of no whole blocks")));
    // A size no volume this long can hold is refused before it is allocated.
    let dev = small();
    let volume = dev.get().len() as u64;
    edit_inode(&dev, FIRST + 1, |i| i.size = volume + 1);
    assert_eq!(read_file(&dev.into_inner(), b"d/f"), Err(Error::Corrupt("a file larger than its volume")));
}

mod admission;
mod buckets;
mod crash;
mod interleave;
mod journal;
mod mount;
mod ops;

/// mkfs lays a file's data out in one run whatever its extent count, so a
/// volume in memory can hand the run itself out: the bytes there are the
/// file's, and each of its extents is verified on the way.
#[test]
fn a_file_mkfs_wrote_is_in_place_and_verified() {
    let meta = Meta { perm: 0o644, uid: 0, gid: 0 };
    // Seven 256 KiB extents and a partial block: past one extent, one bucket
    // and the block size.
    let big: Vec<u8> = (0..(7 << 18) + 1000).map(|i: usize| (i % 251) as u8 | 1).collect();
    let mut tree = Tree::new();
    tree.file(Tree::ROOT, b"big", &big, meta).unwrap();
    tree.file(Tree::ROOT, b"small", b"one block", meta).unwrap();
    tree.file(Tree::ROOT, b"empty", b"", meta).unwrap();
    tree.symlink(Tree::ROOT, b"link", b"big", meta).unwrap();
    let dev = Bytes::new(vec![0u8; 32 << 20]);
    let options = Options { label: b"", time: 0, encryption: None };
    block_on_ready(mkfs(&dev, &options, &tree, &mut Seeded(1))).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    for (name, bytes) in [("big", &big[..]), ("small", b"one block")] {
        let (at, len) = block_on_ready(v.in_place(path(&v, name))).unwrap().expect(name);
        assert_eq!(at % BLOCK as u64, 0, "{name}");
        assert_eq!(&dev.get()[at as usize..(at + len) as usize], bytes, "{name}");
    }
    for name in ["empty", "link"] {
        assert_eq!(block_on_ready(v.in_place(path(&v, name))).unwrap(), None, "{name}");
    }

    // The same volume with a byte of the big file's last extent changed.
    let (at, len) = block_on_ready(v.in_place(path(&v, "big"))).unwrap().unwrap();
    let mut changed = dev.get().clone();
    changed[(at + len - 1) as usize] ^= 0xff;
    let changed = Bytes::new(changed);
    let v = block_on_ready(Volume::open(&changed)).unwrap();
    assert_eq!(block_on_ready(v.in_place(path(&v, "big"))), Err(Error::Checksum("extent")));
}

/// Upstream's inline data is no run of the device, and its fourteen-extent
/// file, which its tools wrote in one run, is that run.
#[test]
fn an_upstream_volumes_file_in_one_run_is_in_place_and_inline_data_is_not() {
    let dev = fixture("crc32c.img.gz");
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.in_place(path(&v, "Documents/a.txt"))).unwrap(), None, "inline data");
    let (at, len) = block_on_ready(v.in_place(path(&v, "Documents/deep/seq.txt"))).unwrap().expect("one run");
    assert_eq!(dev.get()[at as usize..(at + len) as usize], seq()[..]);
}
