use alloc::boxed::Box;
use alloc::collections::btree_map::Entry;
use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
use alloc::sync::Arc;

use crate::block;
use crate::durability::{Owed, Settlement};
use crate::file_backing::FileBacking;
use crate::sync::Lock;
use crate::user_ptr::{ByteSource, UserBytesMut};

pub type FileId = u64;

/// `mm::PAGE_SIZE`, in the width this file's arrays and slices index by.
const PAGE_SIZE: usize = crate::mm::PAGE_SIZE as usize;

struct CachedPage {
    data: Box<[u8; PAGE_SIZE]>,
    /// Dirty state as generations, settled only against what a flush copied.
    dirt: Owed,
    /// CLOCK's second-chance bit: set on every hit, cleared when the sweep passes it over.
    referenced: bool,
}

impl CachedPage {
    fn is_dirty(&self) -> bool {
        self.dirt.is_owed()
    }
}

struct CachedFile {
    pages: BTreeMap<u32, CachedPage>,
    size: u64,
    evictable: bool,
    /// Where an evicted page comes back from; `None` means a dropped page cannot be re-read.
    backing: Option<Arc<dyn FileBacking>>,
    ref_count: u32,
    deleted: bool,
    /// Pins the file alive at `ref_count == 0` for the write-back queue; cleared only by [`finish_writeback`].
    teardown_owed: bool,
    /// The file, not any one handle, owes a flush; settled only by [`settle_file`] on a flush that succeeded.
    dirt: Owed,
    /// The smallest size a shrink has taken this file to since the last settled
    /// metadata write; `None` when none has. Everything at or above it was
    /// discarded, so a page missing from here is zeros and never the backing's.
    shrunk_to: Option<u64>,
}

impl CachedFile {
    /// Whether this file's pages are a copy of disk data — only those count toward the budget or are evictable.
    fn is_cache(&self) -> bool {
        self.evictable && self.backing.is_some()
    }
}

struct FileCache {
    files: BTreeMap<FileId, CachedFile>,
    next_id: u64,
    /// Resident pages belonging to files that satisfy `is_cache`.
    cached_pages: usize,
    max_pages: usize,
    evictions: u64,
    /// CLOCK hand, in (file, page) key order; kept across calls so eviction costs one step, not a full scan.
    hand: (FileId, u32),
    /// The over-budget state has been said; cleared when residency returns within budget, so an episode costs one line.
    over_said: bool,
}

static FILE_CACHE: Lock<FileCache> = Lock::new(FileCache {
    files: BTreeMap::new(),
    next_id: 1,
    cached_pages: 0,
    // Zero, not `usize::MAX`: an uninstalled budget must fail loudly, not silently allow everything.
    max_pages: 0,
    evictions: 0,
    hand: (0, 0),
    over_said: false,
});

/// Install the memory budget; must run after the PMM sizes RAM and before any file is opened.
pub fn init() {
    let max_pages = block::file_cache_pages();
    FILE_CACHE.lock().max_pages = max_pages;
    log!("file cache: budget {} pages ({} MiB)", max_pages, max_pages * PAGE_SIZE / (1024 * 1024));
}

/// Allocate a new FileId. The file cache is the sole allocator.
pub fn create_file(evictable: bool) -> FileId {
    let mut cache = FILE_CACHE.lock();
    let id = cache.next_id;
    cache.next_id += 1;
    cache.files.insert(id, CachedFile {
        pages: BTreeMap::new(),
        size: 0,
        evictable,
        backing: None,
        ref_count: 1,
        deleted: false,
        teardown_owed: false,
        dirt: Owed::new(),
        shrunk_to: None,
    });
    id
}

/// Point a file at the store its evicted pages come back from; idempotent across opens of the same file.
pub fn set_backing(file_id: FileId, backing: Arc<dyn FileBacking>) {
    let mut cache = FILE_CACHE.lock();
    let now_governed;
    {
        let Some(file) = cache.files.get_mut(&file_id) else { return };
        let was_cache = file.is_cache();
        file.backing = Some(backing);
        now_governed = if !was_cache && file.is_cache() { file.pages.len() } else { 0 };
    }
    cache.cached_pages += now_governed;
    evict_if_needed(&mut cache);
}

/// Whether an evicted page of this file could be read back; false for tmpfs and for a disk file with no blocks yet.
pub fn has_backing(file_id: FileId) -> bool {
    FILE_CACHE.lock().files.get(&file_id).is_some_and(|f| f.backing.is_some())
}

/// Increment ref_count for one more open, returning a guard that undoes the
/// increment on drop unless committed: a re-open whose backing lookup fails
/// after this must not pin the file. Caller holds the VFS lock, which
/// [`finish_writeback`] also reads `ref_count` under to serialise against teardown.
#[must_use = "commit() once the re-open cannot fail, or the reference is released"]
pub fn open(file_id: FileId) -> crate::rollback::Rollback<impl FnOnce()> {
    {
        let mut cache = FILE_CACHE.lock();
        if let Some(file) = cache.files.get_mut(&file_id) {
            file.ref_count += 1;
        }
    }
    crate::rollback::Rollback::new(move || undo_open(file_id))
}

/// Undo one [`open`]: a re-open decrements a count another handle or a pending teardown keeps, never orphaning the file.
fn undo_open(file_id: FileId) {
    let mut cache = FILE_CACHE.lock();
    if let Some(file) = cache.files.get_mut(&file_id) {
        file.ref_count = file.ref_count.saturating_sub(1);
    }
}

/// This file's open-reference count, for the leak-rollback self-test's census.
#[cfg(feature = "boot-actuators")]
pub fn ref_count(file_id: FileId) -> u32 {
    FILE_CACHE.lock().files.get(&file_id).map_or(0, |f| f.ref_count)
}

/// The verdict [`release_to_writeback`] hands its caller.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// Other handles still hold the file. Nothing is owed.
    StillHeld,
    /// This was the last handle: the file is pinned for write-back and the caller must [`crate::writeback::enqueue`] it.
    TeardownOwed,
    /// This was the last handle, but a teardown was already owed and enqueued; nothing to enqueue.
    AlreadyOwed,
}

/// Drop one open reference; if it was the last, pins the file for write-back instead of dropping it here — eviction never takes a dirty page, so a re-open before the drain reads the pinned data, not the device.
pub fn release_to_writeback(file_id: FileId) -> Release {
    let mut cache = FILE_CACHE.lock();
    let Some(file) = cache.files.get_mut(&file_id) else { return Release::StillHeld };
    file.ref_count = file.ref_count.saturating_sub(1);
    if file.ref_count != 0 {
        return Release::StillHeld;
    }
    if file.teardown_owed {
        return Release::AlreadyOwed;
    }
    file.teardown_owed = true;
    Release::TeardownOwed
}

/// What [`finish_writeback`] found, and what the drainer does with the filesystem-side handle.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Teardown {
    /// The file left the cache (or is a live tmpfs file whose pages stay): release the handle with `Vfs::close_file`.
    Released,
    /// A re-open adopted the file between the enqueue and the drain, so it is left alive with its handle.
    Adopted,
    /// The file is already gone from the cache. Nothing to close.
    Vanished,
}

/// What `iod`/shutdown needs to know before flushing a queued file, read in one lock.
#[derive(Clone, Copy)]
pub struct WritebackProbe {
    pub flush_owed: bool,
    /// A deleted file's drain skips the flush — its data is going away.
    pub deleted: bool,
}

/// Read a queued file's flush state; a file the queue pinned is always present, so absence reads as "nothing to flush" rather than a panic.
pub fn writeback_probe(file_id: FileId) -> WritebackProbe {
    let cache = FILE_CACHE.lock();
    match cache.files.get(&file_id) {
        Some(file) => WritebackProbe { flush_owed: file.dirt.is_owed(), deleted: file.deleted },
        None => WritebackProbe { flush_owed: false, deleted: false },
    }
}

/// The FILE_CACHE half of a write-back teardown; must run under the VFS lock, which serialises the drain against a re-open.
pub fn finish_writeback(file_id: FileId) -> Teardown {
    let mut cache = FILE_CACHE.lock();
    let Some(file) = cache.files.get_mut(&file_id) else { return Teardown::Vanished };
    if !file.teardown_owed {
        // A re-open's own release already cleared it; nothing owed here.
        return Teardown::Adopted;
    }
    file.teardown_owed = false;
    if file.ref_count != 0 {
        // A re-open adopted the file between the enqueue and now.
        return Teardown::Adopted;
    }
    if file.deleted || file.evictable {
        drop_file(&mut cache, file_id);
    }
    // else: a live tmpfs file keeps its (non-evictable) pages.
    Teardown::Released
}

/// Whether a shrink discarded this page, so the backing's copy is no longer the file's.
fn discarded(file: &CachedFile, page_idx: u32) -> bool {
    file.shrunk_to.is_some_and(|mark| page_idx as u64 * PAGE_SIZE as u64 >= mark)
}

/// Read a file page into `buf`; on `Err` the fetch failed and `buf` holds zeros, not the file's bytes.
pub fn read_page(
    file_id: FileId,
    page_idx: u32,
    offset: usize,
    buf: &mut UserBytesMut,
) -> Result<(), block::BlockError> {
    let backing;
    {
        let mut cache = FILE_CACHE.lock();
        let Some(file) = cache.files.get_mut(&file_id) else { return Ok(()) };
        let file_size = file.size;

        // Beyond file size: zero-fill, no cache insert.
        if (page_idx as u64) * PAGE_SIZE as u64 >= file_size {
            buf.fill_zero(0, buf.len());
            return Ok(());
        }

        if let Some(page) = file.pages.get_mut(&page_idx) {
            page.referenced = true;
            let avail = valid_bytes_in_page(page_idx, file_size);
            copy_page_region_to_buf(&page.data[..], offset, buf, avail);
            return Ok(());
        }
        backing = if discarded(file, page_idx) { None } else { file.backing.clone() };
    }
    // Cache miss: unlock, fetch from backing, re-lock, insert if still absent.

    let mut fetched = blank_page();
    if let Some(backing) = &backing {
        // A failed fetch must not become a resident page: a later partial write would merge into cached zeros and flush them over the file.
        if let Err(e) = backing.read_page(page_idx as u64 * PAGE_SIZE as u64, &mut fetched) {
            buf.fill_zero(0, buf.len());
            return Err(e);
        }
    }
    // else: tmpfs miss → zero-filled page (fetched is already zeroed)

    let mut cache = FILE_CACHE.lock();
    let mut added = 0;
    {
        let Some(file) = cache.files.get_mut(&file_id) else { return Ok(()) };
        let is_cache = file.is_cache();
        if let Entry::Vacant(slot) = file.pages.entry(page_idx) {
            slot.insert(CachedPage::new(fetched));
            added = is_cache as usize;
        }
        let file_size = file.size;
        let page = file.pages.get_mut(&page_idx).unwrap();
        page.referenced = true;
        let avail = valid_bytes_in_page(page_idx, file_size);
        copy_page_region_to_buf(&page.data[..], offset, buf, avail);
    }
    cache.cached_pages += added;
    evict_if_needed(&mut cache);
    Ok(())
}

/// Write data into a file page; the lock is not held during disk I/O on a cache miss.
/// `Err` means the page could not be re-read and nothing was written — merging into zeros would destroy 4 KiB of a file that was fine.
pub fn write_page<S: ByteSource + ?Sized>(
    file_id: FileId,
    page_idx: u32,
    offset: usize,
    data: &S,
) -> Result<(), block::BlockError> {
    // A resident page is written under the same lock acquisition that found it; the fetch path below drops the lock, and a sibling's eviction in that window would otherwise merge the write into a blank page.
    let backing;
    {
        let mut cache = FILE_CACHE.lock();
        {
            let Some(file) = cache.files.get_mut(&file_id) else { return Ok(()) };
            if file.pages.contains_key(&page_idx) {
                apply_write(file, page_idx, offset, data);
                backing = None;
            } else if discarded(file, page_idx) {
                backing = Some(None);
            } else {
                backing = Some(file.backing.clone());
            }
        }
        if backing.is_none() {
            evict_if_needed(&mut cache);
            return Ok(());
        }
    }
    let backing = backing.unwrap();

    let mut fetched = blank_page();
    if let Some(backing) = &backing {
        let page_start = page_idx as u64 * PAGE_SIZE as u64;
        // Past the end there is nothing to preserve, so no fetch and no way for one to fail.
        if page_start < backing.file_size() {
            backing.read_page(page_start, &mut fetched)?;
        }
    }

    // Re-fetching after a sibling's eviction is always correct: only clean pages are ever evicted.
    let mut cache = FILE_CACHE.lock();
    let mut added = 0;
    {
        let Some(file) = cache.files.get_mut(&file_id) else { return Ok(()) };
        let is_cache = file.is_cache();
        if let Entry::Vacant(slot) = file.pages.entry(page_idx) {
            slot.insert(CachedPage::new(fetched));
            added = is_cache as usize;
        }
        apply_write(file, page_idx, offset, data);
    }
    cache.cached_pages += added;
    evict_if_needed(&mut cache);
    Ok(())
}

fn apply_write<S: ByteSource + ?Sized>(
    file: &mut CachedFile,
    page_idx: u32,
    offset: usize,
    data: &S,
) {
    let page = file.pages.get_mut(&page_idx).expect("write_page: page not resident");
    let end = (offset + data.len()).min(PAGE_SIZE);
    data.read_at(0, &mut page.data[offset..end]);
    page.dirt.record_write();
    page.referenced = true;
    // Both recorded under the one FILE_CACHE lock, so a flush never observes one without the other.
    file.dirt.record_write();

    let write_end = page_idx as u64 * PAGE_SIZE as u64 + end as u64;
    if write_end > file.size {
        file.size = write_end;
    }
}

/// Copy a resident page out, with the settlement its flusher must present to
/// mark it clean; `None` leaves `buf` untouched — an absent page is not zeros.
#[must_use]
pub fn copy_page_out(file_id: FileId, page_idx: u32, buf: &mut [u8; PAGE_SIZE]) -> Option<Settlement> {
    let cache = FILE_CACHE.lock();
    let file = cache.files.get(&file_id)?;
    let page = file.pages.get(&page_idx)?;
    *buf = *page.data;
    Some(page.dirt.snapshot())
}

/// What one flush attempt owes, snapshotted in one lock; nothing is cleared
/// here — a write landing mid-flush outruns the settlement and stays owed.
pub struct FlushPlan {
    pub file: Settlement,
    pub pages: BTreeSet<u32>,
    /// What the mount has to give back before this flush's pages land on top of it.
    pub shrunk_to: Option<u64>,
}

pub fn begin_flush(file_id: FileId) -> FlushPlan {
    let cache = FILE_CACHE.lock();
    match cache.files.get(&file_id) {
        Some(file) => FlushPlan {
            file: file.dirt.snapshot(),
            pages: file.pages.iter().filter(|(_, p)| p.is_dirty()).map(|(&i, _)| i).collect(),
            shrunk_to: file.shrunk_to,
        },
        None => FlushPlan {
            file: Owed::new().snapshot(),
            pages: BTreeSet::new(),
            shrunk_to: None,
        },
    }
}

/// Whether a file owes a write-back; `fsync` reads this so a handle that did not itself write still flushes a file another handle dirtied.
pub fn flush_owed(file_id: FileId) -> bool {
    FILE_CACHE.lock().files.get(&file_id).is_some_and(|f| f.dirt.is_owed())
}

/// Settle each page up to what its flush copied; a page written since keeps its debt and the next flush delivers it.
pub fn settle_pages(file_id: FileId, flushed: &[(u32, Settlement)]) {
    let mut cache = FILE_CACHE.lock();
    if let Some(file) = cache.files.get_mut(&file_id) {
        for (page_idx, copied) in flushed {
            if let Some(page) = file.pages.get_mut(page_idx) {
                page.dirt.settle(*copied);
            }
        }
    }
}

/// Settle the file's flush debt; only a flush that wrote its pages and its metadata calls this.
pub fn settle_file(file_id: FileId, upto: Settlement) {
    if let Some(file) = FILE_CACHE.lock().files.get_mut(&file_id) {
        file.dirt.settle(upto);
    }
}

/// Clear the shrink mark: the flush's metadata write is what the mount records now.
/// Sound without a generation because every shrink runs under the VFS lock a flush holds.
pub fn settle_shrink(file_id: FileId) {
    if let Some(file) = FILE_CACHE.lock().files.get_mut(&file_id) {
        file.shrunk_to = None;
    }
}

/// Get the authoritative file size.
pub fn size(file_id: FileId) -> u64 {
    FILE_CACHE.lock().files.get(&file_id).map_or(0, |f| f.size)
}

/// Set file size and drop pages past it; the establishing form (mount-time), does not mark the file dirty — see [`resize`] for a user truncate.
pub fn set_size(file_id: FileId, new_size: u64) {
    let mut cache = FILE_CACHE.lock();
    set_size_locked(&mut cache, file_id, new_size);
    if let Some(file) = cache.files.get_mut(&file_id) {
        file.shrunk_to = None;
    }
}

/// A user truncate: [`set_size`], plus marks the file dirty even when no page changed.
/// The `&mut Vfs` is the witness: every flusher's size-read/`update_metadata`
/// pair runs under the VFS lock, so a resize outside it could record a stale size.
///
/// `Err` means the page the new end falls inside could not be read, and nothing
/// was resized: half of that page survives the shrink and the rest has to be
/// zeroed, which is a read-modify-write and not a truncation.
pub fn resize(
    _vfs: &mut crate::vfs::Vfs,
    file_id: FileId,
    new_size: u64,
) -> Result<(), block::BlockError> {
    if let Some(straddled) = straddled_by_shrink(file_id, new_size) {
        fault_in(file_id, straddled)?;
    }
    let mut cache = FILE_CACHE.lock();
    let shrank = cache.files.get(&file_id).is_some_and(|f| new_size < f.size);
    set_size_locked(&mut cache, file_id, new_size);
    if let Some(file) = cache.files.get_mut(&file_id) {
        file.dirt.record_write();
        if shrank {
            file.shrunk_to = Some(file.shrunk_to.map_or(new_size, |mark| mark.min(new_size)));
        }
    }
    evict_if_needed(&mut cache);
    Ok(())
}

/// The page a shrink to `new_size` would cut in half and does not hold.
///
/// [`set_size_locked`] zeroes that page's tail and dirties it, which is what
/// carries the zeros to the device — and it can only do that to a page that is
/// resident. An absent one leaves the bytes past the new end on the device
/// under a size that still reaches them, which is the discarded tail served
/// back. [`discarded`] cannot cover it either: the page is *partly* the file's.
fn straddled_by_shrink(file_id: FileId, new_size: u64) -> Option<u32> {
    if new_size.is_multiple_of(PAGE_SIZE as u64) {
        return None;
    }
    let idx = (new_size / PAGE_SIZE as u64) as u32;
    let cache = FILE_CACHE.lock();
    let file = cache.files.get(&file_id)?;
    (new_size < file.size && !file.pages.contains_key(&idx) && file.backing.is_some())
        .then_some(idx)
}

/// Make `page_idx` resident, reading it through the backing.
fn fault_in(file_id: FileId, page_idx: u32) -> Result<(), block::BlockError> {
    let backing = {
        let cache = FILE_CACHE.lock();
        let Some(file) = cache.files.get(&file_id) else { return Ok(()) };
        if file.pages.contains_key(&page_idx) || discarded(file, page_idx) {
            return Ok(());
        }
        file.backing.clone()
    };
    let mut fetched = blank_page();
    if let Some(backing) = &backing {
        backing.read_page(page_idx as u64 * PAGE_SIZE as u64, &mut fetched)?;
    }
    let mut cache = FILE_CACHE.lock();
    let mut added = 0;
    if let Some(file) = cache.files.get_mut(&file_id) {
        let is_cache = file.is_cache();
        if let Entry::Vacant(slot) = file.pages.entry(page_idx) {
            slot.insert(CachedPage::new(fetched));
            added = is_cache as usize;
        }
    }
    cache.cached_pages += added;
    // No eviction here: the page it just admitted is clean and unreferenced, so
    // an over-budget sweep would take it straight back. [`resize`] evicts once
    // the shrink has dirtied it, and a dirty page is never a candidate.
    Ok(())
}

fn set_size_locked(cache: &mut FileCache, file_id: FileId, new_size: u64) {
    let dropped;
    {
        let Some(file) = cache.files.get_mut(&file_id) else { return };
        dropped = if new_size < file.size {
            let is_cache = file.is_cache();
            let first_removed = (new_size as usize).div_ceil(PAGE_SIZE) as u32;
            let removed: alloc::vec::Vec<u32> = file.pages.range(first_removed..)
                .map(|(&k, _)| k).collect();
            for k in &removed {
                file.pages.remove(k);
            }
            // The page the new end falls inside is kept; zero its bytes past the
            // new end and dirty it, in the one step that sets the size, so a
            // later grow reads the hole as zeros rather than the discarded tail
            // and the flush carries those zeros to the device.
            let tail = (new_size % PAGE_SIZE as u64) as usize;
            if tail != 0 {
                let straddled = (new_size / PAGE_SIZE as u64) as u32;
                if let Some(page) = file.pages.get_mut(&straddled) {
                    page.data[tail..].fill(0);
                    page.dirt.record_write();
                    file.dirt.record_write();
                }
            }
            if is_cache { removed.len() } else { 0 }
        } else {
            0
        };
        file.size = new_size;
    }
    cache.cached_pages -= dropped;
}

/// What the cache holds for a file after an operation that may have freed it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Residency {
    /// Something still holds it — an open handle, or the write-back queue — so its filesystem handle must survive until that holder's teardown.
    Held,
    /// The cache holds nothing for this id; a filesystem may drop whatever it keeps alongside.
    Gone,
}

/// Mark a file as deleted (unlink). If no handles hold it, free immediately.
#[must_use]
pub fn mark_deleted(file_id: FileId) -> Residency {
    let mut cache = FILE_CACHE.lock();
    let Some(file) = cache.files.get_mut(&file_id) else { return Residency::Gone };
    file.deleted = true;
    // A pinned file (teardown_owed) is left marked deleted for `finish_writeback` to drop; `Held` because the cache still holds it.
    if file.ref_count > 0 || file.teardown_owed {
        return Residency::Held;
    }
    drop_file(&mut cache, file_id);
    Residency::Gone
}

impl CachedPage {
    fn new(data: Box<[u8; PAGE_SIZE]>) -> Self {
        Self { data, dirt: Owed::new(), referenced: false }
    }
}

/// A blank page, allocated directly on the heap: never construct via a stack-sized array.
fn blank_page() -> Box<[u8; PAGE_SIZE]> {
    match alloc::vec![0u8; PAGE_SIZE].into_boxed_slice().try_into() {
        Ok(page) => page,
        Err(_) => unreachable!("a PAGE_SIZE slice is a [u8; PAGE_SIZE]"),
    }
}

fn drop_file(cache: &mut FileCache, file_id: FileId) {
    let Some(removed) = cache.files.remove(&file_id) else { return };
    if removed.is_cache() {
        cache.cached_pages -= removed.pages.len();
    }
}

fn valid_bytes_in_page(page_idx: u32, file_size: u64) -> usize {
    let page_start = page_idx as u64 * PAGE_SIZE as u64;
    if page_start >= file_size {
        0
    } else {
        ((file_size - page_start) as usize).min(PAGE_SIZE)
    }
}

fn copy_page_region_to_buf(page: &[u8], offset: usize, buf: &mut UserBytesMut, valid: usize) {
    let start = offset.min(valid);
    let end = (offset + buf.len()).min(valid);
    let count = end.saturating_sub(start);
    if count > 0 {
        buf.write_at(0, &page[start..start + count]);
    }
    // Zero-fill remainder (past valid data or past file end).
    if count < buf.len() {
        buf.fill_zero(count, buf.len() - count);
    }
}

fn evict_if_needed(cache: &mut FileCache) {
    assert!(cache.max_pages != 0, "file cache used before init installed a budget");
    if cache.cached_pages <= cache.max_pages {
        // A teardown can end an over-budget episode between admissions; the closing line still prints.
        if cache.over_said {
            cache.over_said = false;
            turnover_line(cache);
        }
        return;
    }
    let before = cache.evictions;
    while cache.cached_pages > cache.max_pages {
        if !evict_one(cache) {
            // Everything resident is dirty: write-back is the handle layer's job, so nothing here bounds dirty pages further.
            break;
        }
    }
    // Once per full turnover so the rate scales with the budget, plus once at
    // each over-budget episode's start and end.
    let turnover = cache.max_pages as u64;
    let over = cache.cached_pages > cache.max_pages;
    let crossed =
        cache.evictions != before && (before == 0 || before / turnover != cache.evictions / turnover);
    if crossed || over != cache.over_said {
        turnover_line(cache);
    }
    cache.over_said = over;
}

/// Dirty is on the line because over-budget is lawful exactly when every resident page is dirty; the harness holds that shape.
fn turnover_line(cache: &FileCache) {
    log!("file cache: {} evictions, {}/{} pages resident, {} dirty",
        cache.evictions, cache.cached_pages, cache.max_pages, dirty_pages(cache));
}

/// Governed dirty pages, counted under the same lock hold as the residency they explain.
fn dirty_pages(cache: &FileCache) -> usize {
    cache
        .files
        .values()
        .filter(|f| f.is_cache())
        .map(|f| f.pages.values().filter(|p| p.is_dirty()).count())
        .sum()
}

/// One CLOCK step-and-evict; returns false when a full revolution found no page it was allowed to take.
fn evict_one(cache: &mut FileCache) -> bool {
    // Two full passes: the first may only clear reference bits, so the second must be able to evict; `+2` covers each wrap.
    let steps = cache.cached_pages * 2 + 2;
    for _ in 0..steps {
        let Some((fid, idx)) = seek_hand(cache) else { return false };
        cache.hand = match idx.checked_add(1) {
            Some(next) => (fid, next),
            None => (fid + 1, 0),
        };

        {
            let Some(file) = cache.files.get_mut(&fid) else { continue };
            let Some(page) = file.pages.get_mut(&idx) else { continue };
            if page.is_dirty() {
                continue;
            }
            if page.referenced {
                page.referenced = false;
                continue;
            }
            file.pages.remove(&idx);
        }
        cache.cached_pages -= 1;
        cache.evictions += 1;
        return true;
    }
    false
}

/// The first resident page at or after the hand, wrapping once.
fn seek_hand(cache: &mut FileCache) -> Option<(FileId, u32)> {
    if let Some(found) = page_at_or_after(cache, cache.hand) {
        return Some(found);
    }
    cache.hand = (0, 0);
    page_at_or_after(cache, cache.hand)
}

fn page_at_or_after(cache: &FileCache, from: (FileId, u32)) -> Option<(FileId, u32)> {
    for (&fid, file) in cache.files.range(from.0..) {
        // Skip whole files, not page by page: a tmpfs file's pages can never be evicted, and stepping through one would exhaust the sweep's budget.
        if !file.is_cache() {
            continue;
        }
        let start = if fid == from.0 { from.1 } else { 0 };
        if let Some((&idx, _)) = file.pages.range(start..).next() {
            return Some((fid, idx));
        }
    }
    None
}
