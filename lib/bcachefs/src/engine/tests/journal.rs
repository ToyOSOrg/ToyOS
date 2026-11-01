//! Journal replay: the upstream unclean fixture, and journals laid into a
//! mkfs volume, torn by a crash or breaking a rule fsck checks.

use toyos_blockdev::model::Model;
use toyos_blockdev::{block_on_ready, BlockDev, ReadOnlyBytes, BLOCK};

use super::super::journal::{self, Jset};
use super::super::superblock::{field_type, Field, Root};
use super::super::value::{self, data_type};
use super::*;

/// 200 files `f1`…`f200` written through upstream's FUSE mount, each holding
/// its number and a newline, copied with the daemon stopped (BB-A2-1).
/// Upstream's own `fsck -n` replays entries 61–62: 115 keys read, 90 after
/// later keys at one position replace earlier ones.
#[test]
fn the_unclean_fixture_is_read_through_its_journal() {
    let dev = fixture("unclean.img.gz");
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let (window, replay) = v.replayed().expect("an unclean volume replays");
    assert_eq!((window.first, window.last, window.next), (61, 62, 63));
    // Accounting deltas add rather than replace, so each is kept.
    let deltas: usize = replay.accounting.values().map(Vec::len).sum();
    assert_eq!((replay.read, replay.keys.len() + deltas), (115, 90));
    let mut want: Vec<String> = (1..=200).map(|i| format!("f{i}")).collect();
    want.push("lost+found".into());
    want.sort();
    let (entries, more) = block_on_ready(v.readdir(v.root(), 0, 1000)).unwrap();
    assert!(!more);
    let mut got: Vec<String> = entries.into_iter().map(|e| String::from_utf8(e.name).unwrap()).collect();
    got.sort();
    assert_eq!(got, want);
    for i in 1..=200 {
        assert_eq!(read_all(&v, path(&v, &format!("f{i}"))).unwrap(), format!("{i}\n").into_bytes(), "f{i}");
    }
    // Every count fsck derives agrees with what upstream's nodes and journal
    // hold together, and the window's entries carry every record each must.
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), UPSTREAM_ONLY);
}

/// Marks a mkfs volume unclean as going read-write does: clean 0, and the
/// journal's replicas entry beside the btree's.
fn unclean(dev: &Image) {
    rewrite_superblock(dev, |h, fields| {
        h.flags[0] &= !(1 << 1);
        h.seq += 1;
        let replicas = fields.iter_mut().find(|f| f.kind == field_type::REPLICAS_V0).unwrap();
        *replicas = Field { kind: field_type::REPLICAS_V0, body: vec![data_type::JOURNAL, 1, 0, data_type::BTREE, 1, 0] };
    });
}

/// The records every entry carries after its keys: `roots`, a datetime, the
/// key version 0 and both clocks.
fn tail(area: &mut Vec<u8>, roots: &[Root]) {
    let keys: Vec<(Btree, u8, Vec<u8>)> = roots.iter().map(|r| (r.btree, r.level, r.key.encode(&r.ptr.encode()))).collect();
    journal::tail(area, keys.iter().map(|(b, l, k)| (*b, *l, &k[..])), Some(1_761_177_600), 0, [0; 2]);
}

/// A flush entry's records: one inode key, numbered `n` past the first
/// inode, then [`tail`].
fn area_with_inode(roots: &[Root], n: u64) -> Vec<u8> {
    let inode = InodeValue { flags: 0o100644 << 36, ..Default::default() };
    let key = Key::new(kind::INODE_V3, Bpos::new(0, mkfs::FIRST_INODE + n, mkfs::ROOT_SNAPSHOT));
    let mut area = Vec::new();
    journal::record(&mut area, journal::Rec::Keys { btree: Btree::Inodes, level: 0, keys: &key.encode(&inode.encode()) });
    tail(&mut area, roots);
    area
}

/// `dev` opened, unlocked with `passphrase` when it is encrypted.
fn open<D: BlockDev>(dev: D, passphrase: Option<&[u8]>) -> Result<Volume<D>> {
    match passphrase {
        Some(p) => block_on_ready(Volume::unlock(dev, p, MIN_LIMITS)),
        None => block_on_ready(Volume::open(dev)),
    }
}

/// What laying a journal into a volume needs: its clean roots, internal
/// UUID, master key and first journal block.
struct Ring {
    roots: Vec<Root>,
    uuid: [u8; 16],
    key: Option<super::super::crypt::MasterKey>,
    first: u64,
}

impl Ring {
    fn of(dev: &Image, passphrase: Option<&[u8]>) -> Ring {
        let sb = block_on_ready(super::super::read::read_superblock(dev)).unwrap();
        let key = passphrase.map(|p| super::super::crypt::MasterKey::unwrap(sb.crypt.as_ref().unwrap(), &sb.header.internal_uuid, p, MIN_LIMITS).unwrap());
        let first = sb.journal[0].0 * u64::from(sb.member.bucket_sectors) / 8;
        Ring { roots: sb.clean.as_ref().unwrap().roots.clone(), uuid: sb.header.internal_uuid, key, first }
    }

    /// The entry `seq` as written, whole blocks.
    fn entry(&self, seq: u64, last_seq: u64, area: &[u8]) -> Vec<[u8; BLOCK]> {
        let bytes = journal::encode(&self.uuid, BLOCK, self.key.as_ref(), &Jset { seq, last_seq, area }).unwrap();
        bytes.as_chunks::<BLOCK>().0.to_vec()
    }
}

/// Entries 1 to 3 written and flushed, then entry 4 written with no flush
/// after it, as a crash leaves it: 4 is kept whole, torn at any sector, or
/// lost. Replay ends at 4 exactly when it is whole and at 3 otherwise, and
/// replays each kept entry's inode; a torn entry fails its checksum and is
/// passed over, never trusted.
fn a_torn_entry_is_refused(encryption: Option<Encryption>) {
    let passphrase = encryption.as_ref().map(|e| e.passphrase.to_vec());
    let dev = formatted(8 << 20, encryption);
    unclean(&dev);
    let ring = Ring::of(&dev, passphrase.as_deref());
    let model = Model::new(dev.into_inner(), 512);
    let mut at = ring.first;
    let mut fourth = (0, Vec::new());
    for seq in 1..=4u64 {
        let blocks = ring.entry(seq, 1, &area_with_inode(&ring.roots, seq));
        block_on_ready(async {
            model.flush().await.unwrap();
            model.write(at, &blocks).await.unwrap();
            if seq < 4 {
                model.flush().await.unwrap();
            }
        });
        fourth = (at as usize * BLOCK, blocks.as_flattened().to_vec());
        at += blocks.len() as u64;
    }
    let search = model
        .check(1 << 12, 0x7041_5ee7, |image| {
            let whole = image[fourth.0..fourth.0 + fourth.1.len()] == fourth.1[..];
            let v = open(ReadOnlyBytes::new(image).unwrap(), passphrase.as_deref()).map_err(|e| format!("{e:?}"))?;
            let (window, replay) = v.replayed().ok_or("not replayed")?;
            let last = if whole { 4 } else { 3 };
            if window.last != last {
                return Err(format!("replay ended at {} with the fourth entry {}", window.last, if whole { "whole" } else { "torn or lost" }));
            }
            let inodes = replay.keys.keys().filter(|(b, _, _)| *b == Btree::Inodes).count() as u64;
            if inodes != last {
                return Err(format!("{inodes} inodes replayed through entry {last}"));
            }
            Ok(())
        })
        .unwrap_or_else(|v| panic!("{v}"));
    assert!(matches!(search, toyos_blockdev::model::Search::Every(_)), "{search:?}");
}

/// scrypt's smallest cost, which mkfs's test volumes are formatted with.
const MIN_LIMITS: Limits = Limits { memory: 1 << 20, work: 1 << 20 };

#[test]
fn a_torn_entry_is_refused_by_its_checksum_and_replay_ends_before_it() {
    a_torn_entry_is_refused(None);
}

#[test]
fn a_torn_encrypted_entry_is_refused_by_its_mac_and_replay_ends_before_it() {
    let secret = hex_secret();
    a_torn_entry_is_refused(Some(Encryption { passphrase: &secret, kdf: MIN_KDF }));
}

/// A mkfs volume whose journal holds the clean entry 1 at the ring's
/// start, with `clean` and the member's position naming it: what a clean
/// unmount leaves. `edit` changes the entry's records first, and `position`
/// overrides the member's.
fn clean_with_entry(edit: impl FnOnce(&mut Vec<u8>, &mut u64), position: Option<(u32, u32)>) -> Vec<Finding> {
    let dev = formatted(8 << 20, None);
    let ring = Ring::of(&dev, None);
    let mut area = Vec::new();
    tail(&mut area, &ring.roots);
    let mut seq = 1;
    edit(&mut area, &mut seq);
    let blocks = ring.entry(seq, seq, &area);
    block_on_ready(dev.write(ring.first, &blocks)).unwrap();
    let after = position.unwrap_or((0, (blocks.len() * 8) as u32));
    rewrite_superblock(&dev, |_, fields| {
        let clean = fields.iter_mut().find(|f| f.kind == field_type::CLEAN).unwrap();
        clean.body[8..16].copy_from_slice(&1u64.to_le_bytes());
        let members = fields.iter_mut().find(|f| f.kind == field_type::MEMBERS_V2).unwrap();
        members.body[8 + 136..8 + 140].copy_from_slice(&after.0.to_le_bytes());
        members.body[8 + 140..8 + 144].copy_from_slice(&after.1.to_le_bytes());
    });
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    block_on_ready(fsck(&v)).unwrap()
}

fn journal_rule(seq: u64, rule: &'static str) -> Vec<Finding> {
    vec![Finding::Journal { seq, rule }]
}

#[test]
fn a_clean_volumes_newest_entry_is_clean_s_or_found() {
    assert_eq!(clean_with_entry(|_, _| {}, None), [], "as a clean unmount writes it");
    assert_eq!(clean_with_entry(|_, seq| *seq = 2, None), journal_rule(2, "newest entry of another sequence than clean's"));
    let inode = |area: &mut Vec<u8>, _: &mut u64| {
        let key = Key::new(kind::INODE_V3, Bpos::new(0, mkfs::FIRST_INODE, mkfs::ROOT_SNAPSHOT));
        let mut keys = Vec::new();
        journal::record(&mut keys, journal::Rec::Keys { btree: Btree::Inodes, level: 0, keys: &key.encode(&InodeValue::default().encode()) });
        keys.append(area);
        *area = keys;
    };
    assert_eq!(clean_with_entry(inode, None), journal_rule(1, "clean volume's newest entry not empty"));
    // A root record: its header, the key and a one-pointer value.
    const ROOT: usize = 8 + 40 + 48;
    let second_root_dropped = |area: &mut Vec<u8>, _: &mut u64| {
        area.drain(ROOT..2 * ROOT);
    };
    assert_eq!(
        clean_with_entry(second_root_dropped, None),
        [Finding::Journal { seq: 1, rule: "newest entry's roots other than clean's" }, Finding::Journal { seq: 1, rule: "no root record for a btree with a root" }]
    );
    let roots = Ring::of(&formatted(8 << 20, None), None).roots.len();
    let datetime_dropped = |area: &mut Vec<u8>, _: &mut u64| {
        area.drain(roots * ROOT..roots * ROOT + 16);
    };
    assert_eq!(clean_with_entry(datetime_dropped, None), journal_rule(1, "no datetime record"));
    let write_clock_dropped = |area: &mut Vec<u8>, _: &mut u64| {
        area.truncate(area.len() - 24);
    };
    assert_eq!(clean_with_entry(write_clock_dropped, None), journal_rule(1, "not both clock records"));
    let version_raised = |area: &mut Vec<u8>, _: &mut u64| {
        let at = roots * ROOT + 16 + 8;
        area[at..at + 8].copy_from_slice(&7u64.to_le_bytes());
    };
    assert_eq!(clean_with_entry(version_raised, None), journal_rule(1, "newest entry's key version other than clean's"));
    assert_eq!(clean_with_entry(|_, _| {}, Some((1, 0))), journal_rule(1, "member's journal position other than after the newest entry"));
}


/// A mkfs volume marked unclean with `entries` laid into its ring, each
/// `(seq, last_seq, area, gap)`: the entry, then `gap` empty blocks.
fn unclean_with(entries: &[(u64, u64, Vec<u8>, u64)], edit: impl FnOnce(&Image, &Ring)) -> Image {
    let dev = formatted(8 << 20, None);
    unclean(&dev);
    let ring = Ring::of(&dev, None);
    let mut at = ring.first;
    for (seq, last_seq, area, gap) in entries {
        let blocks = ring.entry(*seq, *last_seq, area);
        block_on_ready(dev.write(at, &blocks)).unwrap();
        at += blocks.len() as u64 + gap;
    }
    edit(&dev, &ring);
    dev
}

fn blacklist(dev: &Image, start: u64, end: u64) {
    rewrite_superblock(dev, |h, fields| {
        h.features[0] |= 1 << 5;
        let mut body = start.to_le_bytes().to_vec();
        body.extend_from_slice(&end.to_le_bytes());
        fields.push(Field { kind: field_type::BLACKLIST, body });
    });
}

fn findings_of(dev: &Image) -> Vec<Finding> {
    let v = block_on_ready(Volume::open(dev)).unwrap();
    block_on_ready(fsck(&v)).unwrap()
}

#[test]
fn every_entry_of_the_window_carries_its_records_and_none_is_blacklisted() {
    let roots = Ring::of(&formatted(8 << 20, None), None).roots;
    let mut area = Vec::new();
    tail(&mut area, &roots);
    let whole = |seqs: &[u64]| seqs.iter().map(|&s| (s, 1, area.clone(), 0)).collect::<Vec<_>>();
    assert_eq!(findings_of(&unclean_with(&whole(&[1, 2, 3]), |_, _| {})), []);
    let mut no_clock = whole(&[1, 2, 3]);
    no_clock[1].2.truncate(area.len() - 24);
    assert_eq!(findings_of(&unclean_with(&no_clock, |_, _| {})), journal_rule(2, "not both clock records"));
    // Entry 2 blacklisted, as a crash after 1 and a replay before 3 leave it.
    let dev = unclean_with(&whole(&[1, 2, 3]), |dev, _| blacklist(dev, 2, 3));
    assert_eq!(findings_of(&dev), journal_rule(2, "flush entry of the window blacklisted"));
}

fn window_of(dev: &Image) -> Result<Option<journal::Window>> {
    block_on_ready(async {
        let sb = super::super::read::read_superblock(dev).await?;
        journal::scan(dev, &sb, None).await?.window(&sb)
    })
}

#[test]
fn a_window_with_an_entry_missing_or_failing_its_checksum_is_refused() {
    let roots = Ring::of(&formatted(8 << 20, None), None).roots;
    let mut area = Vec::new();
    tail(&mut area, &roots);
    let gap = unclean_with(&[(1, 1, area.clone(), 0), (3, 1, area.clone(), 0)], |_, _| {});
    assert_eq!(window_of(&gap), Err(Error::Corrupt("a journal entry missing from the replay window")));
    // The same, with 2 blacklisted: nothing of it can be missing.
    let skipped = unclean_with(&[(1, 1, area.clone(), 0), (3, 1, area.clone(), 0)], |dev, _| blacklist(dev, 2, 3));
    assert_eq!(window_of(&skipped).unwrap().map(|w| (w.first, w.last)), Some((1, 3)));
    // Entry 2's body damaged: past the newest, a torn write would be passed
    // over; inside the window it is refused.
    let damaged = unclean_with(&[(1, 1, area.clone(), 0), (2, 1, area.clone(), 0), (3, 1, area.clone(), 0)], |dev, ring| {
        let block = ring.first + ring.entry(1, 1, &area).len() as u64;
        flip(dev, block * BLOCK as u64 + 100);
    });
    assert_eq!(window_of(&damaged), Err(Error::Checksum("journal entry")));
    // With nothing laid in, mkfs's ring is empty: there is no window.
    assert_eq!(window_of(&unclean_with(&[], |_, _| {})).unwrap().map(|w| (w.first, w.last, w.next)), None);
}

/// What the ring holds past a bucket's newest entry is left over from an
/// earlier pass and is not read; past a damaged entry, the reader steps a
/// block at a time to the next.
#[test]
fn a_bucket_is_read_up_to_an_older_entry_and_past_a_damaged_one() {
    let roots = Ring::of(&formatted(8 << 20, None), None).roots;
    let mut area = Vec::new();
    tail(&mut area, &roots);
    let stale = unclean_with(&[(5, 5, area.clone(), 0), (2, 2, area.clone(), 0)], |_, _| {});
    let seqs = |dev: &Image| -> Vec<(u64, bool)> {
        block_on_ready(async {
            let sb = super::super::read::read_superblock(dev).await.unwrap();
            journal::scan(dev, &sb, None).await.unwrap().entries.values().map(|e| (e.seq, e.good)).collect()
        })
    };
    assert_eq!(seqs(&stale), [(5, true)]);
    let stepped = unclean_with(&[(1, 1, area.clone(), 0), (2, 2, area.clone(), 1), (3, 3, area.clone(), 0)], |dev, ring| {
        let block = ring.first + ring.entry(1, 1, &area).len() as u64;
        flip(dev, block * BLOCK as u64 + 100);
    });
    assert_eq!(seqs(&stepped), [(1, true), (2, false), (3, true)]);
    assert_eq!(window_of(&stepped).unwrap().map(|w| (w.first, w.last, w.next)), Some((3, 3, 4)));
}

/// An unclean volume whose journal holds an unlinked inode of `mode`, with or
/// without its deleted_inodes key: the orphan rules' findings.
fn orphan_findings(mode: u16, listed: bool) -> Vec<Finding> {
    let roots = Ring::of(&formatted(8 << 20, None), None).roots;
    let ino = mkfs::FIRST_INODE;
    let inode = InodeValue { flags: u64::from(mode) << 36 | value::inode_flag::UNLINKED, ..Default::default() };
    let mut area = Vec::new();
    let key = Key::new(kind::INODE_V3, Bpos::new(0, ino, mkfs::ROOT_SNAPSHOT));
    journal::record(&mut area, journal::Rec::Keys { btree: Btree::Inodes, level: 0, keys: &key.encode(&inode.encode()) });
    if listed {
        let key = Key::new(kind::SET, Bpos::new(0, ino, mkfs::ROOT_SNAPSHOT));
        journal::record(&mut area, journal::Rec::Keys { btree: Btree::DeletedInodes, level: 0, keys: &key.encode(&[]) });
    }
    tail(&mut area, &roots);
    let dev = unclean_with(&[(1, 1, area, 0)], |_, _| {});
    let rules = ["unlinked directory", "orphan and deleted_inodes key disagree"];
    findings_of(&dev).into_iter().filter(|f| matches!(f, Finding::Inode { rule, .. } if rules.contains(rule))).collect()
}

#[test]
fn an_unlinked_file_is_an_orphan_only_with_its_key_and_a_directory_never() {
    let ino = mkfs::FIRST_INODE;
    assert_eq!(orphan_findings(0o100644, true), []);
    assert_eq!(orphan_findings(0o100644, false), [Finding::Inode { ino, rule: "orphan and deleted_inodes key disagree" }]);
    assert_eq!(
        orphan_findings(0o40755, true),
        [Finding::Inode { ino, rule: "unlinked directory" }, Finding::Inode { ino, rule: "orphan and deleted_inodes key disagree" }]
    );
}

/// A clean volume holds no orphan and no logged operation (A8.14): fsck
/// finds a `deleted_inodes` key and a logged truncate left on one.
#[test]
fn a_clean_volume_holding_an_orphan_key_or_a_logged_op_is_found() {
    let dev = formatted(8 << 20, None);
    let ino = mkfs::FIRST_INODE;
    let orphan = Bpos::new(0, ino, mkfs::ROOT_SNAPSHOT);
    let op = Bpos::new(0, 1, 0);
    rewrite_leaf(&dev, Btree::DeletedInodes, Bpos::MIN, 0, |keys| keys.push((Key::new(kind::SET, orphan), vec![])));
    rewrite_leaf(&dev, Btree::LoggedOps, Bpos::MIN, 0, |keys| keys.push((Key::new(kind::LOGGED_OP_TRUNCATE, op), vec![0; 24])));
    let rules = ["deleted_inodes key on a clean volume", "logged operation on a clean volume"];
    let found: Vec<Finding> = findings_of(&dev).into_iter().filter(|f| matches!(f, Finding::Key { rule, .. } if rules.contains(rule))).collect();
    assert_eq!(
        found,
        [
            Finding::Key { btree: Btree::DeletedInodes, pos: orphan, rule: rules[0] },
            Finding::Key { btree: Btree::LoggedOps, pos: op, rule: rules[1] },
        ]
    );
}

/// A stored accounting total of zero is valid on any volume: upstream
/// keeps it (A7.23, A7.26) and compares it as absent (A7.28).
#[test]
fn a_stored_zero_accounting_total_is_no_finding_clean_or_unclean() {
    let mut record = vec![value::class::SNAPSHOT];
    record.extend_from_slice(&mkfs::ROOT_SNAPSHOT.to_le_bytes());
    record.extend_from_slice(&(Btree::Xattrs as u32).to_le_bytes());
    let pos = value::accounting_pos(&record);
    let zero = |dev: &Image| rewrite_leaf(dev, Btree::Accounting, pos, 0, |keys| keys.push((Key::new(kind::ACCOUNTING, pos), vec![0; 24])));

    let clean = formatted(8 << 20, None);
    zero(&clean);
    assert_eq!(findings_of(&clean), []);

    let roots = Ring::of(&formatted(8 << 20, None), None).roots;
    let mut area = Vec::new();
    tail(&mut area, &roots);
    let unclean = unclean_with(&[(1, 1, area, 0)], |dev, _| zero(dev));
    assert_eq!(findings_of(&unclean), []);
}

/// A window spanning 2^62 sequences, all but its two entries blacklisted, as
/// a crafted volume can make it: its check walks the entries, not the span.
#[test]
fn a_window_spanning_2_62_sequences_is_checked_entry_by_entry() {
    let roots = Ring::of(&formatted(8 << 20, None), None).roots;
    let mut area = Vec::new();
    tail(&mut area, &roots);
    let far = 1u64 << 62;
    let dev = unclean_with(&[(1, 1, area.clone(), 0), (far, 1, area.clone(), 0)], |dev, _| blacklist(dev, 2, far));
    assert_eq!(window_of(&dev).unwrap().map(|w| (w.first, w.last, w.next)), Some((1, far, far + 1)));
    let gap = unclean_with(&[(1, 1, area.clone(), 0), (far, 1, area, 0)], |dev, _| blacklist(dev, 3, far));
    assert_eq!(window_of(&gap), Err(Error::Corrupt("a journal entry missing from the replay window")));
}

/// A window whose keys a replay installs into one leaf past seven nodes'
/// worth: 110 inodes per 16 KiB of the volume's buckets, which are its
/// nodes, journaled across 13 entries, none yet in a node, as an upstream
/// volume a crash left can hold them. The reader replays it; the read-write
/// mount stops by name at its first write, since reclaim cannot split the
/// leaf in one transaction
/// (`issues/a-replayed-bcachefs-node-past-seven-nodes-is-refused.md`).
#[test]
fn a_replayed_leaf_past_seven_nodes_is_read_but_not_mounted_read_write() {
    let dev = formatted(64 << 20, None);
    let bucket_bytes = u64::from(block_on_ready(Volume::open(&dev)).unwrap().superblock().member.bucket_sectors) * SECTOR as u64;
    unclean(&dev);
    let ring = Ring::of(&dev, None);
    let bucket_blocks = bucket_bytes / BLOCK as u64;
    let mut ino = mkfs::FIRST_INODE + 1;
    for seq in 1..=13u64 {
        let mut keys = Vec::new();
        for _ in 0..110 * bucket_bytes / (16 << 10) {
            keys.extend(Key::new(kind::INODE_V3, Bpos::new(0, ino, mkfs::ROOT_SNAPSHOT)).encode(&InodeValue::default().encode()));
            ino += 1;
        }
        let mut area = Vec::new();
        journal::record(&mut area, journal::Rec::Keys { btree: Btree::Inodes, level: 0, keys: &keys });
        tail(&mut area, &ring.roots);
        let blocks = ring.entry(seq, 1, &area);
        assert!(blocks.len() as u64 <= bucket_blocks);
        block_on_ready(dev.write(ring.first + (seq - 1) * bucket_blocks, &blocks)).unwrap();
    }
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert!(block_on_ready(v.inode(ino - 1)).is_ok(), "the reader replays the window");
    drop(v);
    let m = block_on_ready(Mount::open(&dev, MountOptions { cache_nodes: 256 }, super::mount::random())).unwrap();
    let why = "a btree node whose keys fill more than seven nodes";
    assert_eq!(block_on_ready(m.create(m.root(), b"f", New::File, super::mount::OWNER, super::mount::NOW)).err(), Some(Error::Unsupported(why)));
    assert_eq!(block_on_ready(m.unmount()).err(), Some(Error::ReadOnly(why)));
}
