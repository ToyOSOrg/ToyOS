//! The reader against volumes upstream's tools wrote, and mkfs's volumes
//! against the reader and ToyOS's check.

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
    block_on_ready(v.walk(Btree::Extents, Bpos::new(ino, 1, 0), |_, value| {
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

/// Rewrites `btree`'s root leaf on an unencrypted mkfs volume with its keys
/// as `edit` leaves them, the bset stamped `bset_seq`. The keys bypass the
/// writer's type check, as a hostile image's would.
fn rewrite_leaf(dev: &Image, btree: Btree, bset_seq: u64, edit: impl FnOnce(&mut Vec<(Key, Vec<u8>)>)) {
    let v = block_on_ready(Volume::open(dev)).unwrap();
    let root = *v.roots().iter().find(|r| r.btree == btree).unwrap();
    assert_eq!(root.level, 0);
    let mut keys = block_on_ready(v.node(btree, 0, &root.ptr, root.key.pos)).unwrap().keys;
    edit(&mut keys);
    keys.sort_by_key(|(k, _)| k.pos);
    let encoded: Vec<Vec<u8>> = keys.iter().map(|(k, val)| k.encode(val)).collect();
    // A placeholder bset, then the keys put in raw, so the writer's own
    // assertions never see them.
    let build = Build { btree, level: 0, seq: root.ptr.seq, min: Bpos::MIN, max: Bpos::MAX, journal_seq: bset_seq, keys: &[] };
    let mut bytes = node::encode(node::magic(&v.superblock().internal_uuid()), BLOCK, None, &build).unwrap();
    let flat = encoded.concat();
    let end = 160 + flat.len();
    bytes.resize(end.next_multiple_of(BLOCK), 0);
    bytes[160..end].copy_from_slice(&flat);
    bytes[158..160].copy_from_slice(&((flat.len() / 8) as u16).to_le_bytes());
    let crc = crate::crc32c::crc32c(&bytes[16..end]);
    bytes[..4].copy_from_slice(&crc.to_le_bytes());
    assert!(bytes.len() <= usize::from(root.ptr.written) * 512, "the rewrite outgrew the node's written sectors");
    let mut blocks = vec![[0u8; BLOCK]; bytes.len() / BLOCK];
    blocks.as_flattened_mut().copy_from_slice(&bytes);
    block_on_ready(dev.write(root.ptr.ptr.sector / 8, &blocks)).unwrap();
}

/// The inodes root stamped `bset_seq`, the root directory `inode_seq`.
fn stamp(dev: &Image, bset_seq: u64, inode_seq: u64) {
    rewrite_leaf(dev, Btree::Inodes, bset_seq, |keys| {
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
    rewrite_leaf(&dev, Btree::Inodes, 0, |keys| {
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
    rewrite_leaf(&dev, Btree::Dirents, 0, |keys| keys.push((Key::new(kind::DIRENT, Bpos::new(4096, u64::MAX, u32::MAX)), dirent(4096 + 1, 8, b"x"))));
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.readdir(4096, 0, 10)).err(), Some(Error::Corrupt("dirent at the last offset")));
}

#[test]
fn a_symlink_past_4_kib_is_refused_before_it_is_allocated() {
    let dev = formatted(8 << 20, None);
    rewrite_leaf(&dev, Btree::Inodes, 0, |keys| {
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
    rewrite_leaf(&dev, Btree::Subvolumes, 0, |keys| keys[0].1 = super::value::subvolume(u32::MAX - 1, 4096).to_vec());
    assert_eq!(open_err(&dev), Error::Unsupported("a root subvolume that has been snapshotted"));
}

#[test]
fn a_strict_key_in_another_btrees_leaf_is_refused_and_a_set_key_is_found() {
    let dev = formatted(8 << 20, None);
    rewrite_leaf(&dev, Btree::Inodes, 0, |keys| keys.push((Key::new(kind::ALLOC_V4, Bpos::new(0, 5000, u32::MAX)), vec![0; 64])));
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.inode(4096)).err(), Some(Error::Corrupt("key of a type its btree does not hold")));

    // A set key is non-strict: upstream reads it anywhere, and refuses only
    // its commit. The reader passes it; ToyOS's check finds it.
    let dev = formatted(8 << 20, None);
    let pos = Bpos::new(0, 5000, u32::MAX);
    rewrite_leaf(&dev, Btree::Inodes, 0, |keys| keys.push((Key::new(kind::SET, pos), vec![])));
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
        assert_eq!(block_on_ready(mkfs(&dev, &options, &mut Seeded(1))), Err(Error::Invalid("passphrase holding NUL or ending in a newline")));
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
    let images: [(Image, Option<&[u8]>, usize); 3] = [
        (fixture("crc32c.img.gz"), None, 1500),
        (formatted(8 << 20, None), None, 1500),
        (formatted(8 << 20, Some(Encryption { passphrase: &secret, kdf: MIN_KDF })), Some(&secret), 300),
    ];
    let mut rng = Seeded(0xf022);
    let mut draw = |n: usize| {
        let mut b = [0u8; 8];
        rng.fill(&mut b).unwrap();
        (u64::from_le_bytes(b) % n as u64) as usize
    };
    for (dev, unlock, rounds) in images {
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
        for width in [4, 8] {
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
