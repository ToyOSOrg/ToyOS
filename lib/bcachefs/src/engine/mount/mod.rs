//! A volume mounted read-write: one transaction at a time under the writer
//! slot (§3.3), the btree nodes it changes held in memory, the journal that
//! makes each transaction durable, and the reclaim that writes the nodes so
//! the journal can move on.
//!
//! **Order of waits.** A transaction takes admission first — journal space
//! and cache room at its bound — and the slot second. Nothing waits under the
//! slot but block I/O. A writer short of either at the head of admission,
//! holding nothing, runs reclaim itself; reclaim goes ahead of every queue
//! and takes only its own reserves, so a writer waiting on a full journal or
//! a full cache never blocks the reclaim that frees them.
//!
//! **A node write becomes visible only through its parent** (A2.21, A3.25):
//! the write completes, and only then does a reclaim transaction journal the
//! pointer update that makes it reachable. Until that update is in an entry,
//! the node's keys keep the journal pinned and the node stays in memory.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use toyos_blockdev::{BlockDev, BLOCK};
use toyos_crypto::{scrypt, Random};

use super::bkey::{Bpos, Key, Version, KEY_U64S};
use super::bucket::Bucket;
use super::cache::{key_bytes, Cache, Cached, Keys, NodeId};
use super::crypt::MasterKey;
use super::journal::{self, Class, Closed, Jset, Replayed, Writer, SKIP};
use super::mkfs::{self, COMPAT_CLEAN, ROOT_SNAPSHOT};
use super::node::{self, Build, FIRST_KEYS};
use super::read::{self, Inode, Parts, Source, Volume};
use super::superblock::{field_type, Field, Header, Member, Root, Superblock};
use super::sync::{Broadcast, Queue};
use super::value::{self, accounting_pos, class, data_type, inode_field, inode_flag, mode, AllocV4, DirentType, InodeValue, NodePtr, Ptr, MAX_NAME};
use super::{kind, Btree, Error, Result, SECTOR};

mod allocator;
mod ops;
mod reclaim;

use allocator::{Allocator, Draw, Readers};
pub use ops::{Link, Removed, Rename};
pub(crate) use reclaim::Order;
#[cfg(test)]
pub(crate) use reclaim::Plan;
use reclaim::{fresh_cost, node_ptr, MAX_GROUPS, RECLAIM_ROOM, REWRITTEN};

/// How much a mount may hold in memory.
#[derive(Clone, Copy, Debug)]
pub struct MountOptions {
    /// Btree nodes cached. Dirty and unpointed nodes are bounded well below
    /// it; past it clean nodes are evicted.
    pub cache_nodes: usize,
}

/// u64s one ordinary transaction journals at most: a namespace operation's
/// inode, dirent and accounting keys, names of 512 bytes included.
pub(crate) const K_TX: u64 = 512;
/// Nodes any ordinary transaction dirties at most: the largest grant of
/// the operations' (`mount/ops.rs`).
pub(crate) const FOOTPRINT: usize = 10;
/// Nodes one reclaim batch writes.
const BATCH: usize = 2;
/// u64s a reclaim share holds for each node that can pin the journal:
/// sized for a rewrite to one node, the common case. A share too short for
/// the splits a batch needs stops the mount by name ([`Mount::admit`]).
const PER_NODE: u64 = fresh_cost(1);
/// Cache room past the limit that only reclaim takes: what its transaction
/// dirties, and the nodes a batch's rewrites replace its nodes with.
const RECLAIM: usize = RECLAIM_ROOM + REWRITTEN;
/// The smallest btree node this writer writes. Past its first bset a
/// smaller one has room for about two whole-block appends, and reclaim's own
/// changes to the allocator's leaves then rewrite more nodes than they
/// settle ([`MAX_GROUPS`] needs less).
const MIN_NODE: usize = 32 << 10;
/// Retries of a failed node or journal write, each of the same bytes.
const RETRIES: usize = 2;
/// Feature bits a volume carries once written read-write (A1.41).
const RW_FEATURES: u64 = 1 << 9 | 1 << 12 | 1 << 13 | 1 << 17 | 1 << 18;
/// Feature bit 5: the superblock holds a blacklist.
const BLACKLIST_FEATURE: u64 = 1 << 5;

/// What a new node is.
pub enum New {
    File,
    Dir,
    /// Its target is file data, which B7's data path writes.
    Symlink(Vec<u8>),
}

/// A new node's owner and permission bits.
#[derive(Clone, Copy, Debug)]
pub struct Owner {
    /// Mode bits below the file type, at most 0o7777.
    pub perm: u16,
    pub uid: u32,
    pub gid: u32,
}

/// The mount's state, changed only between awaits.
struct State {
    header: Header,
    fields: Vec<Field>,
    member: Member,
    blacklist: Vec<(u64, u64)>,
    cache: Cache,
    roots: BTreeMap<Btree, Root>,
    /// Btrees given a root in memory, not yet written.
    new_roots: BTreeMap<Btree, NodeId>,
    journal: Writer,
    key_version: u64,
    clocks: [u64; 2],
    /// Seconds since the epoch of the newest operation: each entry's
    /// datetime.
    now: u64,
    /// Cache room granted to admitted ordinary transactions, and reclaim's.
    granted: usize,
    reclaim_granted: usize,
    /// Dirty and unpointed nodes and outstanding ordinary grants together
    /// stay at most this.
    limit: usize,
    /// Cache room only reclaim takes.
    reclaim_room: usize,
    /// After a crash, the first sequence this mount writes: a node read
    /// whose newest bset is older is from before the crash, and its next
    /// write goes to a fresh node (§3.5). 0 after a clean mount.
    fresh_below: u64,
    /// Superblock copies a crash left unreadable, rewritten first so that
    /// a crash in the next rewrite still leaves two whole (§3.4).
    torn_copies: Vec<u64>,
    /// Where new nodes go, and the room left for them.
    alloc: Allocator,
    /// Why the mount stopped writing, once it has.
    read_only: Option<&'static str>,
    /// The most dirty and unpointed nodes ever held together.
    #[cfg(test)]
    peak: usize,
    /// Ordinary admissions that waited for journal space, and for cache
    /// room, as tests construct them.
    #[cfg(test)]
    waits: [usize; 2],
}

/// A volume mounted read-write.
pub struct Mount<D> {
    dev: D,
    /// What nodes are read under: geometry, encryption and the blacklist,
    /// none of which changes while mounted.
    sb: Superblock,
    key: Option<MasterKey>,
    random: RefCell<Box<dyn Random>>,
    st: RefCell<State>,
    slot_held: Cell<bool>,
    slot: Queue,
    slot_reclaim: Queue,
    admit: Queue,
    admit_reclaim: Queue,
    journal_held: Cell<bool>,
    journal_queue: Queue,
    /// Node writes completing, and traversals ending, watched by a prepare
    /// waiting on a node and by reclaim waiting on a bucket.
    written: Broadcast,
    readers: RefCell<Readers>,
    root: u64,
}

const _: () = assert!(PER_NODE <= K_TX, "a reclaim batch of one node fits one reservation");
/// The largest key a node holds, as its reader refuses a larger value
/// (A3.16).
const MAX_KEY: usize = 8 * (KEY_U64S + super::bkey::MAX_VALUE_U64S);
// A node read at five times its bytes unpacked, and two transactions more,
// spread evenly over [`MAX_GROUPS`] nodes: each takes its share and at most
// a key past it.
const _: () = assert!((5 * (MIN_NODE - FIRST_KEYS) + 2 * K_TX as usize * 8).div_ceil(MAX_GROUPS) + MAX_KEY <= MIN_NODE - FIRST_KEYS, "seven nodes hold a node read unpacked and two transactions");

/// The writer slot, held until dropped.
struct Slot<'a, D>(&'a Mount<D>);

impl<D> Drop for Slot<'_, D> {
    fn drop(&mut self) {
        self.0.slot_held.set(false);
        self.0.slot_reclaim.wake();
        self.0.slot.wake();
    }
}

/// A transaction's changes, built in prepare and installed at apply.
#[derive(Default)]
struct Txn {
    /// Keys set, or deleted as `None`, by btree, level and position.
    updates: BTreeMap<(Btree, u8, Bpos), Replayed>,
    /// Accounting deltas by position, two's complement.
    accounting: BTreeMap<Bpos, Vec<u64>>,
    /// Inodes whose journal-sequence field takes the transaction's (A5.14).
    stamped: Vec<Bpos>,
    /// Buckets left empty: their alloc keys and need_discard entries take
    /// the transaction's sequence (A6.18).
    emptied: Vec<(u64, Bucket)>,
    /// Buckets taken that it fills.
    filled: Vec<u64>,
    /// New roots.
    roots: Vec<Root>,
    /// Written nodes made reachable, installed in the cache at apply.
    installs: Vec<Install>,
    /// Nodes held from prepare to apply.
    held: Vec<NodeId>,
    /// The nodes it changes at apply: those its positions were held in.
    dirtied: BTreeSet<NodeId>,
}

/// A written node, made reachable by the transaction that holds it.
enum Install {
    /// `id`'s appended bset, now `sectors` long in all.
    Append { id: NodeId, sectors: u16 },
    /// `id` replaced by the nodes written for it, and a root above them when
    /// a root split.
    Replace { id: NodeId, by: Vec<Planned>, root: Option<Planned> },
}

/// A new node as planned and written.
#[derive(Clone)]
struct Planned {
    btree: Btree,
    level: u8,
    min: Bpos,
    max: Bpos,
    seq: u64,
    sector: u64,
    gen: u8,
    sectors: u16,
    keys: Vec<(Key, Vec<u8>)>,
}

impl Planned {
    fn ptr(&self) -> NodePtr {
        NodePtr { seq: self.seq, written: self.sectors, min: self.min, ptr: node_ptr(self.sector, self.gen) }
    }
}

/// Each root's btree, level and pointer key, encoded, as records carry them.
fn encoded_roots(roots: &BTreeMap<Btree, Root>) -> Vec<(Btree, u8, Vec<u8>)> {
    roots.values().map(|r| (r.btree, r.level, r.key.encode(&r.ptr.encode()))).collect()
}

/// Refuses a volume this writer does not handle in full, by name (§3.1).
fn writable(sb: &Superblock) -> Result<()> {
    // Compression, erasure coding, reflink, reflinked inline data,
    // casefolding, and the three that change how a mount runs (A1.17).
    const UNHANDLED: u64 = 0b111 | 1 << 4 | 1 << 6 | 1 << 14 | 0xf << 20;
    if sb.header.version != super::VERSION {
        return Err(Error::Unsupported("writing a volume of a version other than 1.39"));
    }
    if sb.header.features[0] & UNHANDLED != 0 {
        return Err(Error::Unsupported("writing a volume with a feature ToyOS does not write"));
    }
    if sb.header.compat[0] & !0x3f != 0 || sb.header.compat[1] != 0 {
        return Err(Error::Unsupported("writing a volume with a compat bit v1.39.4 does not define"));
    }
    let ext = sb.field(field_type::EXT).unwrap_or(&[]);
    if ext.get(..16).is_some_and(|r| r.iter().any(|&b| b != 0)) {
        return Err(Error::Unsupported("writing a volume whose superblock requires recovery passes"));
    }
    if sb.journal.is_empty() {
        return Err(Error::Unsupported("writing a volume without journal buckets"));
    }
    if (sb.node_sectors() as usize) * SECTOR < MIN_NODE {
        return Err(Error::Unsupported("writing a volume of btree nodes under 32 KiB"));
    }
    // Every write is of whole device blocks, so entries, bsets and nodes
    // must fall on them.
    let (block, node, bucket) = (u64::from(sb.header.block_sectors), u64::from(sb.node_sectors()), u64::from(sb.member.bucket_sectors));
    if !(block * SECTOR as u64).is_multiple_of(BLOCK as u64) {
        return Err(Error::Unsupported("writing a volume of blocks under 4 KiB"));
    }
    if !node.is_multiple_of(block) || !bucket.is_multiple_of(node) {
        return Err(Error::Unsupported("writing a volume whose nodes are not whole blocks or buckets not whole nodes"));
    }
    // Fields of other shapes than those this writer keeps: the legacy
    // journal, members, quota, disk groups and errors, and replicas with a
    // required count past 1; or a type v1.39.4 does not define (A1.19).
    if sb.fields.iter().any(|f| matches!(f.kind, 0 | 1 | 4 | 5 | 7 | 12) || f.kind > field_type::ERRORS_V2) {
        return Err(Error::Unsupported("writing a volume with a superblock field ToyOS does not keep"));
    }
    // v1.39.7's write-degraded action (A1.47).
    if sb.flag(6, 60, 2) != 0 {
        return Err(Error::Unsupported("writing a volume with a flag bit v1.39.4 does not define"));
    }
    let members = sb.field(field_type::MEMBERS_V2).unwrap_or(&[]);
    if members.len() < 2 || usize::from(super::le16(members, 0)) < Member::BYTES {
        return Err(Error::Unsupported("writing a volume whose member entry is shorter than 1.39's"));
    }
    Ok(())
}

impl<D: BlockDev> Mount<D> {
    /// An unencrypted volume, or one formatted without a passphrase,
    /// mounted read-write; new nodes' sequences are drawn from `random`.
    /// One not cleanly unmounted is replayed first, and its journal resumes
    /// past the post-crash skip.
    pub async fn open(dev: D, options: MountOptions, random: Box<dyn Random>) -> Result<Self> {
        writable(&read::read_superblock(&dev).await?)?;
        let volume = Volume::open(dev).await?;
        no_logged_ops(&volume).await?;
        let empty = allocator::empty_buckets(&volume).await?;
        Self::from(volume.into_parts(), empty, options, random).await
    }

    /// An encrypted volume, unlocked as [`Volume::unlock`] does, mounted
    /// read-write.
    pub async fn unlock(dev: D, passphrase: &[u8], limits: scrypt::Limits, options: MountOptions, random: Box<dyn Random>) -> Result<Self> {
        writable(&read::read_superblock(&dev).await?)?;
        let volume = Volume::unlock(dev, passphrase, limits).await?;
        no_logged_ops(&volume).await?;
        let empty = allocator::empty_buckets(&volume).await?;
        Self::from(volume.into_parts(), empty, options, random).await
    }

    /// `empty` is the volume's count of empty buckets.
    async fn from(parts: Parts<D>, empty: u64, options: MountOptions, random: Box<dyn Random>) -> Result<Self> {
        let Parts { dev, mut sb, key, roots, key_version, replay, root } = parts;
        let ring: Vec<u64> = sb.journal.iter().flat_map(|&(first, count)| first..first + count).collect();
        let bucket_sectors = u64::from(sb.member.bucket_sectors);
        let mut journal = Writer::new(ring, bucket_sectors, sb.block_bytes(), K_TX);
        let mut blacklist = sb.blacklist.clone();
        let (clocks, restate) = match &replay {
            None => {
                let clean = sb.clean.as_ref().expect("a volume opens clean only with its clean field");
                let m = &sb.member;
                let (head, used) = (m.journal_bucket as usize, u64::from(m.journal_offset));
                if head >= journal.ring.len() || used > bucket_sectors || !used.is_multiple_of(u64::from(sb.header.block_sectors)) {
                    return Err(Error::Corrupt("member's journal position past its ring or off a block"));
                }
                journal.head = head;
                journal.head_sectors = used;
                // The entry `clean` names ends where writing resumes (A1.42):
                // in the head bucket, or, at a bucket's start, in the one
                // before it. A journal may also hold none (A1.43). The
                // position alone is not trusted to say which.
                let ends = |found: &[journal::Found], at: Option<u64>| found.iter().any(|e| e.good && e.flush && e.seq == clean.journal_seq && at.is_none_or(|at| e.offset + e.sectors == at));
                let holding = if used > 0 {
                    ends(&journal::scan_bucket(&dev, &sb, key.as_ref(), head).await?, Some(used)).then_some(head)
                } else {
                    let before = (head + journal.ring.len() - 1) % journal.ring.len();
                    ends(&journal::scan_bucket(&dev, &sb, key.as_ref(), before).await?, None).then_some(before)
                };
                if let Some(bucket) = holding.filter(|_| clean.journal_seq > 0) {
                    journal.newest[bucket] = Some(clean.journal_seq);
                }
                journal.durable_last = clean.journal_seq;
                journal.next_seq = clean.journal_seq + 1;
                journal.durable = clean.journal_seq;
                (clean.clocks, holding.is_none() || clean.journal_seq == 0)
            }
            Some((window, replayed)) => {
                let scan = journal::scan(&dev, &sb, key.as_ref()).await?;
                journal.durable_last = window.first;
                for e in scan.entries.values().filter(|e| e.seq >= window.first) {
                    let newest = &mut journal.newest[e.bucket];
                    *newest = Some(newest.map_or(e.seq, |n| n.max(e.seq)));
                }
                // Writing resumes after the newest entry found (A2.10).
                let newest = scan.entries.values().next_back().expect("a window has entries");
                journal.head = newest.bucket;
                journal.head_sectors = newest.offset + newest.sectors;
                // Past every sequence the medium may hold, which the
                // blacklist covers from the window's end (A2.15).
                let next = window.next.checked_add(SKIP).ok_or(Error::Corrupt("a journal sequence too near its last value to skip past"))?.max(blacklist.last().map_or(0, |b| b.1));
                blacklist_add(&mut blacklist, window.last + 1, next);
                journal.next_seq = next;
                journal.durable = window.last;
                (replayed.clocks, false)
            }
        };
        sb.blacklist = blacklist.clone();

        let reclaim_room = RECLAIM;
        // Reclaim's two shares of the ring each hold the pointer updates of
        // every node that can pin it, and leave transactions a quarter.
        let per_entry = journal.body() - journal.max_tx;
        let share = journal.slots() * 3 / 8;
        let by_journal = ((share - 1) * per_entry / PER_NODE) as usize;
        let limit = options.cache_nodes.saturating_sub(reclaim_room + 2 * FOOTPRINT).min(by_journal.saturating_sub(reclaim_room));
        if limit < FOOTPRINT {
            return Err(Error::Invalid("a cache or journal too small for a transaction and its reclaim"));
        }
        journal.reclaim_entries = ((limit + reclaim_room) as u64 * PER_NODE).div_ceil(per_entry) + 1;

        let journal_next = journal.next_seq;
        let mut torn_copies = Vec::new();
        for &offset in &sb.header.layout.offsets {
            if read::read_copy(&dev, offset).await.is_err() {
                torn_copies.push(offset);
            }
        }
        let state = State {
            header: sb.header.clone(),
            fields: sb.fields.clone(),
            member: sb.member.clone(),
            blacklist,
            cache: Cache::new(options.cache_nodes, sb.node_sectors() as usize * SECTOR - FIRST_KEYS),
            roots: roots.into_iter().map(|r| (r.btree, r)).collect(),
            new_roots: BTreeMap::new(),
            journal,
            key_version,
            clocks,
            now: sb.header.write_time,
            granted: 0,
            reclaim_granted: 0,
            limit,
            reclaim_room,
            fresh_below: if replay.is_some() { journal_next } else { 0 },
            torn_copies,
            alloc: Allocator::new(u64::from(sb.member.bucket_sectors) / u64::from(sb.node_sectors()), empty),
            read_only: None,
            #[cfg(test)]
            peak: 0,
            #[cfg(test)]
            waits: [0; 2],
        };
        let mount = Mount {
            dev,
            sb,
            key,
            random: RefCell::new(random),
            st: RefCell::new(state),
            slot_held: Cell::new(false),
            slot: Queue::default(),
            slot_reclaim: Queue::default(),
            admit: Queue::default(),
            admit_reclaim: Queue::default(),
            journal_held: Cell::new(false),
            journal_queue: Queue::default(),
            written: Broadcast::default(),
            readers: RefCell::default(),
            root,
        };
        mount.go_read_write(restate).await?;
        if let Some((_, replayed)) = replay {
            mount.install_replay(replayed).await?;
        }
        Ok(mount)
    }

    /// Writes the superblock as a volume being written carries it: not
    /// clean, the read-write features set, the journal's replicas entry
    /// beside the btree's (A1.41, A2.23), and the blacklist.
    ///
    /// With `restate`, a clean volume's journal holds no entry `clean`
    /// names, so a crash once it is marked unclean would leave nothing to
    /// replay (A9.11). The next entry `S` is closed, so its place, the head
    /// bucket's start, and its size are known; the superblock, still clean,
    /// names `S` with the member's position after it, which an empty
    /// journal accepts (A1.43); then `S` is written, as a clean shutdown
    /// writes its last (A1.42). Each step leaves a valid clean volume, and a
    /// mount after a crash between them restates again.
    async fn go_read_write(&self, restate: bool) -> Result<()> {
        if restate {
            let (seq, clean) = {
                let mut st = self.st.borrow_mut();
                let st = &mut *st;
                st.journal.open_new();
                self.close(st, true);
                let (seq, sectors) = st.journal.closed.back().map(|e| (e.seq, (e.bytes.len() / SECTOR) as u64)).expect("just closed");
                // `S` goes at the head bucket's start: an earlier restate's
                // `S` may never have been written, and a scan stops at an
                // empty bucket start.
                st.journal.head_sectors = 0;
                st.journal.place(sectors);
                st.member.journal_bucket = st.journal.head as u32;
                st.member.journal_offset = (st.journal.head_sectors + sectors) as u32;
                let roots = encoded_roots(&st.roots);
                (seq, journal::clean_body(seq, roots.iter().map(|(b, l, k)| (*b, *l, &k[..])), st.key_version, st.clocks))
            };
            self.write_superblock(Some(clean)).await?;
            self.write_entries(seq).await?;
        }
        {
            let mut st = self.st.borrow_mut();
            st.header.flags[0] &= !(1 << 1);
            st.header.features[0] |= RW_FEATURES;
            if !st.blacklist.is_empty() {
                st.header.features[0] |= BLACKLIST_FEATURE;
            }
        }
        self.write_superblock(None).await
    }

    /// Every superblock copy rewritten from the state, one sequence on, each
    /// flushed before the next is written, a copy a crash tore first: a
    /// crash tears at most the one being written, and the reader takes the
    /// newest whole copy. `clean` is the clean field a clean unmount writes;
    /// without it the journal's replicas entry is listed.
    async fn write_superblock(&self, clean: Option<Vec<u8>>) -> Result<()> {
        let copies: Vec<(u64, Vec<u8>)> = {
            let mut st = self.st.borrow_mut();
            st.header.seq += 1;
            st.member.seq = st.header.seq;
            st.header.write_time = st.now;
            let mut members = st.fields.iter().find(|f| f.kind == field_type::MEMBERS_V2).expect("a superblock opens only with its members").body.clone();
            st.member.patch(&mut members[8..]);
            let blacklist: Vec<u8> = st.blacklist.iter().flat_map(|&(a, b)| [a.to_le_bytes(), b.to_le_bytes()]).flatten().collect();
            let mut replicas = Vec::new();
            if clean.is_none() {
                replicas.extend_from_slice(&[data_type::JOURNAL, 1, 0]);
            }
            replicas.extend_from_slice(&[data_type::BTREE, 1, 0]);
            let mut fields = core::mem::take(&mut st.fields);
            set_field(&mut fields, field_type::MEMBERS_V2, Some(members));
            set_field(&mut fields, field_type::BLACKLIST, (!blacklist.is_empty()).then_some(blacklist));
            set_field(&mut fields, field_type::REPLICAS_V0, Some(replicas));
            if let Some(clean) = clean {
                set_field(&mut fields, field_type::CLEAN, Some(clean));
            }
            let mut offsets = st.header.layout.offsets.clone();
            offsets.sort_by_key(|o| !st.torn_copies.contains(o));
            let copies = offsets.into_iter().map(|o| (o, Superblock::encode(&st.header, &fields, o))).collect();
            st.fields = fields;
            copies
        };
        for (offset, bytes) in copies {
            mkfs::write(&self.dev, offset, &bytes).await.inspect_err(|_| self.stop("superblock write failed"))?;
            self.dev.flush().await.inspect_err(|_| self.stop("superblock flush failed"))?;
            self.st.borrow_mut().torn_copies.retain(|&o| o != offset);
        }
        Ok(())
    }

    /// Stops all writing, for `why`: what the medium holds stays a state a
    /// later mount replays.
    fn stop(&self, why: &'static str) {
        let mut st = self.st.borrow_mut();
        st.read_only.get_or_insert(why);
        drop(st);
        self.admit.wake();
        self.admit_reclaim.wake();
        self.written.wake();
    }

    fn check_writable(&self) -> Result<()> {
        match self.st.borrow().read_only {
            Some(why) => Err(Error::ReadOnly(why)),
            None => Ok(()),
        }
    }
}

/// Refuses a volume holding a logged operation, which a mount must resume
/// before writing (A8.17, A8.18) and this one cannot yet.
async fn no_logged_ops<D: BlockDev>(volume: &Volume<D>) -> Result<()> {
    let mut found = false;
    Source::walk(volume, Btree::LoggedOps, Bpos::MIN, |k, _| {
        found = k.pos.inode == 0;
        Ok(false)
    })
    .await?;
    if found {
        return Err(Error::Unsupported("writing a volume with a logged operation to resume"));
    }
    Ok(())
}

/// Adds `[start, end)` to a sorted blacklist, merging what it overlaps or
/// touches (A1.38).
fn blacklist_add(list: &mut Vec<(u64, u64)>, start: u64, end: u64) {
    if start >= end {
        return;
    }
    let (mut start, mut end) = (start, end);
    list.retain(|&(a, b)| {
        let apart = b < start || a > end;
        if !apart {
            start = start.min(a);
            end = end.max(b);
        }
        apart
    });
    let at = list.partition_point(|&(a, _)| a < start);
    list.insert(at, (start, end));
}

/// Sets the field of `kind` to `body`, or removes it, keeping the others'
/// order.
fn set_field(fields: &mut Vec<Field>, kind: u32, body: Option<Vec<u8>>) {
    match (fields.iter().position(|f| f.kind == kind), body) {
        (Some(i), Some(body)) => fields[i].body = body,
        (Some(i), None) => {
            fields.remove(i);
        }
        (None, Some(body)) => fields.push(Field { kind, body }),
        (None, None) => {}
    }
}

/// Nodes: read, found, held and created.
impl<D: BlockDev> Mount<D> {
    /// The cached node `ptr` points at, read from the medium when it is
    /// not cached.
    async fn load(&self, btree: Btree, level: u8, ptr: &NodePtr, max: Bpos) -> Result<NodeId> {
        // The sector finds a cached node; the pointer must still be to it,
        // as two pointers to one sector on a hostile volume are not.
        let cached = |st: &mut State| {
            st.cache.at_sector(ptr.ptr.sector).map(|id| {
                let n = st.cache.node(id);
                if (n.btree, n.level, n.seq, n.min, n.max) == (btree, level, ptr.seq, ptr.min, max) {
                    Ok(id)
                } else {
                    Err(Error::Corrupt("two btree pointers to one node"))
                }
            })
        };
        if let Some(found) = cached(&mut self.st.borrow_mut()) {
            return found;
        }
        let node = read::read_node(&self.dev, &self.sb, self.key.as_ref(), btree, level, ptr, max).await.inspect_err(|e| {
            // Writing on would rewrite a node holding what this cannot read
            // (§3.1).
            if let Error::Unsupported(why) = *e {
                self.stop(why);
            }
        })?;
        let mut st = self.st.borrow_mut();
        if let Some(found) = cached(&mut st) {
            return found;
        }
        let fresh = node.bset_seqs.iter().max().is_none_or(|&s| s < st.fresh_below);
        Ok(st.cache.insert(Cached {
            btree,
            level,
            min: ptr.min,
            max,
            seq: ptr.seq,
            sector: ptr.ptr.sector,
            gen: ptr.ptr.gen,
            written: ptr.written,
            on_disk: ptr.written,
            bsets: node.bset_seqs.len() as u16,
            bytes: node.keys.iter().map(|(_, v)| key_bytes(v)).sum(),
            keys: Rc::new(node.keys),
            unwritten: BTreeMap::new(),
            captured: None,
            writing: false,
            fresh,
            held: 0,
            used: 0,
        }))
    }

    /// `btree`'s root node, cached; `None` when it has none.
    async fn root_node(&self, btree: Btree) -> Result<Option<NodeId>> {
        let root = {
            let st = self.st.borrow();
            if let Some(&id) = st.new_roots.get(&btree) {
                return Ok(Some(id));
            }
            st.roots.get(&btree).cloned()
        };
        match root {
            Some(r) => Ok(Some(self.load(btree, r.level, &r.ptr, r.key.pos).await?)),
            None => Ok(None),
        }
    }

    /// The node at `level` of `btree` spanning `pos`, cached. A btree with
    /// no root is given an empty one, in memory until it is first written.
    /// Nothing holds it: for a caller that changes it before its next await.
    async fn find(&self, btree: Btree, level: u8, pos: Bpos) -> Result<NodeId> {
        let mut id = match self.root_node(btree).await? {
            Some(id) => id,
            None => self.new_root(btree),
        };
        loop {
            let (child, ptr, max) = {
                let st = self.st.borrow();
                let n = st.cache.node(id);
                if n.level == level {
                    return Ok(id);
                }
                if n.level < level {
                    return Err(Error::Corrupt("a key above its btree's root"));
                }
                let (key, value) = n.keys.get(n.at(pos)).ok_or(Error::Corrupt("interior node short of its range"))?;
                (n.level - 1, NodePtr::decode(value)?, key.pos)
            };
            id = self.load(btree, child, &ptr, max).await?;
        }
    }

    /// An empty leaf root for `btree`, never written: what a btree without a
    /// root holds until its first write (A2.19).
    fn new_root(&self, btree: Btree) -> NodeId {
        let mut st = self.st.borrow_mut();
        // A sector no node has: the first write gives it a real one.
        let sector = u64::MAX - btree as u64;
        let id = st.cache.insert(Cached {
            btree,
            level: 0,
            min: Bpos::MIN,
            max: Bpos::MAX,
            seq: 0,
            sector,
            gen: 0,
            written: 0,
            on_disk: 0,
            bsets: 0,
            bytes: 0,
            keys: Rc::new(Vec::new()),
            unwritten: BTreeMap::new(),
            captured: None,
            writing: false,
            fresh: true,
            held: 0,
            used: 0,
        });
        st.new_roots.insert(btree, id);
        id
    }

    /// Holds the node at `level` of `btree` spanning `pos`, and its path,
    /// for `txn`'s apply, once no write of it is in flight. Each node is held
    /// as the descent reaches it, so loading the next cannot evict it.
    async fn hold(&self, txn: &mut Txn, btree: Btree, level: u8, pos: Bpos) -> Result<NodeId> {
        let mut id = match self.root_node(btree).await? {
            Some(id) => id,
            None => self.new_root(btree),
        };
        loop {
            let next = {
                let mut st = self.st.borrow_mut();
                st.cache.change(id, |n| n.held += 1);
                txn.held.push(id);
                let n = st.cache.node(id);
                if n.level == level {
                    None
                } else if n.level < level {
                    return Err(Error::Corrupt("a key above its btree's root"));
                } else {
                    let (key, value) = n.keys.get(n.at(pos)).ok_or(Error::Corrupt("interior node short of its range"))?;
                    Some((n.level - 1, NodePtr::decode(value)?, key.pos))
                }
            };
            match next {
                Some((child, ptr, max)) => id = self.load(btree, child, &ptr, max).await?,
                None => break,
            }
        }
        // A write that failed stopped the mount: the node stays writing.
        self.written.until(|| self.st.borrow().read_only.is_some() || !self.st.borrow().cache.node(id).writing).await;
        self.check_writable()?;
        txn.dirtied.insert(id);
        Ok(id)
    }

    /// Sets `pos` at `level` of `btree` in `txn`, holding its node.
    async fn put(&self, txn: &mut Txn, btree: Btree, level: u8, pos: Bpos, kv: Option<(Key, Vec<u8>)>) -> Result<()> {
        self.hold(txn, btree, level, pos).await?;
        txn.updates.insert((btree, level, pos), kv);
        Ok(())
    }

    /// Adds `counters` to the accounting key of `record`, holding its node.
    async fn account(&self, txn: &mut Txn, record: &[u8], counters: &[u64]) -> Result<()> {
        let pos = accounting_pos(record);
        self.hold(txn, Btree::Accounting, 0, pos).await?;
        let sums = txn.accounting.entry(pos).or_insert_with(|| alloc::vec![0; counters.len()]);
        for (sum, c) in sums.iter_mut().zip(counters) {
            *sum = sum.wrapping_add(*c);
        }
        Ok(())
    }

    /// A random nonzero 64-bit value: a node sequence, a hash seed.
    fn nonzero(&self) -> Result<u64> {
        let mut random = self.random.borrow_mut();
        loop {
            let mut b = [0u8; 8];
            random.fill(&mut b).map_err(|_| Error::Entropy)?;
            let v = u64::from_le_bytes(b);
            if v != 0 {
                return Ok(v);
            }
        }
    }

    /// The key at `pos` in `btree`'s leaves as `txn` would leave it.
    async fn get_in(&self, txn: &Txn, btree: Btree, pos: Bpos) -> Result<Option<(Key, Vec<u8>)>> {
        if let Some(kv) = txn.updates.get(&(btree, 0, pos)) {
            return Ok(kv.clone());
        }
        Source::get(self, btree, pos).await
    }

    /// The first key of `btree`'s leaves at or after `pos` as `txn` would
    /// leave them.
    async fn first_in(&self, txn: &Txn, btree: Btree, pos: Bpos) -> Result<Option<(Key, Vec<u8>)>> {
        let mut stored = None;
        Source::walk(self, btree, pos, |k, v| {
            let deleted = matches!(txn.updates.get(&(btree, 0, k.pos)), Some(None));
            if !deleted {
                stored = Some((*k, v.to_vec()));
            }
            Ok(deleted)
        })
        .await?;
        let upto = stored.as_ref().map_or(Bpos::MAX, |(k, _)| k.pos);
        let pending = txn.updates.range((btree, 0, pos)..=(btree, 0, upto)).find_map(|(_, kv)| kv.clone());
        Ok(pending.or(stored))
    }
}

impl State {
    /// Whether writing the journal now would free a bucket of its ring: the
    /// last sequence an entry closed now carries passes some bucket's newest.
    fn journal_frees(&self) -> bool {
        let j = &self.journal;
        let next = j.open.as_ref().map_or(j.next_seq, |o| o.seq);
        let last = self.cache.oldest_pin().map_or(next, |p| p.min(next));
        j.newest.iter().enumerate().any(|(i, n)| i != j.head && n.is_some_and(|n| n < last))
    }
}

/// Ends the holds on `ids`.
fn release(st: &mut State, ids: &[NodeId]) {
    for &id in ids {
        st.cache.change(id, |n| n.held -= 1);
    }
}

/// The cached node at `level` of `btree` spanning `pos`, which a hold keeps
/// in memory.
fn held(st: &State, btree: Btree, level: u8, pos: Bpos) -> NodeId {
    let mut id = match st.new_roots.get(&btree) {
        Some(&id) => id,
        None => st.cache.id_at(st.roots[&btree].ptr.ptr.sector).expect("a held root in the cache"),
    };
    loop {
        let n = st.cache.node(id);
        if n.level == level {
            return id;
        }
        let (_, value) = &n.keys[n.at(pos)];
        let ptr = NodePtr::decode(value).expect("a pointer the cache holds decodes");
        id = st.cache.id_at(ptr.ptr.sector).expect("a held node in the cache");
    }
}

impl<D: BlockDev> Source for Mount<D> {
    type Dev = D;

    fn dev(&self) -> &D {
        &self.dev
    }

    fn superblock(&self) -> &Superblock {
        &self.sb
    }

    fn master_key(&self) -> Option<&MasterKey> {
        self.key.as_ref()
    }

    fn root_dir(&self) -> u64 {
        self.root
    }

    fn root_of(&self, btree: Btree) -> Option<Root> {
        let st = self.st.borrow();
        if st.new_roots.contains_key(&btree) {
            return None;
        }
        st.roots.get(&btree).cloned()
    }

    fn rootless(&self, btree: Btree) -> Result<Vec<(Key, Vec<u8>)>> {
        let st = self.st.borrow();
        Ok(st.new_roots.get(&btree).map_or_else(Vec::new, |&id| st.cache.node(id).keys.to_vec()))
    }

    async fn keys(&self, btree: Btree, level: u8, ptr: &NodePtr, max: Bpos) -> Result<Keys> {
        let id = self.load(btree, level, ptr, max).await?;
        Ok(Rc::clone(&self.st.borrow().cache.node(id).keys))
    }

    fn begin_read(&self) -> u64 {
        self.readers.borrow_mut().begin()
    }

    fn end_read(&self, ticket: u64) {
        if self.readers.borrow_mut().end(ticket) {
            self.written.wake();
        }
    }
}

/// Admission, the slot, apply and the journal.
impl<D: BlockDev> Mount<D> {
    /// Takes `journal` u64s of journal space and room for `nodes` dirtied
    /// nodes from `class`'s share, first in, first out. The writer at the
    /// head of the ordinary queue, holding nothing, frees what it lacks
    /// itself: it writes closed entries, reclaims, or writes the journal so
    /// its last sequence moves on, keeping its place; with nothing to free
    /// it waits for what is granted to apply. Reclaim's own share never waits
    /// on reclaim: past its closed entries, a short share is a bug, and the
    /// mount stops.
    async fn admit(&self, class: Class, draw: Draw, journal: u64, nodes: usize) -> Result<()> {
        let queue = match class {
            Class::Ordinary => &self.admit,
            Class::Reclaim => &self.admit_reclaim,
        };
        let place = queue.enter();
        loop {
            let granted = place
                .until(|| match self.grant(class, draw, journal, nodes) {
                    Ok(true) => Some(Ok(true)),
                    Err(e) => Some(Err(e)),
                    Ok(false) if self.can_free() => Some(Ok(false)),
                    Ok(false) => None,
                })
                .await?;
            if granted {
                return Ok(());
            }
            match class {
                // Boxed: reclaim takes its own admission.
                Class::Ordinary => Box::pin(self.free()).await?,
                Class::Reclaim => {
                    self.flush_closed().await?;
                    if !self.grant(class, draw, journal, nodes)? {
                        self.stop("journal reclaim's share exhausted");
                        return Err(Error::ReadOnly("journal reclaim's share exhausted"));
                    }
                    return Ok(());
                }
            }
        }
    }

    /// Grants `class` its request now if it fits. An ordinary one that the
    /// room left could not hold even with nothing else dirty is refused
    /// `NoSpace`, once nothing written or emptied can change that.
    fn grant(&self, class: Class, draw: Draw, journal: u64, nodes: usize) -> Result<bool> {
        let mut st = self.st.borrow_mut();
        if let Some(why) = st.read_only {
            return Err(Error::ReadOnly(why));
        }
        let room = match class {
            Class::Ordinary => {
                let held = st.cache.dirty + st.cache.unpointed + st.granted;
                if !st.alloc.fits(nodes, st.reclaim_room, draw) && held == 0 && st.reclaim_granted == 0 && !st.alloc.returning() {
                    return Err(Error::NoSpace(match draw {
                        Draw::Ordinary => "no room left for the btree nodes a change may write",
                        Draw::Freeing => "no room left for the btree nodes a removal may write",
                    }));
                }
                // Nothing more lands in a node reclaim has yet to split.
                held + nodes <= st.limit && st.cache.oversized == 0 && st.alloc.fits(held + nodes, st.reclaim_room, draw)
            }
            Class::Reclaim => st.reclaim_granted + nodes <= st.reclaim_room,
        };
        if room && st.journal.reserve(journal, class) {
            match class {
                Class::Ordinary => st.granted += nodes,
                Class::Reclaim => st.reclaim_granted += nodes,
            }
            return Ok(true);
        }
        #[cfg(test)]
        if class == Class::Ordinary {
            st.waits[usize::from(!room)] += 1;
        }
        Ok(false)
    }

    /// Whether anything can be freed now: entries to write, a dirty node to
    /// reclaim, or a journal whose last sequence can move on or that holds
    /// an emptying.
    fn can_free(&self) -> bool {
        let st = self.st.borrow();
        let flush = st.journal_frees() || st.alloc.returning();
        !st.journal.closed.is_empty() || (flush && st.journal.may_close()) || st.cache.ids().into_iter().any(|id| {
            let n = st.cache.node(id);
            n.dirty() && !n.writing && n.captured.is_none() && n.held == 0
        })
    }

    /// One step of freeing: closed entries written, else a reclaim batch,
    /// else the open entry written so the journal's last sequence moves on
    /// and what it emptied is free.
    async fn free(&self) -> Result<()> {
        if !self.st.borrow().journal.closed.is_empty() {
            return self.flush_closed().await;
        }
        if self.reclaim_round(Order::OldestPin).await? {
            return Ok(());
        }
        self.flush_open().await.map(|_| ())
    }

    /// Returns what [`Mount::admit`] took.
    fn ungrant(&self, st: &mut State, class: Class, journal: u64, nodes: usize) {
        st.journal.unreserve(journal);
        match class {
            Class::Ordinary => st.granted -= nodes,
            Class::Reclaim => st.reclaim_granted -= nodes,
        }
        self.admit_reclaim.wake();
        self.admit.wake();
    }

    /// The writer slot, reclaim's turn before any other.
    async fn slot(&self, class: Class) -> Slot<'_, D> {
        let queue = match class {
            Class::Ordinary => &self.slot,
            Class::Reclaim => &self.slot_reclaim,
        };
        queue
            .wait(|| {
                let free = !self.slot_held.get() && (class == Class::Reclaim || self.slot_reclaim.is_empty());
                if free {
                    self.slot_held.set(true);
                }
                free.then_some(())
            })
            .await;
        Slot(self)
    }

    /// One ordinary transaction: admitted at `bound` journal u64s and
    /// `nodes` of cache room, drawing on the room left as `draw`, then
    /// `prepare` under the slot, then applied.
    /// It enters the slot only while no changed node is oversized: one that
    /// a transaction before it left so sends it back to wait for reclaim to
    /// split that node ([`MAX_GROUPS`]).
    async fn transact<T>(&self, bound: u64, nodes: usize, draw: Draw, prepare: impl AsyncFnOnce(&mut Txn) -> Result<T>) -> Result<T> {
        assert!(nodes <= FOOTPRINT, "a grant of {nodes} nodes, past the largest any operation takes");
        self.check_writable()?;
        let _slot = loop {
            self.admit(Class::Ordinary, draw, bound, nodes).await?;
            let slot = self.slot(Class::Ordinary).await;
            let mut st = self.st.borrow_mut();
            if st.cache.oversized == 0 {
                break slot;
            }
            self.ungrant(&mut st, Class::Ordinary, bound, nodes);
        };
        let mut txn = Txn::default();
        let out = prepare(&mut txn).await;
        self.finish(txn, out.is_ok(), Class::Ordinary, bound, nodes)?;
        out
    }

    /// Applies `txn` when `apply`, else drops it; either way releases its
    /// holds and its grant. `ReadOnly` when the mount stopped writing while
    /// it was prepared.
    fn finish(&self, txn: Txn, apply: bool, class: Class, bound: u64, nodes: usize) -> Result<()> {
        let mut st = self.st.borrow_mut();
        let st = &mut *st;
        // An ordinary grant is each operation's bound; reclaim's is capped
        // by its room, and a batch past it stops the mount.
        let over = txn.dirtied.len() > nodes;
        assert!(!over || class == Class::Reclaim, "a transaction changing {} nodes past its grant of {nodes}", txn.dirtied.len());
        if over {
            st.read_only.get_or_insert("reclaim dirtying more nodes than its cache room");
        }
        let stopped = st.read_only;
        if apply && stopped.is_none() {
            self.apply(st, txn, bound);
        } else {
            release(st, &txn.held);
        }
        #[cfg(test)]
        {
            st.peak = st.peak.max(st.cache.dirty + st.cache.unpointed);
        }
        assert!(st.cache.dirty + st.cache.unpointed <= st.limit + st.reclaim_room, "dirty and unpointed nodes past the cache's limit and reclaim's room");
        self.ungrant(st, class, bound, nodes);
        self.written.wake();
        match stopped {
            Some(why) if apply => Err(Error::ReadOnly(why)),
            _ => Ok(()),
        }
    }

    /// Installs `txn`: its records appended to the open entry at the sequence
    /// that entry has, then its keys in their nodes, its roots and its
    /// written nodes. No await: what the slot's holder prepared, the cache
    /// holds.
    fn apply(&self, st: &mut State, mut txn: Txn, bound: u64) {
        let installs = core::mem::take(&mut txn.installs);
        let holds = core::mem::take(&mut txn.held);
        let filled = core::mem::take(&mut txn.filled);
        let emptied: Vec<u64> = txn.emptied.iter().map(|&(b, _)| b).collect();
        let size = (records(&stamped(&txn, 0), 0, 0).0.len() / 8) as u64;
        assert!(size <= bound, "a transaction of {size} u64s past its bound of {bound}");
        if !st.journal.fits(size) {
            if st.journal.open.is_some() {
                self.close(st, false);
            }
            st.journal.open_new();
        }
        let open = st.journal.open.as_mut().expect("an entry open");
        let (seq, base) = (open.seq, (open.area.len() / 8) as u64);
        let mut txn = stamped(&txn, seq);
        let (area, versions) = records(&txn, seq, base);
        assert_eq!(area.len() / 8, size as usize, "a transaction's records changed size with its sequence");
        open.area.extend_from_slice(&area);
        for b in filled {
            st.alloc.filled(b);
        }
        for b in emptied {
            st.alloc.emptied(b, seq);
            self.readers.borrow_mut().emptied(b);
        }

        // Every change's node is found before any changes: a pointer update
        // this transaction makes would lead the descent to a node not yet
        // installed.
        let updates: Vec<_> = core::mem::take(&mut txn.updates).into_iter().map(|((btree, level, pos), kv)| (held(st, btree, level, pos), pos, kv)).collect();
        let accounting: Vec<_> = core::mem::take(&mut txn.accounting).into_iter().map(|(pos, delta)| (held(st, Btree::Accounting, 0, pos), pos, delta)).collect();
        for (id, pos, kv) in updates {
            st.cache.change(id, |n| n.put(pos, kv, seq));
        }
        for (id, pos, delta) in accounting {
            let version = versions[&pos];
            st.cache.change(id, |n| {
                let i = n.at(pos);
                let mut sums = match n.keys.get(i) {
                    Some((k, v)) if k.pos == pos => journal::counters(v).expect("a cached accounting key decodes"),
                    _ => Vec::new(),
                };
                sums.resize(sums.len().max(delta.len()), 0);
                for (sum, d) in sums.iter_mut().zip(&delta) {
                    *sum = sum.wrapping_add(*d);
                }
                let key = Key { version, ..Key::new(kind::ACCOUNTING, pos) };
                // A total of zero stays, its version with it, so a replay
                // never adds the delta that made it zero again (A7.23, A7.24).
                n.put(pos, Some((key, value::counters(&sums))), seq);
            });
        }
        for root in core::mem::take(&mut txn.roots) {
            st.roots.insert(root.btree, root);
        }
        // Before an install replaces a node it held.
        release(st, &holds);
        for install in installs {
            self.install(st, install);
        }
    }

    /// Closes the open entry: its last sequence the oldest any node pins,
    /// its tail the roots, datetime, key version and clocks as they stand
    /// now. A pin released by an update a later entry carries cannot move
    /// this entry's window past the keys it held (A2.21).
    ///
    /// **Nor past the newest entry already durable** (A2.24): upstream drops
    /// every entry below a flush entry's last sequence as it reads, torn or
    /// not, so a torn entry naming itself would leave it nothing to replay
    /// (`issues/upstream-bcachefs-recovery-loops-on-a-torn-newest-journal-entry.md`).
    /// Only an entry that must name itself does: `own` is the first after a
    /// clean state, written before the superblock goes unclean (A2.25), and
    /// a clean unmount's last (A1.42).
    fn close(&self, st: &mut State, own: bool) {
        let mut open = st.journal.open.take().expect("an entry open");
        let pinned = st.cache.oldest_pin().map_or(open.seq, |p| p.min(open.seq));
        let last_seq = if own { pinned } else { pinned.min(st.journal.durable) };
        assert!(last_seq >= st.journal.durable_last, "a journal entry's last sequence moved back");
        let roots = encoded_roots(&st.roots);
        journal::tail(&mut open.area, roots.iter().map(|(b, l, k)| (*b, *l, &k[..])), Some(st.now), st.key_version, st.clocks);
        let entry = Jset { seq: open.seq, last_seq, area: &open.area };
        let bytes = journal::encode(&st.header.internal_uuid, self.sb.block_bytes(), self.key.as_ref(), &entry).expect("a journal entry encrypts");
        st.journal.closed.push_back(Closed { seq: open.seq, last_seq, bytes });
    }

    /// Writes every closed entry through `upto`, in order, each after a
    /// flush and followed by one (A2.12), so each is durable once written and
    /// so is every node write that completed before it. An entry naming
    /// itself has its header sector written last, after a flush. A failed
    /// write is reissued with the same bytes, or the mount stops writing.
    async fn write_entries(&self, upto: u64) -> Result<()> {
        self.journal_queue.wait(|| (!self.journal_held.get()).then(|| self.journal_held.set(true))).await;
        let out = async {
            loop {
                let (seq, last_seq, bytes, sector) = {
                    let mut st = self.st.borrow_mut();
                    let Some(e) = st.journal.closed.front() else { return Ok(()) };
                    if e.seq > upto {
                        return Ok(());
                    }
                    let (seq, last_seq, bytes) = (e.seq, e.last_seq, e.bytes.clone());
                    let (sector, _) = st.journal.place((bytes.len() / SECTOR) as u64);
                    (seq, last_seq, bytes, sector)
                };
                // An entry naming itself (A2.25, A1.42) would, torn, drop
                // every entry below it as upstream reads (A2.T1): its header
                // goes last, so a crash leaves it whole or not there at all.
                let header_last = last_seq == seq;
                self.reissue("journal write failed", async || {
                    self.dev.flush().await?;
                    if header_last {
                        let mut headless = bytes.clone();
                        headless[..SECTOR].fill(0);
                        mkfs::write(&self.dev, sector, &headless).await?;
                        self.dev.flush().await?;
                        mkfs::write(&self.dev, sector, &bytes[..BLOCK]).await?;
                    } else {
                        mkfs::write(&self.dev, sector, &bytes).await?;
                    }
                    self.dev.flush().await?;
                    Ok(())
                })
                .await?;
                let mut st = self.st.borrow_mut();
                st.journal.closed.pop_front();
                st.journal.written(seq, last_seq, (bytes.len() / SECTOR) as u64);
                st.alloc.durable_through(seq);
                drop(st);
                self.admit_reclaim.wake();
                self.admit.wake();
            }
        }
        .await;
        self.journal_held.set(false);
        self.journal_queue.wake();
        out
    }

    /// `write`, tried again on failure with the same bytes: a failed write
    /// may have reached the medium, and other bytes under the same nonce
    /// would repeat it (§3.5). Past the retries the mount stops, for `why`.
    async fn reissue(&self, why: &'static str, write: impl AsyncFn() -> Result<()>) -> Result<()> {
        let mut tries = 0;
        loop {
            match write().await {
                Ok(()) => return Ok(()),
                Err(_) if tries < RETRIES => tries += 1,
                Err(e) => {
                    self.stop(why);
                    return Err(e);
                }
            }
        }
    }

    /// Makes every applied transaction durable: the open entry closed, and
    /// every entry through it written.
    pub async fn commit(&self) -> Result<()> {
        self.check_writable()?;
        let upto = loop {
            {
                let mut st = self.st.borrow_mut();
                let st = &mut *st;
                let pending = st.journal.open.is_some();
                if !pending {
                    st.journal.closing = false;
                    break st.journal.closed.back().map_or(0, |e| e.seq);
                }
                if st.journal.may_close() {
                    st.journal.closing = false;
                    self.close(st, false);
                    break st.journal.closed.back().expect("just closed").seq;
                }
                // Reservations counted the open entry's room: they apply
                // first, and no new one counts it.
                st.journal.closing = true;
            }
            self.written.until(|| self.st.borrow().journal.may_close()).await;
        };
        self.write_entries(upto).await
    }

    /// Closes the open entry, whose tail carries roots even when it holds
    /// no keys, and writes everything closed: so the journal's durable last
    /// sequence catches up with the pins. Whether there was anything to
    /// write.
    async fn flush_open(&self) -> Result<bool> {
        let upto = {
            let mut st = self.st.borrow_mut();
            let st = &mut *st;
            if st.journal.open.is_some() && st.journal.may_close() {
                self.close(st, false);
            }
            st.journal.closed.back().map(|e| e.seq)
        };
        match upto {
            Some(upto) => self.write_entries(upto).await.map(|()| true),
            None => Ok(false),
        }
    }

    async fn flush_closed(&self) -> Result<()> {
        let upto = self.st.borrow().journal.closed.back().map(|e| e.seq);
        match upto {
            Some(upto) => self.write_entries(upto).await,
            None => Ok(()),
        }
    }
}

/// `txn` with its sequence `seq` in place: in its inodes' journal-sequence
/// fields, and in its emptied buckets' alloc keys and need_discard entries.
fn stamped(txn: &Txn, seq: u64) -> Txn {
    let mut out = Txn { updates: txn.updates.clone(), accounting: txn.accounting.clone(), roots: txn.roots.clone(), ..Txn::default() };
    for pos in &txn.stamped {
        if let Some(Some((_, v))) = out.updates.get_mut(&(Btree::Inodes, 0, *pos)) {
            v[..8].copy_from_slice(&seq.to_le_bytes());
        }
    }
    for &(b, mut bucket) in &txn.emptied {
        bucket.journal_seq_empty = seq;
        let at = Bpos::new(0, b, 0);
        out.updates.insert((Btree::Alloc, 0, at), Some((Key::new(kind::ALLOC_V4, at), bucket.encode().to_vec())));
        let pos = Bpos::new(seq, b, 0);
        out.updates.insert((Btree::NeedDiscard, 0, pos), Some((Key::new(kind::SET, pos), Vec::new())));
    }
    out
}

/// `txn`'s journal records at sequence `seq`, its first u64 at `base` of
/// its entry's area: its keys grouped by btree and level, then its
/// accounting deltas, each versioned by its place in the journal (A7.6);
/// and those versions.
fn records(txn: &Txn, seq: u64, base: u64) -> (Vec<u8>, BTreeMap<Bpos, Version>) {
    let mut area = Vec::new();
    let mut group: Option<((Btree, u8), Vec<u8>)> = None;
    for ((btree, level, pos), kv) in &txn.updates {
        let bytes = match kv {
            Some((k, v)) => Key { pos: *pos, ..*k }.encode(v),
            None => Key::new(kind::DELETED, *pos).encode(&[]),
        };
        match &mut group {
            Some((at, keys)) if *at == (*btree, *level) => keys.extend_from_slice(&bytes),
            _ => {
                if let Some(((b, l), keys)) = group.take() {
                    journal::record(&mut area, journal::Rec::Keys { btree: b, level: l, keys: &keys });
                }
                group = Some(((*btree, *level), bytes));
            }
        }
    }
    if let Some(((b, l), keys)) = group {
        journal::record(&mut area, journal::Rec::Keys { btree: b, level: l, keys: &keys });
    }
    let mut versions = BTreeMap::new();
    if !txn.accounting.is_empty() {
        let mut keys = Vec::new();
        // The record's header, then each key at its own u64.
        let first = base + (area.len() / 8) as u64 + 1;
        for (pos, delta) in &txn.accounting {
            let off = first + (keys.len() / 8) as u64;
            let version = Version { hi: (seq >> 32) as u32, lo: seq << 32 | off };
            versions.insert(*pos, version);
            keys.extend_from_slice(&Key { version, ..Key::new(kind::ACCOUNTING, *pos) }.encode(&value::counters(delta)));
        }
        journal::record(&mut area, journal::Rec::Keys { btree: Btree::Accounting, level: 0, keys: &keys });
    }
    (area, versions)
}

/// The clean unmount.
impl<D: BlockDev> Mount<D> {
    /// Writes everything out and marks the volume clean (A1.42): every
    /// orphan released; every dirty node written, leaves first, each
    /// level's pointer updates journaled before the level above is written;
    /// a final empty entry `S` whose last sequence is its own; then the
    /// superblock, clean, with `clean` naming `S`. Accounting totals that
    /// came to zero stay, as upstream keeps them (A7.23).
    pub async fn unmount(self) -> Result<()> {
        // A clean volume holds no orphan (A8.14), and nothing holds one now.
        self.release_orphans().await?;
        self.check_writable()?;
        while self.busy() {
            // Nothing else runs now, so every dirty node is ready to write.
            assert!(self.reclaim_round(Order::LeavesFirst).await?, "dirty nodes no reclaim round writes");
        }
        self.flush_open().await?;
        let upto = {
            let mut st = self.st.borrow_mut();
            let st = &mut *st;
            st.journal.open_new();
            self.close(st, true);
            st.journal.closed.back().expect("just closed").seq
        };
        self.write_entries(upto).await?;
        let clean = {
            let mut st = self.st.borrow_mut();
            let roots = encoded_roots(&st.roots);
            let body = journal::clean_body(upto, roots.iter().map(|(b, l, k)| (*b, *l, &k[..])), st.key_version, st.clocks);
            st.header.flags[0] |= 1 << 1;
            st.header.compat[0] |= COMPAT_CLEAN;
            st.member.journal_bucket = st.journal.head as u32;
            st.member.journal_offset = st.journal.head_sectors as u32;
            body
        };
        self.write_superblock(Some(clean)).await
    }

    /// Whether any node is dirty or unpointed.
    fn busy(&self) -> bool {
        let st = self.st.borrow();
        st.cache.dirty > 0 || st.cache.unpointed > 0
    }
}

#[cfg(test)]
impl<D: BlockDev> Mount<D> {
    /// The dirty-node limit, reclaim's room, the most dirty and unpointed
    /// nodes held at once, and the ordinary admissions that waited for
    /// journal space and for cache room.
    pub(crate) fn pressure(&self) -> (usize, usize, usize, [usize; 2]) {
        let st = self.st.borrow();
        (st.limit, st.reclaim_room, st.peak, st.waits)
    }

    /// Buckets taken again after an emptying this mount or an earlier one.
    pub(crate) fn reused(&self) -> usize {
        self.st.borrow().alloc.reused
    }

    /// What a stalled test reports: the counts admission and reclaim look
    /// at, and every node not clean.
    pub(crate) fn dump(&self) -> alloc::string::String {
        let st = self.st.borrow();
        let j = &st.journal;
        let dirty: Vec<_> = st.cache.ids().into_iter().map(|id| st.cache.node(id)).filter(|n| n.dirty() || n.captured.is_some() || n.writing || n.held > 0).map(|n| (n.btree, n.level, n.dirty(), n.pin(), n.captured, n.writing, n.held, n.bytes)).collect();
        alloc::format!("dirty={} unp={} over={} granted={} rg={} limit={} room={} open={:?} closed={} ring_free={} openable={} reclaim_entries={} durable_last={} newest={:?} head={} hs={} pins={:?} slot={} admitq_empty={} reclq={} nodes={:?}", st.cache.dirty, st.cache.unpointed, st.cache.oversized, st.granted, st.reclaim_granted, st.limit, st.reclaim_room, j.open.as_ref().map(|o| (o.seq, o.area.len()/8)), j.closed.len(), j.ring_free(), j.openable(), j.reclaim_entries, j.durable_last, j.newest, j.head, j.head_sectors, st.cache.oldest_pin(), self.slot_held.get(), self.admit.is_empty(), self.admit_reclaim.is_empty(), dirty)
    }
}
