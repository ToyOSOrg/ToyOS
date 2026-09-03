//! The two FAT32 partitions this kernel mounts: the boot volume the firmware
//! loaded from, and the log volume beside it.
//!
//! A volume is mounted only when three checks pass: [`gpt::boot_volume`] and
//! [`gpt::log_volume`] name it from the handoff, never by scanning for a FAT
//! signature or a partition type; [`FatDevice`] clamps every read and write to
//! the volume and never the wider partition; and `toyos-fat32` never writes a
//! BPB, so a volume that fails to parse is left untouched.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use hashbrown::HashMap;

use toyos_abi::syscall::SyscallError;
use toyos_fat32::{BlockAccess, Error, Extent, Fat32, FatTime, IoError};

use crate::block::BlockDevice;
use crate::mm::PAGE_BYTES;
use crate::drivers::{usb_storage, xhci};
use crate::file_backing::FileBacking;
use crate::file_cache::{self, FileId};
use crate::fs_rename::{self, Committed, ReplaceRename};
use crate::gpt;
use crate::sync::Lock;
use crate::vfs::FileSystem;

/// The only transfer unit [`BlockDevice`] has, which is `mm::PAGE_SIZE`.
const BLOCK: u64 = crate::mm::PAGE_SIZE;

/// Which of the two partitions a mount is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// The partition firmware loaded the bootloader from.
    Boot,
    /// The log partition, kept off the ESP so macOS mounts it.
    Log,
}

impl Role {
    /// The VFS mount name, which is also the top-level directory.
    pub fn mount(self) -> &'static str {
        match self {
            Role::Boot => "boot",
            Role::Log => "log",
        }
    }

    fn slot(self) -> usize {
        match self {
            Role::Boot => 0,
            Role::Log => 1,
        }
    }

    fn volume(self) -> Option<gpt::Volume> {
        match self {
            Role::Boot => gpt::boot_volume(),
            Role::Log => gpt::log_volume(),
        }
    }
}

impl core::fmt::Display for Role {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.mount())
    }
}

/// Bounded so the extent-list `Vec` stays under `mm::MAX_HEAP_ALLOC`.
const MAX_EXTENTS: usize = 65_536;

const _: () = assert!(core::mem::size_of::<Extent>() == 16);

/// One partition, as a byte range over a device that only does whole 4 KiB
/// blocks; offsets are relative to the partition.
struct FatDevice {
    dev: Box<dyn BlockDevice>,
    /// Where the partition starts, in bytes from the start of the device.
    start: u64,
    /// How many bytes it has.
    len: u64,
    /// On the heap: the deepest caller is the idle loop, whose 16 KiB stack
    /// has no guard page.
    scratch: Vec<u8>,
    /// [`RESIDENT_BLOCKS`] blocks, each tagged with the block it holds.
    resident: Vec<u8>,
    tags: [Option<u64>; RESIDENT_BLOCKS],
    /// Round-robin: the access pattern touches every resident block per
    /// operation, so recency ranks nothing.
    next_victim: usize,
}

/// Blocks [`FatDevice`] keeps resident: enough for one append's FAT, mirror
/// FAT, directory, FSInfo and data blocks.
const RESIDENT_BLOCKS: usize = 8;

/// One function and not `From`: this crosses the crate boundary in the
/// direction that has no orphan.
fn as_io_error(e: crate::block::BlockError) -> IoError {
    match e {
        crate::block::BlockError::Device => IoError::Device,
        crate::block::BlockError::BudgetExpired => IoError::BudgetExpired,
    }
}

impl FatDevice {
    /// The device byte offset `offset` names, or [`IoError::Device`] past the
    /// partition.
    fn locate(&self, offset: u64, len: usize) -> Result<u64, IoError> {
        let end = offset.checked_add(len as u64).ok_or(IoError::Device)?;
        if end > self.len {
            return Err(IoError::Device);
        }
        Ok(self.start + offset)
    }

    fn slot_of(&self, block: u64) -> Option<usize> {
        self.tags.iter().position(|&t| t == Some(block))
    }

    /// Leave `block` in `scratch`, reading it only if it is not already here.
    fn load(&mut self, block: u64) -> Result<(), IoError> {
        if let Some(slot) = self.slot_of(block) {
            let at = slot * BLOCK as usize;
            self.scratch.copy_from_slice(&self.resident[at..at + BLOCK as usize]);
            return Ok(());
        }
        let Self { dev, scratch, .. } = self;
        dev.read_blocks(block, 1, scratch).map_err(as_io_error)?;
        self.retain(block);
        Ok(())
    }

    /// Record `scratch` as this device's `block`; only called where the
    /// device already holds those bytes.
    fn retain(&mut self, block: u64) {
        let slot = self.slot_of(block).unwrap_or_else(|| {
            let s = self.next_victim;
            self.next_victim = (s + 1) % RESIDENT_BLOCKS;
            s
        });
        let at = slot * BLOCK as usize;
        self.resident[at..at + BLOCK as usize].copy_from_slice(&self.scratch);
        self.tags[slot] = Some(block);
    }

    fn forget(&mut self, first: u64, count: u64) {
        for tag in &mut self.tags {
            if tag.is_some_and(|b| b >= first && b < first + count) {
                *tag = None;
            }
        }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let base = self.locate(offset, buf.len())?;
        let mut done = 0usize;
        while done < buf.len() {
            let at = base + done as u64;
            let block = at / BLOCK;
            let within = (at % BLOCK) as usize;
            let left = buf.len() - done;
            if within == 0 && left >= BLOCK as usize {
                let count = left / BLOCK as usize;
                let end = done + count * BLOCK as usize;
                self.dev
                    .read_blocks(block, count as u32, &mut buf[done..end])
                    .map_err(as_io_error)?;
                done = end;
            } else {
                let n = (BLOCK as usize - within).min(left);
                self.load(block)?;
                buf[done..done + n].copy_from_slice(&self.scratch[within..within + n]);
                done += n;
            }
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError> {
        let base = self.locate(offset, buf.len())?;
        let mut done = 0usize;
        while done < buf.len() {
            let at = base + done as u64;
            let block = at / BLOCK;
            let within = (at % BLOCK) as usize;
            let left = buf.len() - done;
            if within == 0 && left >= BLOCK as usize {
                let count = left / BLOCK as usize;
                let end = done + count * BLOCK as usize;
                self.forget(block, count as u64);
                self.dev
                    .write_blocks(block, count as u32, &buf[done..end])
                    .map_err(as_io_error)?;
                done = end;
            } else {
                // Bytes this request doesn't cover belong to another file or
                // the partition table; preserved by the read-modify-write.
                let n = (BLOCK as usize - within).min(left);
                self.load(block)?;
                self.scratch[within..within + n].copy_from_slice(&buf[done..done + n]);
                let Self { dev, scratch, .. } = self;
                dev.write_blocks(block, 1, scratch).map_err(as_io_error)?;
                self.retain(block);
                done += n;
            }
        }
        Ok(())
    }
}

/// Lock order: VFS → here → `XHCI`; never the other way, and never two of
/// these at once.
static VOLUMES: [Lock<Option<FatDevice>>; 2] = [Lock::new(None), Lock::new(None)];

fn device(role: Role) -> &'static Lock<Option<FatDevice>> {
    &VOLUMES[role.slot()]
}

/// One [`VOLUMES`] entry in the shape `toyos-fat32` asks for.
pub struct FatVolume {
    role: Role,
    bytes: u64,
}

impl BlockAccess for FatVolume {
    fn capacity(&self) -> u64 {
        self.bytes
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let mut guard = device(self.role).lock();
        let served = guard.as_mut().ok_or(IoError::Device)?.read_at(offset, buf);
        if injected_read_failure(self.role) {
            // Zeroed to match what a real failed read leaves behind.
            buf.fill(0);
            log!("{}-volume: read of {} B at volume offset {offset} failed",
                self.role, buf.len());
            return Err(IoError::Device);
        }
        served
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError> {
        // `fat-mirror-write-refuse` actuator: refused before the write is
        // issued, like a spent `block::OPERATION` — the volume stays untouched.
        #[cfg(feature = "boot-actuators")]
        if self.role == Role::Log && mirror_refuse::should_refuse(offset, buf.len()) {
            log!(
                "log-volume: fat-mirror-write-refuse: refusing the FAT-1 mirror write of a \
                 drain flush at volume offset {offset} as a budget expiry"
            );
            return Err(IoError::BudgetExpired);
        }
        let mut guard = device(self.role).lock();
        guard.as_mut().ok_or(IoError::Device)?.write_at(offset, buf)
    }

    fn flush(&mut self) -> Result<(), IoError> {
        let mut guard = device(self.role).lock();
        guard.as_mut().ok_or(IoError::Device)?.dev.flush().map_err(as_io_error)
    }
}

/// Whether a page re-read succeeds; the read is still issued and only its
/// verdict replaced, so a broken transport cannot hide behind it.
fn fat_backing_reads() -> bool {
    !crate::actuator::fat_backing_read_fails()
}

/// Whether the boot volume may still answer a filesystem read once mounted;
/// the metadata-path sibling of [`fat_backing_reads`], which is the
/// page-fault path.
fn boot_volume_reads() -> bool {
    !crate::actuator::fat_boot_reads_fail()
}

/// Set once [`mount`] has installed the boot volume, so the injection cannot
/// fire during mount itself.
static BOOT_MOUNTED: AtomicBool = AtomicBool::new(false);

// Boot only: the log volume carries the kernel's own log, and failing its
// reads would take the channel the evidence arrives on.
fn injected_read_failure(role: Role) -> bool {
    !boot_volume_reads() && role == Role::Boot && BOOT_MOUNTED.load(Ordering::Relaxed)
}

/// Armed only around the leak-rollback self-test's reopen, to fail [`FatFs::backing`].
#[cfg(feature = "boot-actuators")]
static SELFTEST_BACKING_FAIL: AtomicBool = AtomicBool::new(false);

/// Self-test hook: make [`FatFs::backing`] fail like a transient device error.
#[cfg(feature = "boot-actuators")]
pub(crate) fn selftest_fail_backing(on: bool) {
    SELFTEST_BACKING_FAIL.store(on, Ordering::Relaxed);
}

/// Which byte ranges hold one file's data, shared by every [`FatBacking`] for
/// that name so an unlink revokes all of them at once.
struct FatExtents {
    /// `None` once the volume has the clusters back.
    runs: Lock<Option<Vec<Extent>>>,
}

/// Where one file offset is, once [`FatExtents`] has been asked.
enum Located {
    /// Volume byte offset, and how many contiguous bytes follow.
    Run(u64, u64),
    /// Past the extent list: the file has no data there; zeros are its own bytes.
    Hole,
}

impl FatExtents {
    fn new(runs: Vec<Extent>) -> Arc<Self> {
        Arc::new(Self { runs: Lock::new(Some(runs)) })
    }

    /// Give the ranges up; every read through a sharing backing fails from
    /// here on.
    fn revoke(&self) {
        *self.runs.lock() = None;
    }

    /// Take a fresh [`Fat32::extents`] list; write-through so every sharer's
    /// cell sees it.
    fn refresh(&self, runs: Vec<Extent>) {
        *self.runs.lock() = Some(runs);
    }

    /// Give up bytes past `len`; runs before `Fat32::set_len` frees the tail
    /// so no backing ever names a reissued cluster.
    fn truncate_to(&self, len: u64) {
        let mut guard = self.runs.lock();
        let Some(runs) = guard.as_mut() else { return };
        let mut kept = 0u64;
        runs.retain_mut(|run| {
            let room = len.saturating_sub(kept);
            run.len = run.len.min(room);
            kept += run.len;
            run.len != 0
        });
    }

    /// Where `file_offset` is on the volume, or `None` once the file is gone;
    /// the lock is a leaf, never held across a device read.
    fn locate(&self, file_offset: u64) -> Option<Located> {
        let guard = self.runs.lock();
        let runs = guard.as_ref()?;
        let mut base = 0u64;
        for run in runs {
            if file_offset < base + run.len {
                let within = file_offset - base;
                return Some(Located::Run(run.offset + within, run.len - within));
            }
            base += run.len;
        }
        Some(Located::Hole)
    }
}

/// The `fat-mirror-write-refuse` actuator: refuses the first two FAT-1 mirror
/// writes of a drain flush, as a budget expiry.
#[cfg(feature = "boot-actuators")]
mod mirror_refuse {
    use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

    /// The log volume's FAT-1 byte range, volume-relative, captured at mount.
    static LO: AtomicU64 = AtomicU64::new(0);
    static HI: AtomicU64 = AtomicU64::new(0);
    /// Set while `writeback`'s drain holds a `flush_file` open, so refusal
    /// targets only the drain path.
    static IN_DRAIN: AtomicBool = AtomicBool::new(false);
    /// How many drain-flush mirror writes have been refused so far.
    static REFUSED: AtomicU32 = AtomicU32::new(0);

    /// Two, not one: a single refusal only reaches the retry ladder's attempt
    /// 1, which merely yields — attempt 2 is the one that parks.
    const REFUSALS: u32 = 2;

    pub fn capture(lo: u64, hi: u64) {
        LO.store(lo, Ordering::Relaxed);
        HI.store(hi, Ordering::Relaxed);
    }

    pub fn set_in_drain(on: bool) {
        IN_DRAIN.store(on, Ordering::Relaxed);
    }

    /// Whether this log-volume write should be refused: actuator armed,
    /// mid-drain, under [`REFUSALS`], and overlapping the mirror FAT.
    pub fn should_refuse(offset: u64, len: usize) -> bool {
        if !crate::actuator::fat_mirror_write_refuse()
            || !IN_DRAIN.load(Ordering::Relaxed)
            || REFUSED.load(Ordering::Relaxed) >= REFUSALS
        {
            return false;
        }
        let (lo, hi) = (LO.load(Ordering::Relaxed), HI.load(Ordering::Relaxed));
        let end = offset + len as u64;
        if hi > lo && offset < hi && end > lo {
            REFUSED.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        false
    }
}

/// The `fat-flush-meta-refuse` actuator: refuses one file's second
/// directory-entry write, which is the last step of a flush and the one whose
/// failure leaves the pages before it already written and settled. The second
/// and not the first, because the first is that file's seed being made durable.
#[cfg(feature = "boot-actuators")]
mod meta_refuse {
    use core::sync::atomic::{AtomicU32, Ordering};

    /// Mirrored in `tests/toyos-rust-tests/src/bin/writeback_durability.rs`.
    const STAGED: &str = "wb-retry.bin";
    /// Which of this file's metadata writes is refused, counting from zero.
    const AT: u32 = 1;
    static SEEN: AtomicU32 = AtomicU32::new(0);

    pub fn should_refuse(name: &str) -> bool {
        crate::actuator::fat_flush_meta_refuse()
            && name.ends_with(STAGED)
            && SEEN.fetch_add(1, Ordering::Relaxed) == AT
    }
}

/// Mark that a write-back drain flush is in progress, so the mirror-refuse
/// actuator targets the drain path and not `SYS_FSYNC`.
#[cfg(feature = "boot-actuators")]
pub(crate) fn enter_drain_flush() {
    mirror_refuse::set_in_drain(true);
}

#[cfg(feature = "boot-actuators")]
pub(crate) fn leave_drain_flush() {
    mirror_refuse::set_in_drain(false);
}

/// A file's byte ranges, read without going back through the filesystem;
/// `size` is a snapshot, not a view of the live length.
struct FatBacking {
    role: Role,
    extents: Arc<FatExtents>,
    size: u64,
}

impl FileBacking for FatBacking {
    fn read_page(&self, file_offset: u64, buf: &mut [u8; PAGE_BYTES]) -> crate::block::BlockResult {
        buf.fill(0);
        if file_offset >= self.size {
            return Ok(());
        }
        let valid = (4096u64).min(self.size - file_offset) as usize;
        let mut done = 0usize;
        // A page can span multiple runs when the cluster size is under 4096.
        while done < valid {
            // Re-asked every run, so a mid-page revocation stops the rest of
            // it here.
            let Some(found) = self.extents.locate(file_offset + done as u64) else {
                // Clusters already back in the allocator; reading them would
                // serve another file's data.
                log!("{}-volume: read through a backing whose file was deleted", self.role);
                return Err(crate::block::BlockError::Device);
            };
            let Located::Run(at, run) = found else {
                // Past the extent list: a hole; the zeros already in `buf`
                // are correct.
                return Ok(());
            };
            let n = (run as usize).min(valid - done);
            let mut guard = device(self.role).lock();
            let Some(dev) = guard.as_mut() else {
                // Not silent, unlike elsewhere: `serving zeros` is the string
                // a triage greps for.
                log!("{}-volume: not mounted; serving zeros", self.role);
                return Err(crate::block::BlockError::Device);
            };
            let served = dev.read_at(at, &mut buf[done..done + n]);
            if !fat_backing_reads() {
                // Zeroed to match what a real failed read leaves behind.
                buf[done..done + n].fill(0);
            }
            if served.is_err() || !fat_backing_reads() {
                log!("{}-volume: read of {n} B at volume offset {at} failed; serving zeros",
                    self.role);
                return Err(crate::block::BlockError::Device);
            }
            drop(guard);
            done += n;
        }
        Ok(())
    }

    fn file_size(&self) -> u64 {
        self.size
    }
}

/// Per-open-file state: the path, and the crate's handle caching directory
/// location and chain position.
struct OpenFile {
    name: String,
    file: toyos_fat32::File,
}

/// VFS adapter for one of the two partitions; a file's identity is its path,
/// which `by_name` keys on.
pub struct FatFs {
    role: Role,
    fs: Fat32<FatVolume>,
    open: HashMap<FileId, OpenFile>,
    by_name: BTreeMap<String, FileId>,
    /// The one [`FatExtents`] every backing for a name shares; keyed by name
    /// because `open_backing` hands one out without opening a file.
    extents: BTreeMap<String, Weak<FatExtents>>,
}

/// What to stamp on an entry: reads `clock` directly, in local time as FAT
/// requires — the VFS's `mtime` is nanoseconds since boot, not a time of day.
fn now() -> FatTime {
    crate::clock::local_secs().map_or(FatTime::EPOCH, FatTime::from_unix_secs)
}

/// What one of `toyos-fat32`'s errors means to the [`FileSystem`] caller;
/// exhaustive so a new variant fails to compile here.
fn as_syscall_error(e: Error) -> SyscallError {
    match e {
        Error::NotFound => SyscallError::NotFound,
        Error::AlreadyExists => SyscallError::AlreadyExists,
        // Structural corruption reads as `Io`, not `NotFound`: the volume
        // can't say what's there, not that nothing is.
        Error::Io
        | Error::NotFat32
        | Error::Truncated
        | Error::CorruptChain
        | Error::CorruptDirectory => SyscallError::Io,
        // Not `Io`: the volume wasn't touched; `block::OPERATION` (the
        // caller's bound) expired. Reaches userland as `WouldBlock`.
        Error::BudgetExpired => SyscallError::WouldBlock,
        // Not `NotFound`: the name resolves; the operation just isn't defined
        // for what it names.
        Error::NotADirectory | Error::IsADirectory | Error::DirectoryNotEmpty => {
            SyscallError::InvalidArgument
        }
        Error::InvalidName => SyscallError::InvalidArgument,
        // `TooLarge` is FAT32's 4 GiB field limit, not a full volume, but both
        // mean no room.
        Error::NoSpace | Error::TooLarge => SyscallError::ResourceExhausted,
        Error::LimitExceeded => SyscallError::ResourceExhausted,
    }
}

/// Log what the volume said and return its code; `NotFound` is skipped so
/// opening a missing path doesn't write the log it lives on.
fn refused(role: Role, op: &str, name: &str, e: Error) -> SyscallError {
    if e != Error::NotFound {
        log!("{role}-volume: {op} of {name}: {e}");
    }
    as_syscall_error(e)
}

impl FatFs {
    fn new(role: Role, fs: Fat32<FatVolume>) -> Self {
        Self {
            role,
            fs,
            open: HashMap::new(),
            by_name: BTreeMap::new(),
            extents: BTreeMap::new(),
        }
    }

    fn backing(&mut self, name: &str) -> Result<Arc<dyn FileBacking>, SyscallError> {
        let role = self.role;
        // Self-test: the transient-device-error trigger of the reopen leak.
        #[cfg(feature = "boot-actuators")]
        if SELFTEST_BACKING_FAIL.load(Ordering::Relaxed) {
            return Err(SyscallError::Io);
        }
        let size = self.fs.metadata(name).map_err(|e| refused(role, "metadata", name, e))?.len;
        let runs = self
            .fs
            .extents(name, MAX_EXTENTS)
            .map_err(|e| refused(role, "extents", name, e))?;
        let extents = self.extents_for(name, runs);
        Ok(Arc::new(FatBacking { role, extents, size }))
    }

    /// The cell every backing for `name` reads through, carrying the
    /// just-read extent list.
    fn extents_for(&mut self, name: &str, runs: Vec<Extent>) -> Arc<FatExtents> {
        // Swept here, not on a timer: this call is the only place the map
        // grows.
        self.extents.retain(|_, weak| weak.strong_count() > 0);

        if let Some(live) = self.extents.get(name).and_then(Weak::upgrade) {
            live.refresh(runs);
            return live;
        }
        let cell = FatExtents::new(runs);
        self.extents.insert(String::from(name), Arc::downgrade(&cell));
        cell
    }

    /// The live cell for `name`, if some backing still holds one.
    fn live_extents(&self, name: &str) -> Option<Arc<FatExtents>> {
        self.extents.get(name).and_then(Weak::upgrade)
    }

    /// Give up every backing reading `name`'s clusters; must be called before
    /// the clusters are freed, since [`FatBacking::read_page`] takes no VFS
    /// lock.
    fn revoke(&mut self, name: &str) {
        if let Some(cell) = self.extents.remove(name).as_ref().and_then(Weak::upgrade) {
            cell.revoke();
        }
    }

    /// Make sure every directory on the way to `name` exists: a create may
    /// land under a path no `mkdir` ever touched.
    fn ensure_parent(&mut self, name: &str, time: FatTime) -> Result<(), SyscallError> {
        let Some((parent, _)) = name.rsplit_once('/') else { return Ok(()) };
        let role = self.role;
        self.fs.create_dir_all(parent, time).map_err(|e| refused(role, "mkdir -p", parent, e))
    }
}

/// FAT cannot replace an entry in one step. The backend renames the destination
/// aside, moves the source, and returns the still-live displaced entry; only
/// `release` may retire its in-memory state and free its clusters.
impl ReplaceRename for FatFs {
    type Displaced = (toyos_fat32::Replaced, Option<FileId>);

    fn source_present(&mut self, old: &str) -> Result<bool, SyscallError> {
        let role = self.role;
        self.fs.exists(old).map_err(|e| refused(role, "exists", old, e))
    }

    fn same_object(&mut self, old: &str, new: &str) -> Result<bool, SyscallError> {
        // Identity is the entry's location: FAT names one entry by two strings.
        let role = self.role;
        self.fs.same_entry(old, new).map_err(|e| refused(role, "same_entry", old, e))
    }

    fn commit(
        &mut self,
        old: &str,
        new: &str,
    ) -> Result<Committed<Self::Displaced>, SyscallError> {
        let role = self.role;
        let displaced = self.by_name.get(new).copied();
        let replaced = self.fs.replace_rename(old, new).map_err(|e| {
            // Nothing on the volume names it, so this line is the only record
            // that the destination's data is alive and unreachable.
            if let Some(stranded) = &e.stranded {
                log!("{role}-volume: {new} could not be put back and is under {stranded}");
            }
            refused(role, "replace rename", old, e.cause)
        })?;
        Ok(Committed::new((replaced, displaced)))
    }

    fn release(
        &mut self,
        old: &str,
        new: &str,
        committed: Committed<Self::Displaced>,
    ) -> Result<(), SyscallError> {
        let (replaced, displaced) = committed.into_displaced();
        let had_destination = replaced.displaced();
        if let Some(file_id) = displaced {
            let _ = file_cache::mark_deleted(file_id);
            self.open.remove(&file_id);
            self.by_name.remove(new);
        }
        if had_destination {
            // Backings under `new` still read the displaced file's clusters.
            self.revoke(new);
        }

        let role = self.role;
        let released = self
            .fs
            .release_replaced(replaced)
            .map_err(|e| refused(role, "release replaced", new, e));

        // Re-key, not revoke: the source's data did not move, so backings under
        // the old name still read it.
        if let Some(file_id) = self.by_name.remove(old) {
            self.by_name.insert(String::from(new), file_id);
            if let Some(info) = self.open.get_mut(&file_id) {
                info.name = String::from(new);
            }
        }
        if let Some(cell) = self.extents.remove(old) {
            self.extents.insert(String::from(new), cell);
        }
        released
    }
}

impl FileSystem for FatFs {
    /// The `limit` bound is honoured before each push, not after — unlike the
    /// bcachefs adapters.
    fn list(&mut self, dir: &str, limit: usize) -> Result<Vec<(String, u64)>, SyscallError> {
        let role = self.role;
        self.fs.walk(dir, limit).map_err(|e| refused(role, "list", dir, e))
    }

    fn file_mtime(&mut self, name: &str) -> Result<u64, SyscallError> {
        let role = self.role;
        self.fs
            .metadata(name)
            .map(|m| m.modified_unix)
            .map_err(|e| refused(role, "metadata", name, e))
    }

    /// Always `Ok(None)`: FAT32 has no symlink representation.
    fn read_link(&mut self, _name: &str) -> Result<Option<String>, SyscallError> {
        Ok(None)
    }

    fn open_file(&mut self, name: &str) -> Result<(FileId, Option<Arc<dyn FileBacking>>), SyscallError> {
        if let Some(&file_id) = self.by_name.get(name) {
            let held = file_cache::open(file_id);
            let backing = self.backing(name)?;
            held.commit();
            return Ok((file_id, Some(backing)));
        }
        let role = self.role;
        let file = self.fs.open(name).map_err(|e| refused(role, "open", name, e))?;
        let size = file.len();
        let backing = self.backing(name)?;

        let file_id = file_cache::create_file(true);
        file_cache::set_size(file_id, size);
        self.by_name.insert(String::from(name), file_id);
        self.open.insert(file_id, OpenFile { name: String::from(name), file });
        Ok((file_id, Some(backing)))
    }

    fn create(&mut self, name: &str, _mtime: u64) -> Result<FileId, SyscallError> {
        if let Some(&file_id) = self.by_name.get(name) {
            return Ok(file_id);
        }
        let role = self.role;
        let time = now();
        self.ensure_parent(name, time)?;
        let file = match self.fs.create(name, time) {
            Ok(file) => file,
            // Not an error here: `create` also reopens an existing file for
            // writing, deliberately — a create that silently opened somebody
            // else's file is how a caller comes to believe it owns bytes it
            // does not.
            Err(Error::AlreadyExists) => {
                self.fs.open(name).map_err(|e| refused(role, "open", name, e))?
            }
            Err(e) => return Err(refused(role, "create", name, e)),
        };
        let file_id = file_cache::create_file(true);
        file_cache::set_size(file_id, file.len());
        self.by_name.insert(String::from(name), file_id);
        self.open.insert(file_id, OpenFile { name: String::from(name), file });
        Ok(file_id)
    }

    fn close_file(&mut self, file_id: FileId) {
        if let Some(info) = self.open.remove(&file_id) {
            self.by_name.remove(&info.name);
        }
    }

    /// Unlink, giving up both the write handle and every read backing
    /// unconditionally — a held-open handle must not read or write the next
    /// file's data.
    fn delete(&mut self, name: &str) -> Result<(), SyscallError> {
        if let Some(file_id) = self.by_name.remove(name) {
            let _ = file_cache::mark_deleted(file_id);
            self.open.remove(&file_id);
        }
        self.revoke(name);
        let role = self.role;
        self.fs.remove(name).map_err(|e| refused(role, "delete", name, e))
    }

    fn rename(&mut self, old: &str, new: &str) -> Result<(), SyscallError> {
        fs_rename::replace_rename(self, old, new)
    }

    fn create_dir(&mut self, name: &str) -> Result<(), SyscallError> {
        let role = self.role;
        self.fs.create_dir(name, now()).map_err(|e| refused(role, "mkdir", name, e))
    }

    fn remove_dir(&mut self, name: &str) -> Result<(), SyscallError> {
        let role = self.role;
        self.fs.remove_dir(name).map_err(|e| refused(role, "rmdir", name, e))
    }

    fn write_page(
        &mut self,
        file_id: FileId,
        page_idx: u32,
        data: &[u8; PAGE_BYTES],
    ) -> Result<(), SyscallError> {
        let Self { role, fs, open, .. } = self;
        let role = *role;
        let info = open.get_mut(&file_id).ok_or(SyscallError::NotFound)?;
        match fs.write(&mut info.file, page_idx as u64 * 4096, data) {
            Ok(()) => Ok(()),
            Err(e) => Err(refused(role, "write", &info.name, e)),
        }
    }

    /// `set_len` frees the clusters and the cell stops naming them, so a later
    /// grow zero-fills what it takes back rather than serving the old tail.
    fn truncate_to(&mut self, file_id: FileId, size: u64, _mtime: u64) -> Result<(), SyscallError> {
        let role = self.role;
        let known = self.open.get(&file_id).ok_or(SyscallError::NotFound)?;
        let (name, was) = (known.name.clone(), known.file.len());
        if was <= size {
            return Ok(());
        }
        if let Some(cell) = self.live_extents(&name) {
            cell.truncate_to(size);
        }
        let Self { fs, open, .. } = self;
        let info = open.get_mut(&file_id).ok_or(SyscallError::NotFound)?;
        fs.set_len(&mut info.file, size).map_err(|e| refused(role, "set_len", &name, e))
    }

    /// Record the real length and re-derive the backing; a shrink truncates
    /// the extent list before `set_len` frees the tail, so no backing ever
    /// names a reissued cluster.
    fn update_metadata(
        &mut self,
        file_id: FileId,
        size: u64,
        _mtime: u64,
    ) -> Result<(), SyscallError> {
        let role = self.role;
        let time = now();
        let name = {
            let known = self.open.get(&file_id).ok_or(SyscallError::NotFound)?;
            let (name, was) = (known.name.clone(), known.file.len());
            if was > size {
                if let Some(cell) = self.live_extents(&name) {
                    cell.truncate_to(size);
                }
            }
            let Self { fs, open, .. } = self;
            let info = open.get_mut(&file_id).ok_or(SyscallError::NotFound)?;
            if info.file.len() != size {
                fs.set_len(&mut info.file, size)
                    .map_err(|e| refused(role, "set_len", &info.name, e))?;
            }
            // Refused before the entry is written, like a spent `block::OPERATION`:
            // the pages this flush already wrote and settled stay where they are.
            #[cfg(feature = "boot-actuators")]
            if meta_refuse::should_refuse(&info.name) {
                log!(
                    "{role}-volume: fat-flush-meta-refuse: refusing the directory-entry write \
                     of {} as a budget expiry",
                    info.name
                );
                return Err(SyscallError::WouldBlock);
            }
            fs.flush_meta(&mut info.file, time)
                .map_err(|e| refused(role, "flush_meta", &info.name, e))?;
            name
        };
        // A failure here only costs evictability; the write itself is
        // already on the volume.
        match self.backing(&name) {
            Ok(backing) => file_cache::set_backing(file_id, backing),
            Err(_) => log!("{role}-volume: {name} was written but has no re-readable extent list"),
        }
        Ok(())
    }

    /// Always an error: FAT32 has no symlink representation to write.
    fn create_symlink(&mut self, _name: &str, _target: &str) -> Result<(), SyscallError> {
        Err(SyscallError::NotSupported)
    }

    /// Error returned, not logged via [`refused`]: a log write here would be
    /// more pending content for the next sync.
    fn sync(&mut self) -> Result<(), SyscallError> {
        self.fs.sync().map_err(as_syscall_error)
    }

    fn open_backing(&mut self, name: &str) -> Result<Arc<dyn FileBacking>, SyscallError> {
        self.backing(name)
    }

    fn cached_file_id(&mut self, name: &str) -> Option<FileId> {
        self.by_name.get(name).copied()
    }
}

/// Ask every USB disk for the partitions this kernel was given, retrying
/// while the boot volume is still not found.
pub fn probe_boot_disks() {
    let deadline = crate::clock::nanos_since_boot() + xhci::PORT_SETTLE_CEILING.nanos();
    let mut probed = 0;
    loop {
        probed = probe_announced(probed);
        // Nothing further can change: no partition named, resolved, or
        // ambiguity no device can repair.
        if gpt::boot_partition().is_none() || !gpt::boot_volume_still_possible() {
            return;
        }
        if crate::clock::nanos_since_boot() >= deadline {
            log!(
                "usb-storage: {probed} disk(s) on this machine and none carries the boot \
                 partition after {} ms of looking — this boot has no /boot and no /log",
                xhci::PORT_SETTLE_CEILING.duration().millis()
            );
            return;
        }
        // Paced, not spun: one MMIO read per port under the controller lock,
        // on physical hardware.
        let next = crate::clock::nanos_since_boot() + xhci::PORT_POLL.nanos();
        while crate::clock::nanos_since_boot() < next {
            core::hint::spin_loop();
        }
        xhci::recheck_ports();
    }
}

/// Probe every disk announced since the last call; indices are stable and
/// dense, so a disk is never probed twice.
fn probe_announced(mut probed: usize) -> usize {
    let count = usb_storage::count();
    while probed < count {
        let index = probed;
        probed += 1;
        // One `open` call for both the handle and the block size; the disk
        // carries its own geometry.
        let Some(mut disk) = usb_storage::open(index) else {
            log!(
                "usb-storage: disk {index} was announced and was gone again before its partition \
                 table could be read — not probed"
            );
            continue;
        };
        let lba_bytes = disk.logical_block_bytes();
        gpt::probe(&mut disk, lba_bytes);
    }
    probed
}

/// The bound disk carrying `id`, or `None` when no driver here serves it
/// (only USB today; see `issues/build/page-cache-owns-one-device.md`).
fn device_carrying(id: crate::block::DeviceId) -> Option<Box<dyn BlockDevice>> {
    (0..usb_storage::count())
        .filter_map(usb_storage::open)
        .find(|disk| disk.device_id() == id)
        .map(|disk| Box::new(disk) as Box<dyn BlockDevice>)
}

/// Open the partition `role` names, or `None` for any of several ordinary
/// non-matches — never a reason to log.
pub fn mount(role: Role) -> Option<FatFs> {
    let volume = role.volume()?;

    let Some(dev) = device_carrying(volume.device) else {
        log!(
            "{role}-volume: the partition is on device {} and no driver here can open it",
            volume.device
        );
        return None;
    };

    let lba = volume.lba_bytes as u64;
    let start = volume.start_lba.checked_mul(lba)?;
    let len = volume.blocks.checked_mul(lba)?;
    let device_bytes = dev.block_count().checked_mul(BLOCK)?;
    if start.checked_add(len)? > device_bytes {
        log!(
            "{role}-volume: the table puts the partition at {start}+{len} on a device of \
             {device_bytes} bytes — refusing to mount past the end of it"
        );
        return None;
    }

    *device(role).lock() = Some(FatDevice {
        dev,
        start,
        len,
        scratch: vec![0u8; BLOCK as usize],
        resident: vec![0u8; RESIDENT_BLOCKS * BLOCK as usize],
        tags: [None; RESIDENT_BLOCKS],
        next_victim: 0,
    });

    // `probe` only reads, so the bound can tighten from partition to volume
    // before any write; this only ever shrinks.
    let mut volume = FatVolume { role, bytes: len };
    let geom = match Fat32::probe(&mut volume) {
        Ok(geom) => geom,
        Err(e) => {
            log!("{role}-volume: the partition holds no FAT32 this kernel can mount: {e}");
            *device(role).lock() = None;
            return None;
        }
    };
    let volume_bytes = geom.total_sectors as u64 * geom.bytes_per_sector as u64;
    volume.bytes = volume_bytes;
    if let Some(mounted) = device(role).lock().as_mut() {
        mounted.len = volume_bytes;
    }

    // FAT-1 mirror range for the `fat-mirror-write-refuse` actuator; empty
    // and inert on a one-FAT volume.
    #[cfg(feature = "boot-actuators")]
    if role == Role::Log && geom.num_fats >= 2 {
        let one_fat = geom.fat_sectors as u64 * geom.bytes_per_sector as u64;
        let lo = geom.fat_base_offset(1);
        mirror_refuse::capture(lo, lo + one_fat);
    }

    match Fat32::mount(volume) {
        Ok(fs) => {
            log!(
                "{role}-volume: partition mounted, {volume_bytes} bytes of a {len}-byte partition \
                 at device offset {start}, {}-byte sectors, {}-byte clusters, {} clusters",
                geom.bytes_per_sector,
                geom.bytes_per_cluster(),
                geom.cluster_count
            );
            if role == Role::Boot {
                BOOT_MOUNTED.store(true, Ordering::Relaxed);
            }
            Some(FatFs::new(role, fs))
        }
        Err(e) => {
            log!("{role}-volume: the partition holds no FAT32 this kernel can mount: {e}");
            *device(role).lock() = None;
            None
        }
    }
}
