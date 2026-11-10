//! A volume mounted read-write: one transaction at a time under the writer
//! slot (§3.3). An ordinary transaction's keys go into the nodes it holds;
//! replayed keys, reclaim's own updates and a change a node cannot take yet
//! wait in pending, and reclaim materializes them in bounded steps so the
//! journal can move on.
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

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use toyos_blockdev::{BlockDev, BLOCK};
use toyos_crypto::{scrypt, Random};

use super::bkey::{Bpos, Key, Version, KEY_U64S};
use super::bucket::Bucket;
use super::cache::{key_bytes, Cache, Cached, NodeId, Unwritten};
use super::crypt::MasterKey;
use super::journal::{self, Class, Closed, Jset, Replayed, Writer, SKIP};
use super::mkfs::{self, COMPAT_CLEAN, ROOT_SNAPSHOT};
use super::node::{self, Build, FIRST_KEYS};
use super::pending::{Overlay, Pending};
use super::read::{self, Inode, Parts, Source, Volume};
use super::superblock::{field_type, Field, Header, Member, Root, Superblock};
use super::sync::{Broadcast, Queue};
use super::value::{self, accounting_pos, class, data_type, inode_field, inode_flag, mode, AllocV4, DirentType, InodeValue, NodePtr, Ptr, MAX_NAME};
use super::{kind, Btree, Error, Result, SECTOR};

mod allocator;
mod data;
mod gc;
mod ops;
mod publish;
mod reclaim;

use allocator::{Allocator, Draw, Readers};
pub use data::Reservation;
pub use ops::{Link, Removed, Rename};
pub(crate) use reclaim::Order;
#[cfg(test)]
pub(crate) use reclaim::Plan;
use reclaim::{fresh_cost, node_ptr, MAX_GROUPS, MAX_REWRITE, RECLAIM_ROOM, REWRITTEN};

/// The mount's node cache capacity. Pending journal keys are held separately.
#[derive(Clone, Copy, Debug)]
pub struct MountOptions {
    /// Btree nodes cached. Dirty and unpointed nodes are bounded well below
    /// it; past it clean nodes are evicted.
    pub cache_nodes: usize,
}

/// u64s one ordinary transaction journals at most.
pub(crate) const K_TX: u64 = 512;
/// Nodes any ordinary transaction dirties at most.
pub(crate) const FOOTPRINT: usize = 28;
/// Nodes one reclaim batch writes.
const BATCH: usize = 2;
/// A pinning node may require the largest supported split. This also
/// exceeds a two-source sibling rewrite's cost, even with one clean source.
const PER_NODE: u64 = fresh_cost(MAX_REWRITE);
/// Cache room past the limit that only reclaim takes: what its transaction
/// dirties, and the nodes a batch's rewrites replace its nodes with.
const RECLAIM: usize = RECLAIM_ROOM + REWRITTEN;
pub(crate) const MIN_CACHE: usize = RECLAIM + 3 * FOOTPRINT;
/// Retries of a failed node or journal write, each of the same bytes.
const RETRIES: usize = 2;
/// Feature bits a volume carries once written read-write (A1.41).
const RW_FEATURES: u64 = 1 << 8 | 1 << 9 | 1 << 12 | 1 << 13 | 1 << 17 | 1 << 18;
/// Feature bit 5: the superblock holds a blacklist.
const BLACKLIST_FEATURE: u64 = 1 << 5;

/// Ring geometry that leaves two cache-sized reclaim shares and an ordinary
/// quarter, with room for at least one maximum-size ordinary transaction.
pub(crate) fn journal_buckets(bucket_sectors: u64, block: usize) -> u64 {
    let journal = Writer::new(alloc::vec![0], bucket_sectors, block, K_TX);
    let per_entry = journal.body() - K_TX;
    let share = ((RECLAIM + FOOTPRINT) as u64 * PER_NODE).div_ceil(per_entry) + 1;
    (share * 8).div_ceil(3).div_ceil(journal.slots())
}

/// What a new node is.
pub enum New {
    File,
    Dir,
    /// The target, without its on-disk NUL terminator.
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
    pending: Pending,
    roots: BTreeMap<Btree, Root>,
    /// Btrees given a root in memory, not yet written.
    new_roots: BTreeMap<Btree, NodeId>,
    journal: Writer,
    key_version: u64,
    next_version: u64,
    metadata_bytes: u64,
    metadata_buckets: u64,
    btree_nodes: u64,
    sparse_metadata: BTreeSet<u64>,
    sparse_data: BTreeSet<u64>,
    baseline_nodes: BTreeSet<u64>,
    granted_bytes: u64,
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
    /// The most bytes one ordinary apply grew a node by.
    #[cfg(test)]
    grown: usize,
    /// Ordinary admissions that waited for journal space, and for cache
    /// room, as tests construct them.
    #[cfg(test)]
    waits: [usize; 2],
    #[cfg(test)]
    journal_bytes: [usize; 3],
    #[cfg(test)]
    published: Published,
    /// Bytes of keys past which a terminal publication splits a new node,
    /// a node's unless a test lowers it.
    #[cfg(test)]
    group_limit: Option<usize>,
}

/// What the last terminal publication wrote, as tests inspect it.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Published {
    pub members: usize,
    /// Bounds added once the publication's own keys overfilled a new node.
    pub regrouped: usize,
    /// Members appended to.
    pub appended: usize,
    /// Members appended to with no change left once the plan settled.
    pub idle: usize,
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
    data_held: Cell<bool>,
    data_queue: Queue,
    reservations: Rc<RefCell<data::Reservations>>,
    metadata_gc: Cell<bool>,
    root: u64,
}

const _: () = assert!(PER_NODE <= K_TX, "a reclaim batch of one node fits one reservation");
/// The largest key a node holds, as its reader refuses a larger value
/// (A3.16).
const MAX_KEY: usize = 8 * (KEY_U64S + super::bkey::MAX_VALUE_U64S);
const _: () = assert!(MAX_KEY <= K_TX as usize * 8, "materialization's room takes any one key, so it always takes its first position");
/// Whether nodes of `node` bytes hold reclaim's largest split: a node read
/// at five times its bytes unpacked, and two transactions more, spread
/// evenly over [`MAX_GROUPS`] nodes, each taking its share and at most a key
/// past it.
const fn splits_fit(node: usize) -> bool {
    (5 * (node - FIRST_KEYS) + 2 * K_TX as usize * 8).div_ceil(MAX_GROUPS) + MAX_KEY <= node - FIRST_KEYS
}
const _: () = assert!(splits_fit(super::mkfs::MIN_BUCKET as usize * SECTOR), "mkfs's smallest nodes are writable");

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
    allocated_data: Vec<u64>,
    bucket_states: Vec<(u64, Bucket, Bucket)>,
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
    Append { id: NodeId, sectors: u16, journal_seq: u64 },
    /// Sources replaced by nodes covering their combined range, with a new
    /// root when the old root split.
    Replace { ids: Vec<NodeId>, by: Vec<Planned>, root: Option<Planned> },
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
    journal_seq: u64,
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
    if !splits_fit(sb.node_sectors() as usize * SECTOR) {
        return Err(Error::Unsupported("writing a volume of btree nodes too small for reclaim's largest split"));
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
    // An unclean mount replays what the ring holds; mkfs's ring is the
    // bound on that, so a larger one, another writer's, is not written.
    // Last: sizing mkfs's ring takes a journal writer for these buckets,
    // which the shapes refused above cannot have.
    let ring: u64 = sb.journal.iter().map(|&(_, count)| count).sum();
    if ring > super::mkfs::ring_buckets(sb.member.nbuckets, u64::from(sb.member.bucket_sectors), sb.block_bytes()) {
        return Err(Error::Unsupported("writing a volume whose journal is larger than mkfs lays out"));
    }
    Ok(())
}

impl<D: BlockDev> Mount<D> {
    /// An unencrypted volume, or one formatted without a passphrase,
    /// mounted read-write; new nodes' sequences are drawn from `random`.
    /// One not cleanly unmounted is replayed first, and its journal resumes
    /// past the post-crash skip.
    pub async fn open(dev: D, options: MountOptions, random: Box<dyn Random>) -> Result<Self> {
        // Early, so a volume refused is refused by name before it is read;
        // `from` judges again the copy the open read.
        writable(&read::read_superblock(&dev).await?.0)?;
        let volume = Volume::open(dev).await?;
        no_logged_ops(&volume).await?;
        let empty = allocator::empty_buckets(&volume).await?;
        let metadata = allocator::metadata_budget(&volume).await?;
        Self::from(volume.into_parts(), empty, metadata, options, random).await
    }

    /// An encrypted volume, unlocked as [`Volume::unlock`] does, mounted
    /// read-write.
    pub async fn unlock(dev: D, passphrase: &[u8], limits: scrypt::Limits, options: MountOptions, random: Box<dyn Random>) -> Result<Self> {
        // Early, so a volume refused costs no scrypt and is refused by name;
        // `from` judges again the copy the unlock read.
        writable(&read::read_superblock(&dev).await?.0)?;
        let volume = Volume::unlock(dev, passphrase, limits).await?;
        no_logged_ops(&volume).await?;
        let empty = allocator::empty_buckets(&volume).await?;
        let metadata = allocator::metadata_budget(&volume).await?;
        Self::from(volume.into_parts(), empty, metadata, options, random).await
    }

    /// `empty` is the volume's count of empty buckets.
    async fn from(parts: Parts<D>, empty: u64, metadata: allocator::Metadata, options: MountOptions, random: Box<dyn Random>) -> Result<Self> {
        let Parts { dev, mut sb, key, roots, key_version, replay, root, torn: torn_copies } = parts;
        // The superblock this mount runs on, which a device may answer
        // differently from an earlier read.
        writable(&sb)?;
        let key_version = key_version.checked_add(if replay.is_some() && key.is_some() { 65_536 } else { 0 }).ok_or(Error::NoSpace("encrypted extent versions"))?;
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
        let by_journal = (share.saturating_sub(1) * per_entry / PER_NODE) as usize;
        if options.cache_nodes < MIN_CACHE {
            return Err(Error::Invalid("cache too small for a transaction and worst-case reclaim"));
        }
        if by_journal < reclaim_room + FOOTPRINT {
            return Err(Error::Unsupported("journal too small for a transaction and worst-case reclaim"));
        }
        let limit = options.cache_nodes.saturating_sub(reclaim_room + 2 * FOOTPRINT).min(by_journal.saturating_sub(reclaim_room));
        assert!(limit >= FOOTPRINT, "validated cache and journal admit one transaction");
        journal.reclaim_entries = ((limit + reclaim_room) as u64 * PER_NODE).div_ceil(per_entry) + 1;

        let journal_next = journal.next_seq;
        let state = State {
            header: sb.header.clone(),
            fields: sb.fields.clone(),
            member: sb.member.clone(),
            blacklist,
            cache: Cache::new(options.cache_nodes, sb.node_sectors() as usize * SECTOR - FIRST_KEYS),
            pending: Pending::default(),
            roots: roots.into_iter().map(|r| (r.btree, r)).collect(),
            new_roots: BTreeMap::new(),
            journal,
            key_version,
            next_version: key_version,
            metadata_bytes: metadata.bytes,
            metadata_buckets: metadata.buckets,
            btree_nodes: metadata.nodes,
            sparse_metadata: metadata.sparse,
            sparse_data: metadata.sparse_data,
            baseline_nodes: metadata.baseline,
            granted_bytes: 0,
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
            grown: 0,
            #[cfg(test)]
            waits: [0; 2],
            #[cfg(test)]
            journal_bytes: [0; 3],
            #[cfg(test)]
            published: Published::default(),
            #[cfg(test)]
            group_limit: None,
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
            data_held: Cell::new(false),
            data_queue: Queue::default(),
            reservations: Rc::default(),
            metadata_gc: Cell::new(false),
            root,
        };
        mount.go_read_write(restate).await?;
        if let Some((_, replayed)) = replay {
            mount.install_replay(replayed).await?;
        }
        mount.resume_truncates().await?;
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

/// Refuses logged operations this writer cannot resume.
async fn no_logged_ops<D: BlockDev>(volume: &Volume<D>) -> Result<()> {
    Source::walk(volume, Btree::LoggedOps, Bpos::MIN, |k, v| {
        if k.pos.inode != 0 { return Ok(false); }
        data::truncate_value(k, v)?;
        Ok(true)
    })
    .await?;
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
            journal_seq: node.bset_seqs.iter().copied().max().unwrap_or(0),
            bytes: node.keys.iter().map(|(_, v)| key_bytes(v)).sum(),
            keys: Rc::new(node.keys),
            unwritten: Unwritten::default(),
            captured: None,
            compact: None,
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
        let root = match self.root_node(btree).await? {
            Some(id) => id,
            None => self.new_root(btree),
        };
        self.descend(root, level, pos).await
    }

    /// The node at `level` spanning `pos` below `id`, cached; held as
    /// [`Mount::find`]'s is.
    async fn descend(&self, mut id: NodeId, level: u8, pos: Bpos) -> Result<NodeId> {
        let btree = self.st.borrow().cache.node(id).btree;
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
                let (key, value) = st.logical(id, pos).next().ok_or(Error::Corrupt("interior node short of its range"))??;
                (n.level - 1, NodePtr::decode(&value)?, key.pos)
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
            journal_seq: 0,
            bytes: 0,
            keys: Rc::new(Vec::new()),
            unwritten: Unwritten::default(),
            captured: None,
            compact: None,
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
                    let (key, value) = st.logical(id, pos).next().ok_or(Error::Corrupt("interior node short of its range"))??;
                    Some((n.level - 1, NodePtr::decode(&value)?, key.pos))
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
    /// Node `id`'s keys from `from` on, as the pending keys over them make
    /// them.
    fn logical(&self, id: NodeId, from: Bpos) -> Overlay<'_> {
        let n = self.cache.node(id);
        self.pending.overlay(n.btree, n.level, from.max(n.min), n.max, &n.keys)
    }

    /// The logical key at `pos` of node `id`.
    fn get(&self, id: NodeId, pos: Bpos) -> Result<Option<(Key, Cow<'_, [u8]>)>> {
        let n = self.cache.node(id);
        self.pending.overlay(n.btree, n.level, pos, pos, &n.keys).next().transpose()
    }

    fn oldest_pin(&self) -> Option<u64> {
        self.cache.oldest_pin().into_iter().chain(self.pending.oldest_pin()).min()
    }

    /// Whether writing the journal now would free a bucket of its ring: the
    /// last sequence an entry closed now carries passes some bucket's newest.
    fn journal_frees(&self) -> bool {
        let j = &self.journal;
        let next = j.open.as_ref().map_or(j.next_seq, |o| o.seq);
        let last = self.oldest_pin().map_or(next, |p| p.min(next));
        j.newest.iter().enumerate().any(|(i, n)| i != j.head && n.is_some_and(|n| n < last))
    }
}

/// Puts `key` into `n`, journaled at `seq`: an accounting key adds its
/// delta to the node's.
fn put(n: &mut Cached, key: Key, value: Vec<u8>, seq: u64) {
    let pos = key.pos;
    if n.btree != Btree::Accounting || n.level != 0 {
        n.put(pos, (key.kind != kind::DELETED).then_some((key, value)), seq, seq);
        return;
    }
    let (mut sum, mut sums) = match n.keys.get(n.at(pos)) {
        Some((stored, counters)) if stored.pos == pos => (*stored, journal::counters(counters).expect("cached accounting decodes")),
        _ => (Key::new(kind::ACCOUNTING, pos), Vec::new()),
    };
    let delta = journal::counters(&value).expect("a transaction's accounting decodes");
    sums.resize(sums.len().max(delta.len()), 0);
    for (s, d) in sums.iter_mut().zip(delta) { *s = s.wrapping_add(d); }
    sum.version = sum.version.max(key.version);
    n.put(pos, Some((sum, value::counters(&sums))), seq, seq);
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
        let (_, value) = st.logical(id, pos).next().expect("a held node spans its range").expect("a held logical node decodes");
        let ptr = NodePtr::decode(&value).expect("a pointer the cache holds decodes");
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

    async fn get(&self, btree: Btree, pos: Bpos) -> Result<Option<(Key, Vec<u8>)>> {
        let _reading = read::Reading(self, self.begin_read());
        let Some(root) = self.root_node(btree).await? else { return Ok(None) };
        let leaf = self.descend(root, 0, pos).await?;
        Ok(self.st.borrow().get(leaf, pos)?.map(|(k, v)| (k, v.into_owned())))
    }

    /// A leaf at a time, each found from the root: pending keys are read
    /// where they lie, never copied over a whole node.
    async fn walk(&self, btree: Btree, mut from: Bpos, mut visit: impl FnMut(&Key, &[u8]) -> Result<bool>) -> Result<()> {
        let _reading = read::Reading(self, self.begin_read());
        loop {
            let Some(root) = self.root_node(btree).await? else { return Ok(()) };
            let leaf = self.descend(root, 0, from).await?;
            let st = self.st.borrow();
            for kv in st.logical(leaf, from) {
                let (key, value) = kv?;
                if !visit(&key, &value)? {
                    return Ok(());
                }
            }
            match st.cache.node(leaf).max.successor() {
                Some(next) => from = next,
                None => return Ok(()),
            }
        }
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
    /// it waits for what is granted to apply. A refusal waits only for what
    /// was committed before its arrival ([`Mount::grant`]). Reclaim's own share never waits
    /// on reclaim: past its closed entries, a short share is a bug, and the
    /// mount stops.
    async fn admit(&self, class: Class, draw: Draw, journal: u64, credit: u64, nodes: usize) -> Result<()> {
        let queue = match class {
            Class::Ordinary => &self.admit,
            Class::Reclaim => &self.admit_reclaim,
        };
        let place = queue.enter();
        let arrived = self.st.borrow().journal.next_seq;
        loop {
            let granted = place
                .until(|| match self.grant(class, draw, journal, credit, nodes, arrived) {
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
                    if !self.grant(class, draw, journal, credit, nodes, arrived)? {
                        self.stop("journal reclaim's share exhausted");
                        return Err(Error::ReadOnly("journal reclaim's share exhausted"));
                    }
                    return Ok(());
                }
            }
        }
    }

    /// Whether `credit` bytes more of metadata, past what is promised and
    /// granted, and `nodes` nodes would find room: what [`Mount::grant`]
    /// refuses `NoSpace` without, judged now.
    pub(super) fn room_for(&self, credit: u64, nodes: usize) -> bool {
        self.cancel_dropped();
        let pending = self.reservations.borrow().pending(Draw::Ordinary);
        let st = self.st.borrow();
        let stable = self.metadata_nodes(&st, pending + st.granted_bytes + credit);
        st.read_only.is_none() && st.alloc.keeps(st.metadata_buckets, stable, nodes, st.reclaim_room, Draw::Ordinary, false) && st.alloc.fits(nodes, st.reclaim_room, Draw::Ordinary)
    }

    /// Grants `class` its request now if it fits. An ordinary one that the
    /// room left could not hold is refused `NoSpace` once nothing written or
    /// emptied can change that: no other grant outstanding, as the ledgers
    /// `keeps` judges change when a transaction applies; no emptying
    /// awaiting durability; and nothing committed before sequence `arrived`
    /// left unwritten. Writing later changes leaves those ledgers as they
    /// are, and on a node of one block reclaim's writes never settle, as
    /// each one's keys dirty another node.
    fn grant(&self, class: Class, draw: Draw, journal: u64, credit: u64, nodes: usize, arrived: u64) -> Result<bool> {
        self.cancel_dropped();
        let pending = self.reservations.borrow().pending(draw);
        let mut st = self.st.borrow_mut();
        if let Some(why) = st.read_only {
            return Err(Error::ReadOnly(why));
        }
        let room = match class {
            Class::Ordinary => {
                let held = st.cache.dirty + st.cache.unpointed + st.granted;
                let stable = self.metadata_nodes(&st, pending + st.granted_bytes + credit);
                let keeps = st.alloc.keeps(st.metadata_buckets, stable, nodes, st.reclaim_room, draw, false);
                let physical = st.alloc.fits(nodes, st.reclaim_room, draw);
                let can_compact = st.sparse_metadata.iter().any(|&b| !st.alloc.metadata_open(b));
                let settled = st.granted == 0 && st.reclaim_granted == 0 && !st.alloc.returning() && st.oldest_pin().is_none_or(|pin| pin >= arrived);
                if (!keeps || (!physical && !can_compact)) && settled {
                    return Err(Error::NoSpace(match draw {
                        Draw::Ordinary | Draw::Reserved(_) => "no room left for the btree nodes a change may write",
                        Draw::Freeing => "no room left for the btree nodes a removal may write",
                    }));
                }
                // Nothing more lands in a node reclaim has yet to split.
                held + nodes <= st.limit && st.cache.oversized == 0 && st.alloc.fits(held + nodes, st.reclaim_room, draw)
                    && st.alloc.keeps(st.metadata_buckets, stable, held + nodes, st.reclaim_room, draw, false)
            }
            Class::Reclaim => st.reclaim_granted + nodes <= st.reclaim_room,
        };
        if room && st.journal.reserve(journal, class) {
            match class {
                Class::Ordinary => { st.granted += nodes; st.granted_bytes += credit; }
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

    /// Whether anything can be freed now: pending runs to materialize,
    /// entries to write, a dirty node to reclaim, or a journal whose last
    /// sequence can move on or that holds an emptying.
    fn can_free(&self) -> bool {
        let st = self.st.borrow();
        let flush = st.journal_frees() || st.alloc.returning();
        st.pending.oldest_pin().is_some()
            || !st.journal.closed.is_empty()
            || (flush && st.journal.open.is_some() && st.journal.may_close())
            || (st.journal_frees() && st.journal.open.is_none() && st.journal.openable() > 0)
            || st.sparse_metadata.iter().any(|&b| !st.alloc.metadata_open(b))
            || st.cache.ids().into_iter().any(|id| {
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
        if self.compact_metadata().await? { return Ok(()); }
        self.flush_open().await.map(|_| ())
    }

    /// Returns what [`Mount::admit`] took.
    fn ungrant(&self, st: &mut State, class: Class, journal: u64, credit: u64, nodes: usize) {
        st.journal.unreserve(journal);
        match class {
            Class::Ordinary => { st.granted -= nodes; st.granted_bytes -= credit; }
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
        self.transact_holding(bound, bound * 8, nodes, draw, prepare).await
    }

    /// [`Mount::transact`], its grant judging and holding `credit` bytes of
    /// metadata room rather than its journal bound's: what it promises past
    /// its own records, as a reservation's blocks each owe a transaction.
    async fn transact_holding<T>(&self, bound: u64, credit: u64, nodes: usize, draw: Draw, prepare: impl AsyncFnOnce(&mut Txn) -> Result<T>) -> Result<T> {
        assert!(nodes <= FOOTPRINT, "a grant of {nodes} nodes, past the largest any operation takes");
        assert!(credit >= bound * 8, "a grant's credit covers its own records");
        self.check_writable()?;
        let _slot = loop {
            self.admit(Class::Ordinary, draw, bound, credit, nodes).await?;
            let slot = self.slot(Class::Ordinary).await;
            let mut st = self.st.borrow_mut();
            if st.cache.oversized == 0 {
                break slot;
            }
            self.ungrant(&mut st, Class::Ordinary, bound, credit, nodes);
        };
        self.prepare_pending(Class::Ordinary, bound, credit, nodes)?;
        let mut txn = Txn::default();
        let out = prepare(&mut txn).await;
        self.finish(txn, out.is_ok(), Class::Ordinary, bound, credit, nodes)?;
        out
    }

    fn prepare_pending(&self, class: Class, bound: u64, credit: u64, nodes: usize) -> Result<()> {
        let mut st = self.st.borrow_mut();
        if let Err(error) = st.pending.reserve(bound as usize / KEY_U64S) {
            self.ungrant(&mut st, class, bound, credit, nodes);
            self.written.wake();
            return Err(error);
        }
        Ok(())
    }

    /// Applies `txn` when `apply`, else drops it; either way releases its
    /// holds and its grant. `ReadOnly` when the mount stopped writing while
    /// it was prepared.
    fn finish(&self, mut txn: Txn, apply: bool, class: Class, bound: u64, credit: u64, nodes: usize) -> Result<()> {
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
        let allocated = core::mem::take(&mut txn.allocated_data);
        if apply && stopped.is_none() {
            for &(b, before, after) in &txn.bucket_states {
                st.metadata_buckets = st.metadata_buckets + u64::from(after.data_type == data_type::BTREE) - u64::from(before.data_type == data_type::BTREE);
                if after.data_type == data_type::BTREE && u64::from(after.dirty) <= u64::from(self.sb.member.bucket_sectors) / 2 {
                    st.sparse_metadata.insert(b);
                } else {
                    st.sparse_metadata.remove(&b);
                }
                if after.data_type == data_type::USER && u64::from(after.dirty) <= u64::from(self.sb.member.bucket_sectors) / 2 {
                    st.sparse_data.insert(b);
                } else {
                    st.sparse_data.remove(&b);
                }
            }
            for (&(btree, level, pos), update) in &txn.updates {
                if level != 0 { continue; }
                let prior = st.get(held(st, btree, level, pos), pos).expect("a prepared logical node decodes");
                let prior = prior.as_ref().map(|(_, v)| &v[..]);
                let update = update.as_ref().map(|(_, v)| &v[..]);
                let charge = |value: Option<&[u8]>| value.map_or(0, |v| allocator::charge(btree, v));
                let tree_node = |value: Option<&[u8]>| value.is_some_and(|v| v.get(2) == Some(&data_type::BTREE));
                let (added, removed, before, after) = (charge(update), charge(prior), tree_node(prior), tree_node(update));
                st.metadata_bytes = st.metadata_bytes + added - removed;
                if btree == Btree::Backpointers {
                    st.btree_nodes = st.btree_nodes + u64::from(after) - u64::from(before);
                    if before && !after { st.baseline_nodes.remove(&(pos.offset >> self.bp_shift())); }
                }
            }
            for (&pos, delta) in &txn.accounting {
                let prior = st.get(held(st, Btree::Accounting, 0, pos), pos).expect("a prepared accounting node decodes");
                let before = prior.as_ref().map_or(0, |(_, v)| key_bytes(v) as u64);
                let counters = prior.as_ref().map_or(0, |(_, v)| v.len() / 8).max(delta.len());
                st.metadata_bytes = st.metadata_bytes + (KEY_U64S * 8 + counters * 8) as u64 - before;
            }
            self.apply(st, txn, bound, class);
            for sector in allocated { st.alloc.unpin(sector / u64::from(self.sb.member.bucket_sectors)); }
        } else {
            release(st, &txn.held);
            for sector in allocated { st.alloc.cancel_data(sector, u64::from(self.sb.member.bucket_sectors)); }
        }
        #[cfg(test)]
        {
            st.peak = st.peak.max(st.cache.dirty + st.cache.unpointed);
        }
        assert!(st.cache.dirty + st.cache.unpointed <= st.limit + st.reclaim_room, "dirty and unpointed nodes past the cache's limit and reclaim's room");
        self.ungrant(st, class, bound, credit, nodes);
        self.written.wake();
        match stopped {
            Some(why) if apply => Err(Error::ReadOnly(why)),
            _ => Ok(()),
        }
    }

    /// Appends the transaction's records, transfers its keys to pending
    /// ownership, and publishes roots and completed node writes. No await
    /// separates these logical changes.
    fn apply(&self, st: &mut State, mut txn: Txn, bound: u64, class: Class) {
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

        let updates = core::mem::take(&mut txn.updates).into_iter().map(|((btree, level, pos), kv)| {
            let (key, value) = kv.unwrap_or_else(|| (Key::new(kind::DELETED, pos), Vec::new()));
            (btree, level, key, value)
        });
        let accounting = core::mem::take(&mut txn.accounting).into_iter().map(|(pos, delta)| {
            let key = Key { version: versions[&pos], ..Key::new(kind::ACCOUNTING, pos) };
            (Btree::Accounting, 0, key, value::counters(&delta))
        });
        // An ordinary transaction's keys go into the nodes it holds, as
        // reclaim's materialization would put them, where pending holds
        // nothing older at their position. A node that cannot take them now,
        // and reclaim's own updates, leave them to pending and reclaim.
        let room = match class {
            Class::Ordinary => K_TX.checked_sub(size).expect("an ordinary transaction within K_TX") as usize * 8,
            Class::Reclaim => 0,
        };
        let mut drained = Vec::new();
        let mut left = Vec::new();
        for (btree, level, key, value) in updates.chain(accounting) {
            let id = (class == Class::Ordinary).then(|| held(st, btree, level, key.pos)).filter(|&id| st.cache.node(id).captured.is_none());
            let Some(id) = id.filter(|&id| self.drained(st, id, key.pos, room, &mut drained)) else {
                left.push((btree, level, key, value));
                continue;
            };
            st.cache.change(id, |n| put(n, key, value, seq));
        }
        #[cfg(test)]
        for &(id, before) in &drained {
            st.grown = st.grown.max(st.cache.node(id).bytes.saturating_sub(before));
        }
        st.pending.apply(seq, left);
        for root in core::mem::take(&mut txn.roots) {
            st.roots.insert(root.btree, root);
        }
        // Before an install replaces a node it held.
        release(st, &holds);
        for install in installs {
            self.install(st, install);
        }
    }

    /// Whether node `id` takes a key at `pos` in place: pending holds
    /// nothing at `pos`. A node the transaction has not yet `drained` first
    /// takes what pending holds of its range within `room`, the transaction
    /// bound its own keys leave, so together they grow it by at most one
    /// transaction's.
    fn drained(&self, st: &mut State, id: NodeId, pos: Bpos, room: usize, drained: &mut Vec<(NodeId, usize)>) -> bool {
        let n = st.cache.node(id);
        let (btree, level, min, max) = (n.btree, n.level, n.min, n.max);
        if !drained.iter().any(|&(d, _)| d == id) {
            drained.push((id, n.bytes));
            if st.pending.holds(btree, level, min, max) && self.take_into(st, id, min, room).is_err() { return false; }
        }
        !st.pending.holds(btree, level, pos, pos)
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
        let pinned = st.oldest_pin().map_or(open.seq, |p| p.min(open.seq));
        let last_seq = if own { pinned } else { pinned.min(st.journal.durable) };
        assert!(last_seq >= st.journal.durable_last.max(st.journal.closed_last), "a journal entry's last sequence moved back");
        st.journal.closed_last = last_seq;
        let roots = encoded_roots(&st.roots);
        journal::tail(&mut open.area, roots.iter().map(|(b, l, k)| (*b, *l, &k[..])), Some(st.now), st.key_version, st.clocks);
        let entry = Jset { seq: open.seq, last_seq, area: &open.area };
        let bytes = journal::encode(&st.header.internal_uuid, self.sb.block_bytes(), self.key.as_ref(), &entry).expect("a journal entry encrypts");
        #[cfg(test)]
        {
            st.journal_bytes[0] += 1;
            st.journal_bytes[1] += open.area.len();
            st.journal_bytes[2] += bytes.len();
        }
        st.journal.closed.push_back(Closed { seq: open.seq, last_seq, bytes });
    }

    /// Writes every closed entry through `upto`, in order, each after a
    /// flush and itself durable as it is written (A2.12), so each is durable
    /// once written and so is every node write that completed before it. An
    /// entry naming itself has its header sector written last, once the rest
    /// is durable. The journal's blocks are written no other way, so a
    /// durable write of one is never over a write no flush has covered. A
    /// failed write is reissued with the same bytes, or the mount stops
    /// writing.
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
                        mkfs::write_durable(&self.dev, sector, &headless).await?;
                        mkfs::write_durable(&self.dev, sector, &bytes[..BLOCK]).await
                    } else {
                        mkfs::write_durable(&self.dev, sector, &bytes).await
                    }
                })
                .await?;
                let mut st = self.st.borrow_mut();
                st.journal.closed.pop_front();
                st.journal.written(seq, last_seq, (bytes.len() / SECTOR) as u64);
                st.pending.durable(seq);
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
            if st.journal.open.is_none() && st.journal_frees() && st.journal.openable() > 0 {
                st.journal.open_new();
            }
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
    /// orphan released; everything committed before the unmount written by
    /// reclaim, oldest first; what reclaim's own updates left, and what
    /// writing that derives, in one terminal publication
    /// ([`Mount::publish`]); a final empty entry `S` whose last sequence is
    /// its own and whose roots reach that publication; then the
    /// superblock, clean, with `clean` naming `S`. Accounting totals that
    /// came to zero stay, as upstream keeps them (A7.23).
    pub async fn unmount(self) -> Result<()> {
        self.unmount_clean().await
    }

    async fn unmount_clean(&self) -> Result<()> {
        // A clean volume holds no orphan (A8.14), and nothing holds one now.
        self.release_orphans().await?;
        self.check_writable()?;
        self.flush_open().await?;
        // Reclaim keeps the working set within the cache while it writes
        // what was committed; its own updates pin only later sequences.
        let start = self.st.borrow().journal.next_seq;
        while self.st.borrow().oldest_pin().is_some_and(|pin| pin < start) {
            // Nothing else runs now, so every dirty node is ready to write.
            assert!(self.reclaim_round(Order::OldestPin).await?, "committed changes no reclaim round writes");
        }
        self.publish_clean().await
    }

    /// Everything left published, then the clean entry and superblock.
    async fn publish_clean(&self) -> Result<()> {
        self.flush_open().await?;
        while self.materialize(Order::LeavesFirst).await? {}
        let seq = self.st.borrow().journal.next_seq;
        self.publish(seq).await?;
        let upto = {
            let mut st = self.st.borrow_mut();
            let st = &mut *st;
            st.journal.open_new();
            self.close(st, true);
            st.journal.closed.back().expect("just closed").seq
        };
        assert_eq!(upto, seq, "the clean entry publishes the terminal writes");
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

    /// Whether journal-owned keys or unfinished node writes remain.
    fn busy(&self) -> bool {
        let st = self.st.borrow();
        st.pending.oldest_pin().is_some() || st.cache.dirty > 0 || st.cache.unpointed > 0
    }
}

#[cfg(test)]
impl<D: BlockDev> Mount<D> {
    /// Drops pending's spare workspace, so its next batch allocates.
    pub(crate) fn shrink_pending(&self) {
        self.st.borrow_mut().pending.shrink();
    }

    /// The most bytes one ordinary transaction grew a node by, its keys and
    /// what the node took from pending together.
    #[cfg(test)]
    pub(crate) fn grown(&self) -> usize {
        self.st.borrow().grown
    }

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

    pub(crate) fn pending_pressure(&self) -> (usize, usize, usize) {
        let st = self.st.borrow();
        (st.pending.iter().count(), st.pending.allocated_bytes(), st.cache.dirty + st.cache.unpointed)
    }

    /// Splits a terminal publication's new nodes past `bytes` of keys.
    pub(crate) fn limit_publication_groups(&self, bytes: usize) {
        self.st.borrow_mut().group_limit = Some(bytes);
    }

    /// A clean unmount without reclaim first writing what was committed:
    /// all of it in the terminal publication, and what that wrote.
    /// Every cached node of `btree` written to a fresh node next, as one
    /// read after an unclean mount and not yet rewritten is (§3.5).
    pub(crate) fn rewrite_next(&self, btree: Btree) {
        let mut st = self.st.borrow_mut();
        for id in st.cache.ids() {
            st.cache.change(id, |n| n.fresh |= n.btree == btree);
        }
    }

    /// A clean unmount, and what its terminal publication wrote.
    pub(crate) async fn unmount_published(self) -> Result<Published> {
        self.unmount_clean().await?;
        Ok(self.st.borrow().published)
    }

    pub(crate) async fn unmount_in_one_publication(self) -> Result<Published> {
        self.release_orphans().await?;
        self.publish_clean().await?;
        Ok(self.st.borrow().published)
    }

    /// The ring bucket the journal writes into.
    #[cfg(test)]
    pub(crate) fn journal_head(&self) -> usize {
        self.st.borrow().journal.head
    }

    pub(crate) fn journal_room(&self) -> (u64, u64) {
        let st = self.st.borrow();
        (st.journal.ring_free(), 2 * st.journal.reclaim_entries)
    }

    /// What a stalled test reports: the counts admission and reclaim look
    /// at, and every node not clean.
    pub(crate) fn dump(&self) -> alloc::string::String {
        let st = self.st.borrow();
        let j = &st.journal;
        let dirty: Vec<_> = st.cache.ids().into_iter().map(|id| st.cache.node(id)).filter(|n| n.dirty() || n.captured.is_some() || n.writing || n.held > 0).map(|n| (n.btree, n.level, n.dirty(), n.pin(), n.captured, n.writing, n.held, n.bytes)).collect();
        let mut owners = BTreeMap::new();
        for entry in st.pending.iter().filter(|e| Some(e.capture().first) == st.pending.oldest_pin()) {
            let position = entry.position();
            *owners.entry((position.0, position.1)).or_insert(0) += 1;
        }
        let geometry = (st.alloc.capacity(st.metadata_buckets), st.alloc.ready(0), st.btree_nodes, st.alloc.occupied_metadata(st.metadata_buckets), st.metadata_bytes, st.baseline_nodes.len(), self.metadata_nodes(&st, K_TX * 8));
        alloc::format!("stopped={:?} capacity_free_live_occupied_bytes_baseline_stable={geometry:?} pending={} pending_bytes={} pending_pin={:?} pending_owners={owners:?} journal_entries_payload_encoded={:?} dirty={} unp={} over={} granted={} rg={} limit={} room={} open={:?} closed={} ring_free={} openable={} reclaim_entries={} durable_last={} newest={:?} head={} hs={} pins={:?} slot={} admitq_empty={} reclq={} nodes={:?}", st.read_only, st.pending.iter().count(), st.pending.allocated_bytes(), st.pending.oldest_pin(), st.journal_bytes, st.cache.dirty, st.cache.unpointed, st.cache.oversized, st.granted, st.reclaim_granted, st.limit, st.reclaim_room, j.open.as_ref().map(|o| (o.seq, o.area.len()/8)), j.closed.len(), j.ring_free(), j.openable(), j.reclaim_entries, j.durable_last, j.newest, j.head, j.head_sectors, st.oldest_pin(), self.slot_held.get(), self.admit.is_empty(), self.admit_reclaim.is_empty(), dirty)
    }
}

