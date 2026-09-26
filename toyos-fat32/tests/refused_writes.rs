//! A device write whose outcome is unknown, at every write of every call that
//! changes the volume's structure.
//!
//! A refused write may already be on the medium: the kernel's USB path can
//! issue a write, lose the answer, and report its own budget expired. So each
//! write is refused twice over — once before it reaches the bytes, once after —
//! and then in bursts that also refuse the call's own repair and the next
//! attempts, as a stopping machine refuses every retry for a while. Reads are
//! served from a copy a refused write does not update, as the kernel adapter's
//! resident blocks are, so reading back cannot stand in for knowing; a read is
//! refused too, at every read of a truncation. The judge is
//! `toyos-fat32-check`, written from fatgen103 and sharing no code with the
//! writer, over a volume that same crate's fixture built from the
//! specification.
//!
//! What must hold: after a single refusal the call has left the volume
//! consistent by the time it returns; after any refusals, retrying until the
//! device answers leaves it consistent, with the two FATs agreeing and no
//! cluster orphaned.

mod common;

#[path = "../../toyos-fat32-check/tests/common/mod.rs"]
mod spec_volume;

use spec_volume::{fat_offset, Volume, BYTES_PER_SECTOR, CLUSTERS, FAT_SECTORS, NUM_FATS};
use toyos_fat32::{BlockAccess, Error, Fat32, FatTime, File, IoError, MAX_LFN_CHARS, MAX_REPAIR_STEPS};
use toyos_fat32_check::Complaint;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// The write never reached the bytes.
    Refused,
    /// The write reached the bytes and was answered as refused anyway.
    Landed,
}

type Plan = Box<dyn FnMut(u64, u64, usize) -> Option<Outcome>>;

/// Whether to refuse read `index` since the plan was armed.
type ReadPlan = Box<dyn FnMut(u64) -> bool>;

type Call = fn(&mut Fat32<Faulty>, &mut Option<File>, u32) -> Result<(), Error>;

/// The volume in memory, refusing whichever reads and writes the plans name.
///
/// Reads are served from `resident`, which is the kernel adapter's resident
/// blocks at their stalest: a write that succeeded updates it, and a refused
/// write does not, even when it landed on the medium. So a call that reads to
/// learn a refused write's outcome learns the wrong one here, as it would on
/// the stick.
struct Faulty {
    /// The medium, and what the checker judges.
    bytes: Vec<u8>,
    resident: Vec<u8>,
    /// Writes since the plan was armed.
    writes: u64,
    plan: Option<Plan>,
    /// Reads since the read plan was armed.
    reads: u64,
    read_plan: Option<ReadPlan>,
    refused: u32,
}

impl Faulty {
    fn new(bytes: Vec<u8>) -> Faulty {
        let resident = bytes.clone();
        Faulty { bytes, resident, writes: 0, plan: None, reads: 0, read_plan: None, refused: 0 }
    }

    fn arm(&mut self, plan: Plan) {
        self.writes = 0;
        self.refused = 0;
        self.plan = Some(plan);
    }

    fn arm_reads(&mut self, plan: ReadPlan) {
        self.reads = 0;
        self.refused = 0;
        self.read_plan = Some(plan);
    }

    fn range(&self, offset: u64, len: usize) -> Result<core::ops::Range<usize>, IoError> {
        let start = usize::try_from(offset).map_err(|_| IoError::Device)?;
        let end = start.checked_add(len).ok_or(IoError::Device)?;
        if end > self.bytes.len() {
            return Err(IoError::Device);
        }
        Ok(start..end)
    }
}

impl BlockAccess for Faulty {
    fn capacity(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let r = self.range(offset, buf.len())?;
        let index = self.reads;
        self.reads += 1;
        if self.read_plan.as_mut().is_some_and(|p| p(index)) {
            self.refused += 1;
            return Err(IoError::BudgetExpired);
        }
        buf.copy_from_slice(&self.resident[r]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError> {
        let r = self.range(offset, buf.len())?;
        let index = self.writes;
        self.writes += 1;
        let verdict = self.plan.as_mut().and_then(|p| p(index, offset, buf.len()));
        match verdict {
            None => {
                self.bytes[r.clone()].copy_from_slice(buf);
                self.resident[r].copy_from_slice(buf);
                Ok(())
            }
            Some(outcome) => {
                self.refused += 1;
                if outcome == Outcome::Landed {
                    self.bytes[r].copy_from_slice(buf);
                }
                Err(IoError::BudgetExpired)
            }
        }
    }

    fn flush(&mut self) -> Result<(), IoError> {
        Ok(())
    }
}

fn stamp() -> FatTime {
    FatTime::from_unix_secs(1_717_245_296)
}

/// Everything the checker has to say, less a stale `FSI_Free_Count` when the
/// caller has not synced — FSInfo is a hint `sync` writes, and between syncs it
/// is stale by design.
fn complaints(bytes: &[u8], synced: bool) -> Vec<Complaint> {
    toyos_fat32_check::check(bytes)
        .into_iter()
        .filter(|c| synced || !matches!(c, Complaint::FsInfoFreeCount { .. }))
        .collect()
}

fn assert_clean(fs: &mut Fat32<Faulty>, synced: bool, context: &str) {
    common::assert_fats_agree(fs);
    let said = complaints(&fs.device().bytes, synced);
    assert!(said.is_empty(), "{context}:\n{}", toyos_fat32_check::describe(&said));
}

/// A call, and how to ask for it again.
struct Scenario {
    name: &'static str,
    /// The call has a commit, after which it answers `Ok` whatever its
    /// remaining writes met: they are queued, and the next call or `sync`
    /// finishes them first.
    commits: bool,
    /// Run once on the clean fixture; the volume it leaves is synced and is
    /// where every case starts. Returns the handle the call works on, if any.
    setup: fn(&mut Fat32<Faulty>) -> Option<File>,
    /// The call. `attempt` counts from zero, so a retry can accept what a
    /// committed first attempt already did.
    call: Call,
    /// What must be true once the call has gone through.
    verify: fn(&mut Fat32<Faulty>, &mut Option<File>),
}

fn fixture(s: &Scenario) -> (Vec<u8>, Option<File>) {
    let mut v = Volume::new();
    v.finish();
    let mut fs = Fat32::mount(Faulty::new(v.bytes)).expect("mount the fixture");
    let handle = (s.setup)(&mut fs);
    fs.sync().expect("sync the setup");
    assert_clean(&mut fs, true, &format!("{}: the setup itself", s.name));
    (fs.into_device().bytes, handle)
}

/// Refuse `count` writes from write `from`, each with `outcome`.
fn burst(from: u64, count: u64, outcome: Outcome) -> Plan {
    Box::new(move |i, _, _| (i >= from && i - from < count).then_some(outcome))
}

/// Every write of `s.call`, refused each way, alone and in bursts. Returns how
/// many cases refused something.
fn exhaust(s: &Scenario) -> u32 {
    let (base, handle) = fixture(s);
    let mut cases = 0;
    for outcome in [Outcome::Refused, Outcome::Landed] {
        for count in [1u64, 2, 3, 12] {
            for from in 0u64.. {
                let context = format!("{}: writes {from}..+{count} {outcome:?}", s.name);
                let arm = |d: &mut Faulty| d.arm(burst(from, count, outcome));
                if !refused_case(s, &base, handle, arm, count == 1, &context) {
                    assert!(from > 0, "{}: the call wrote nothing", s.name);
                    break;
                }
                cases += 1;
            }
        }
    }
    eprintln!("{}: {cases} refused cases, every one consistent", s.name);
    cases
}

/// Every read of `s.call` refused, alone and in bursts: a read refused before
/// a write that depends on it must leave nothing the retry cannot finish.
fn exhaust_reads(s: &Scenario) -> u32 {
    let (base, handle) = fixture(s);
    let mut cases = 0;
    for count in [1u64, 3, 12] {
        for from in 0u64.. {
            let context = format!("{}: reads {from}..+{count} refused", s.name);
            let arm = |d: &mut Faulty| d.arm_reads(Box::new(move |i| i >= from && i - from < count));
            if !refused_case(s, &base, handle, arm, count == 1, &context) {
                assert!(from > 0, "{}: the call read nothing", s.name);
                break;
            }
            cases += 1;
        }
    }
    eprintln!("{}: {cases} refused-read cases, every one consistent", s.name);
    cases
}

/// One case on a fresh mount of `base`: the call under the armed plan, then
/// retried until it answers and synced. False when the plan refused nothing.
fn refused_case(
    s: &Scenario,
    base: &[u8],
    handle: Option<File>,
    arm: impl FnOnce(&mut Faulty),
    single: bool,
    context: &str,
) -> bool {
    let mut fs = Fat32::mount(Faulty::new(base.to_vec())).expect("mount");
    let mut h = handle;
    arm(fs.device());

    let first = (s.call)(&mut fs, &mut h, 0);
    if fs.device().refused == 0 {
        assert_eq!(first, Ok(()), "{context}: nothing was refused");
        return false;
    }
    if !(s.commits && first.is_ok()) {
        assert_eq!(first, Err(Error::BudgetExpired), "{context}");
    }
    // A handle whose entry lags its chain is `File::needs_reconcile`'s
    // window, which a stop leaves with no refusal at all.
    if single && !h.is_some_and(|f| f.needs_reconcile()) {
        assert_clean(&mut fs, false, &format!("{context}: stopped as the call returned"));
    }

    let mut attempt = 0;
    let mut result = first;
    while result.is_err() {
        attempt += 1;
        assert!(attempt <= 24, "{context}: still refused after {attempt} attempts: {result:?}");
        result = (s.call)(&mut fs, &mut h, attempt);
    }
    let mut syncs = 0;
    while let Err(e) = fs.sync() {
        syncs += 1;
        assert!(syncs <= 24, "{context}: sync still refused: {e}");
    }
    assert_clean(&mut fs, true, &format!("{context}: after {attempt} retries"));
    (s.verify)(&mut fs, &mut h);
    true
}

// ---------------------------------------------------------------- append

const LOG: &str = "dated.log";

fn one_cluster_logged(fs: &mut Fat32<Faulty>) -> Option<File> {
    let mut f = fs.create(LOG, stamp()).expect("create");
    fs.write(&mut f, 0, &common::pattern(512, 1)).expect("first cluster");
    fs.flush_meta(&mut f, stamp()).expect("flush");
    Some(f)
}

fn append_two_clusters(fs: &mut Fat32<Faulty>, h: &mut Option<File>, _: u32) -> Result<(), Error> {
    let f = h.as_mut().expect("a handle");
    fs.write(f, 512, &common::pattern(1024, 2))?;
    fs.flush_meta(f, stamp())
}

fn appended_in_place(fs: &mut Fat32<Faulty>, _: &mut Option<File>) {
    let mut want = common::pattern(512, 1);
    want.extend(common::pattern(1024, 2));
    assert_eq!(common::read_all(fs, LOG), want);
    // One run: the retry claimed the clusters the refusal gave back, rather
    // than stepping over one it had leaked.
    assert_eq!(fs.extents(LOG, 8).expect("extents").len(), 1, "the retry did not reuse the claim");
}

/// `append_cluster`: claim, then link, per cluster; then the entry.
fn append() -> Scenario {
    Scenario { name: "append", commits: false, setup: one_cluster_logged, call: append_two_clusters, verify: appended_in_place }
}

/// A file's first cluster, which no entry reaches until the flush.
fn first_allocation() -> Scenario {
    Scenario {
        name: "first allocation",
        commits: false,
        setup: |fs| {
            let mut f = fs.create(LOG, stamp()).expect("create");
            fs.flush_meta(&mut f, stamp()).expect("flush");
            Some(f)
        },
        call: |fs, h, _| {
            let f = h.as_mut().expect("a handle");
            fs.write(f, 0, &common::pattern(1536, 3))?;
            fs.flush_meta(f, stamp())
        },
        verify: |fs, _| {
            assert_eq!(common::read_all(fs, LOG), common::pattern(1536, 3));
            assert_eq!(fs.extents(LOG, 8).expect("extents").len(), 1);
        },
    }
}

/// An append and the sync after it, as one call: the sync re-drives whatever
/// repair the append's refusals left, and a refusal inside that re-drive — or
/// of FSInfo — is retried like any other.
fn append_and_sync() -> Scenario {
    Scenario {
        name: "append+sync",
        commits: false,
        setup: one_cluster_logged,
        call: |fs, h, a| {
            append_two_clusters(fs, h, a)?;
            fs.sync()
        },
        verify: appended_in_place,
    }
}

/// A shrink frees a tail from its first write on; its refusal is carried
/// forward too, and the entry is recorded once the free has gone through.
fn truncate() -> Scenario {
    Scenario {
        name: "truncate",
        commits: false,
        setup: |fs| {
            let mut f = fs.create(LOG, stamp()).expect("create");
            fs.write(&mut f, 0, &common::pattern(5 * 512, 5)).expect("write");
            fs.flush_meta(&mut f, stamp()).expect("flush");
            Some(f)
        },
        call: |fs, h, _| {
            let f = h.as_mut().expect("a handle");
            fs.set_len(f, 700)?;
            fs.flush_meta(f, stamp())
        },
        verify: |fs, _| {
            assert_eq!(common::read_all(fs, LOG), common::pattern(5 * 512, 5)[..700].to_vec());
        },
    }
}

// ------------------------------------------------------------ directories

/// Short names, so each takes one entry and the two root clusters the
/// fixture has hold exactly this many.
const ROOT_SLOTS: u32 = 32;

fn full_root(fs: &mut Fat32<Faulty>) -> Option<File> {
    for i in 0..ROOT_SLOTS {
        let mut f = fs.create(&format!("F{i:02}.TXT"), stamp()).expect("fill the root");
        fs.flush_meta(&mut f, stamp()).expect("flush");
    }
    None
}

fn root_intact(fs: &mut Fat32<Faulty>) {
    for i in 0..ROOT_SLOTS {
        assert!(fs.exists(&format!("F{i:02}.TXT")).expect("exists"), "F{i:02}.TXT went missing");
    }
}

/// `find_free_run`'s growth: claim, zero, link — then a long-name run and its
/// short entry in the new cluster.
fn extend_dir() -> Scenario {
    Scenario {
        name: "extend-dir",
        commits: false,
        setup: full_root,
        call: |fs, _, _| fs.create("A name long enough for three entries.txt", stamp()).map(drop),
        verify: |fs, _| {
            root_intact(fs);
            let m = fs.metadata("A name long enough for three entries.txt").expect("created");
            assert_eq!(m.len, 0);
        },
    }
}

/// A long name in a directory with room: its entries are written one at a
/// time, and one refused after it reached the bytes is written back with the
/// ones before it.
fn long_name_create() -> Scenario {
    Scenario {
        name: "create",
        commits: false,
        setup: |_| None,
        call: |fs, _, _| fs.create("2026-09-21-083539.log", stamp()).map(drop),
        verify: |fs, _| {
            assert_eq!(fs.read_dir("", 64).expect("read_dir").len(), 1, "one name, once");
            assert!(fs.exists("2026-09-21-083539.log").expect("exists"));
        },
    }
}

/// `create_dir` over a full parent: the directory's own claim, and inside it
/// the parent's growth claim — the two nest.
fn nested_create_dir() -> Scenario {
    Scenario {
        name: "create_dir",
        commits: false,
        setup: full_root,
        call: |fs, _, _| fs.create_dir("A directory with a long name", stamp()),
        verify: |fs, _| {
            root_intact(fs);
            assert!(fs.metadata("A directory with a long name").expect("created").is_dir);
            common::write_new(fs, "A directory with a long name/inner.bin", &[7u8; 700], stamp());
            fs.sync().expect("sync");
            let said = complaints(&fs.device().bytes, true);
            assert!(said.is_empty(), "{}", toyos_fat32_check::describe(&said));
        },
    }
}

/// A move into another directory: the new run, the old run's erase, and the
/// moved directory's `..`, undone together.
fn rename() -> Scenario {
    Scenario {
        name: "rename",
        commits: false,
        setup: |fs| {
            fs.create_dir("into", stamp()).expect("mkdir");
            fs.create_dir("moving", stamp()).expect("mkdir");
            common::write_new(fs, "moving/kept.bin", &common::pattern(700, 6), stamp());
            None
        },
        call: |fs, _, _| fs.rename("moving", "into/A moved directory"),
        verify: |fs, _| {
            assert!(!fs.exists("moving").expect("exists"));
            assert_eq!(common::read_all(fs, "into/A moved directory/kept.bin"), common::pattern(700, 6));
        },
    }
}

const DOOMED: &str = "An old log with a long name.log";

/// Erase, then free the chain. A refusal while erasing restores the entry and
/// answers `Err`; once it is erased the call answers `Ok` and carries the free
/// forward, so a retry never meets `NotFound` and nothing leaks either way.
fn remove() -> Scenario {
    Scenario {
        name: "remove",
        commits: true,
        setup: |fs| {
            common::write_new(fs, DOOMED, &common::pattern(5 * 512, 4), stamp());
            None
        },
        call: |fs, _, _| fs.remove(DOOMED),
        verify: |fs, _| {
            assert!(!fs.exists(DOOMED).expect("exists"));
            let free = fs.free_bytes().expect("free");
            assert_eq!(free, fs.total_bytes() - 2 * 512, "only the root's two clusters stay taken");
        },
    }
}

/// `remove_dir`: the same erase-then-free as `remove`.
fn remove_dir() -> Scenario {
    Scenario {
        name: "remove_dir",
        commits: true,
        setup: |fs| {
            fs.create_dir("A directory with a long name", stamp()).expect("mkdir");
            None
        },
        call: |fs, _, _| fs.remove_dir("A directory with a long name"),
        verify: |fs, _| {
            assert!(!fs.exists("A directory with a long name").expect("exists"));
            assert_eq!(fs.free_bytes().expect("free"), fs.total_bytes() - 2 * 512);
        },
    }
}

#[test]
fn every_refused_write_of_an_append_is_undone() {
    assert!(exhaust(&append()) > 0);
}

#[test]
fn every_refused_write_of_a_first_allocation_is_undone() {
    assert!(exhaust(&first_allocation()) > 0);
}

#[test]
fn every_refused_write_of_an_append_and_its_sync_converges() {
    assert!(exhaust(&append_and_sync()) > 0);
}

#[test]
fn every_refused_write_of_a_truncation_is_carried_through() {
    assert!(exhaust(&truncate()) > 0);
}

/// The kept cluster's entry is read before the terminator is written, so a
/// refusal there has written nothing and the retry must still shrink.
#[test]
fn every_refused_read_of_a_truncation_is_retried_to_the_same_end() {
    assert!(exhaust_reads(&truncate()) > 0);
}

#[test]
fn every_refused_write_of_a_directory_growth_is_undone() {
    assert!(exhaust(&extend_dir()) > 0);
}

#[test]
fn every_refused_write_of_a_long_name_create_is_undone() {
    assert!(exhaust(&long_name_create()) > 0);
}

#[test]
fn every_refused_write_of_a_nested_create_dir_is_undone() {
    assert!(exhaust(&nested_create_dir()) > 0);
}

#[test]
fn every_refused_write_of_a_rename_is_undone() {
    assert!(exhaust(&rename()) > 0);
}

#[test]
fn every_refused_write_of_a_remove_leaves_nothing_behind() {
    assert!(exhaust(&remove()) > 0);
}

#[test]
fn every_refused_write_of_a_remove_dir_leaves_nothing_behind() {
    assert!(exhaust(&remove_dir()) > 0);
}

fn all() -> [Scenario; 10] {
    [
        append(),
        first_allocation(),
        append_and_sync(),
        truncate(),
        extend_dir(),
        long_name_create(),
        nested_create_dir(),
        rename(),
        remove(),
        remove_dir(),
    ]
}

/// A device that stops answering at a write and never answers again, so no
/// repair lands: the machine stopping mid-call, at every write of every call.
///
/// What that leaves is the windows `toyos-fat32`'s repair module names —
/// orphaned clusters no more than the call claims and frees, at most one entry
/// whose copies differ, at most one partial long-name run — and the handle's
/// own window a growth leaves until `flush_meta`. Never a chain into free
/// space, a cluster two entries reach, or a second split entry.
#[test]
fn a_stop_at_any_write_leaves_only_the_named_windows() {
    for s in all() {
        // What these two calls' own write order leaves at a stop, owned by
        // their issue files and not by this repair.
        let own: fn(&Complaint) -> bool = match s.name {
            // issues/filesystem/a-rename-that-stops-between-its-writes-leaves-a-chain-two-entries-reach.md
            "rename" => |c| {
                matches!(
                    c,
                    Complaint::CrossLinked { .. }
                        | Complaint::DotDotCluster { .. }
                        | Complaint::DotEntry { .. }
                )
            },
            // issues/filesystem/a-shrink-frees-clusters-before-the-entry-stops-naming-them.md
            "truncate" => |c| matches!(c, Complaint::ChainTooShort { .. }),
            _ => |_| false,
        };
        let (base, handle) = fixture(&s);
        let moved = allocation_moved(&s, &base, handle);
        for outcome in [Outcome::Refused, Outcome::Landed] {
            for from in 0u64.. {
                let mut fs = Fat32::mount(Faulty::new(base.clone())).expect("mount");
                let mut h = handle;
                fs.device().arm(burst(from, u64::MAX, outcome));
                let _ = (s.call)(&mut fs, &mut h, 0);
                if fs.device().refused == 0 {
                    break;
                }
                let said = complaints(&fs.device().bytes, false);
                let context = format!("{}: stopped at write {from}, {outcome:?}", s.name);
                let splits = said.iter().filter(|c| matches!(c, Complaint::FatMirror { .. })).count();
                assert!(splits <= 1, "{context}: {splits} split entries");
                let orphans: u32 = said
                    .iter()
                    .map(|c| match c {
                        Complaint::LostChain { clusters, .. } => *clusters,
                        _ => 0,
                    })
                    .sum();
                assert!(orphans <= moved, "{context}: {orphans} orphaned clusters, and the call moves {moved}");
                let partial = said
                    .iter()
                    .filter(|c| matches!(c, Complaint::OrphanLongName { .. } | Complaint::LongNameLastFlag { .. }))
                    .count();
                assert!(partial <= 1, "{context}: {partial} partial long-name runs");
                let unnamed: Vec<Complaint> = said
                    .into_iter()
                    .filter(|c| {
                        !matches!(
                            c,
                            Complaint::LostChain { .. }
                                | Complaint::FatMirror { .. }
                                | Complaint::OrphanLongName { .. }
                                | Complaint::LongNameLastFlag { .. }
                                | Complaint::ChainTooLong { .. }
                        ) && !own(c)
                    })
                    .collect();
                assert!(unnamed.is_empty(), "{context}:\n{}", toyos_fat32_check::describe(&unnamed));
            }
        }
    }
}

// ----------------------------------------------- the shape a stop produces

/// Where FAT copy 0 lives on the fixture.
fn active_fat() -> (u64, u64) {
    let lo = fat_offset(0, 0) as u64;
    (lo, lo + (FAT_SECTORS * BYTES_PER_SECTOR) as u64)
}

/// T14 run 139 and `quiesce_leaves_the_volume_whole`'s forced shape, on the
/// host: an append whose claim lands on both FATs, whose link's active-FAT
/// write is refused after its mirror was written, and whose every active-FAT
/// write — the rollback's included — is refused for the next eight attempts,
/// each attempt one retry of the same write as `SYS_FSYNC`'s ladder makes it.
///
/// On the code before this repair the claimed cluster stayed end-of-chain and
/// unreached, and the tenth attempt claimed the next one: the checker's "1
/// cluster(s) ... no directory entry reaches them", with the file's chain
/// stepping over the orphan.
#[test]
fn a_stop_that_refuses_every_retry_for_a_while_leaks_nothing() {
    assert_eq!(NUM_FATS, 2);
    let s = append();
    let (base, handle) = fixture(&s);
    let mut fs = Fat32::mount(Faulty::new(base)).expect("mount");
    let mut h = handle;
    let (lo, hi) = active_fat();
    let active = move |offset: u64, len: usize| offset < hi && offset + len as u64 > lo;

    // Attempt 1 lets its first active-FAT write through — the claim — and
    // refuses every one after it.
    let mut seen = 0u32;
    fs.device().arm(Box::new(move |_, offset, len| {
        if !active(offset, len) {
            return None;
        }
        seen += 1;
        (seen > 1).then_some(Outcome::Refused)
    }));
    assert_eq!(append_two_clusters(&mut fs, &mut h, 0), Err(Error::BudgetExpired));
    assert!(fs.device().refused >= 2, "the link and the rollback's re-drive were both refused");

    // Each later attempt meets the first attempt's repair still unlanded,
    // and says so by name rather than as a refusal of its own.
    for attempt in 2..=9 {
        fs.device().arm(Box::new(move |_, offset, len| active(offset, len).then_some(Outcome::Refused)));
        assert_eq!(append_two_clusters(&mut fs, &mut h, attempt), Err(Error::RepairPending));
        assert!(fs.pending_repair() > 0);
    }
    fs.device().plan = None;
    append_two_clusters(&mut fs, &mut h, 10).expect("attempt 10 on an answering device");
    fs.sync().expect("sync");
    assert_clean(&mut fs, true, "after the tenth attempt");
    (s.verify)(&mut fs, &mut h);
}

/// Every FAT entry that is free on one volume and taken on the other: what a
/// clean run of `s.call` claims and frees, which bounds what a stop inside it
/// can orphan.
fn allocation_moved(s: &Scenario, base: &[u8], handle: Option<File>) -> u32 {
    let mut fs = Fat32::mount(Faulty::new(base.to_vec())).expect("mount");
    let mut h = handle;
    (s.call)(&mut fs, &mut h, 0).expect("the call on an answering device");
    let after = fs.into_device().bytes;
    let taken = |bytes: &[u8], cluster: u32| {
        let at = fat_offset(0, cluster);
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) & 0x0FFF_FFFF != 0
    };
    (2..CLUSTERS + 2).filter(|&c| taken(base, c) != taken(&after, c)).count() as u32
}

// ------------------------------------------------------ the repair's bound

/// The deepest repair there is: a directory with a name of the longest run
/// moved under another longest name into a directory with no free slot, so
/// the insert grows it twice before it writes 21 entries, the erase writes 21
/// more, and the moved directory's `..` is the last write. A stop at each
/// write leaves queued exactly what the call had reached.
#[test]
fn the_deepest_repair_reaches_its_bound_and_no_further() {
    let long = |c: char| c.to_string().repeat(MAX_LFN_CHARS);
    let (from, to) = (format!("from/{}", long('L')), format!("into/{}", long('M')));
    let mut v = Volume::new();
    v.finish();
    let mut fs = Fat32::mount(Faulty::new(v.bytes)).expect("mount");
    fs.create_dir("into", stamp()).expect("mkdir");
    // One 512-byte cluster: `.`, `..` and these fourteen fill it.
    for i in 0..14 {
        let mut f = fs.create(&format!("into/F{i:02}.TXT"), stamp()).expect("fill");
        fs.flush_meta(&mut f, stamp()).expect("flush");
    }
    fs.create_dir("from", stamp()).expect("mkdir");
    fs.create_dir(&from, stamp()).expect("mkdir");
    fs.sync().expect("sync");
    let base = fs.into_device().bytes;

    let mut deepest = 0;
    for stop in 0u64.. {
        let mut fs = Fat32::mount(Faulty::new(base.clone())).expect("mount");
        fs.device().arm(burst(stop, u64::MAX, Outcome::Refused));
        let result = fs.rename(&from, &to);
        if fs.device().refused == 0 {
            result.expect("the rename on an answering device");
            assert!(fs.metadata(&to).expect("moved").is_dir);
            break;
        }
        deepest = deepest.max(fs.pending_repair());
    }
    assert_eq!(deepest, MAX_REPAIR_STEPS, "the deepest stop left {deepest} steps queued");
}

// ------------------------------------------------------ the free count

/// A call that fails on a volume that answered every write keeps an exact
/// free count, so the next sync writes it rather than counting the FAT again.
#[test]
fn a_call_that_refused_nothing_keeps_the_free_count() {
    let mut v = Volume::new();
    v.finish();
    let mut fs = Fat32::mount(Faulty::new(v.bytes)).expect("mount");
    let before = fs.free_bytes().expect("free");
    fs.sync().expect("sync the counted hint");
    let mut f = fs.create(LOG, stamp()).expect("create");
    let too_big = vec![0u8; (before + 512) as usize];
    assert_eq!(fs.write(&mut f, 0, &too_big), Err(Error::NoSpace));
    assert_eq!(fs.pending_repair(), 0);

    fs.device().arm_reads(Box::new(|_| false));
    fs.sync().expect("sync");
    let reads = fs.device().reads;
    assert!(reads < FAT_SECTORS as u64, "the sync read {reads} times: it counted the FAT again");
    assert_eq!(fs.free_bytes().expect("free"), before);
    assert_clean(&mut fs, true, "after the refused write and its sync");
}

/// A rollback the device refuses is what the caller hears, not the error that
/// started it: a write that ran out of space and whose free of the claims was
/// refused answers the refusal, and its repair is still queued.
#[test]
fn an_unlanded_rollback_is_the_answer() {
    let mut v = Volume::new();
    v.finish();
    let mut fs = Fat32::mount(Faulty::new(v.bytes)).expect("mount");
    let before = fs.free_bytes().expect("free");
    let mut f = fs.create(LOG, stamp()).expect("create");
    // Every FAT entry the growth claims is written as a claim, then as a link;
    // the third write to one is its rollback's free.
    let mut seen = std::collections::HashMap::<u64, u32>::new();
    fs.device().arm(Box::new(move |_, offset, _| {
        let n = seen.entry(offset).or_insert(0);
        *n += 1;
        (*n >= 3).then_some(Outcome::Refused)
    }));
    let too_big = vec![0u8; (before + 512) as usize];
    assert_eq!(fs.write(&mut f, 0, &too_big), Err(Error::BudgetExpired));
    assert!(fs.pending_repair() > 0);

    fs.device().plan = None;
    fs.sync().expect("sync lands the rollback");
    assert_eq!(fs.pending_repair(), 0);
    assert_eq!(fs.free_bytes().expect("free"), before);
    assert_clean(&mut fs, true, "after the rollback landed");
}

/// A `remove` answers `Ok` once its name is gone, except for a chain it finds
/// corrupt part way through the free: that is reported, since nothing else
/// records what it could not follow.
#[test]
fn a_remove_reports_a_corrupt_chain_under_the_name_it_erased() {
    let mut v = Volume::new();
    v.finish();
    let mut fs = Fat32::mount(Faulty::new(v.bytes)).expect("mount");
    common::write_new(&mut fs, DOOMED, &common::pattern(5 * 512, 4), stamp());
    let third = {
        let f = fs.extents(DOOMED, 8).expect("extents");
        assert_eq!(f.len(), 1, "one run");
        ((f[0].offset as usize - spec_volume::cluster_offset(2)) / spec_volume::BYTES_PER_CLUSTER) as u32 + 2 + 2
    };
    // Cluster 1 is reserved, so a link to it is a link to nothing.
    for copy in 0..NUM_FATS {
        let at = fat_offset(copy, third);
        let d = fs.device();
        d.bytes[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
        d.resident[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
    }
    assert_eq!(fs.remove(DOOMED), Err(Error::CorruptChain));
    assert!(!fs.exists(DOOMED).expect("exists"), "the name is gone either way");
    assert_eq!(fs.pending_repair(), 0);
}
