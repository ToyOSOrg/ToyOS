use alloc::boxed::Box;
use alloc::collections::btree_map::Entry;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use alloc::vec::Vec;

use toyos_abi::syscall::SyscallError;

use crate::file_backing::{BlockError, FileBacking};
use crate::mm::charge::{self, Charge, Exhausted};
use crate::mm::space::CopySource;
use crate::sync::Lock;
use crate::user_ptr::UserBytesMut;

pub type FileId = u64;

/// `mm::PAGE_SIZE`, in the width this file's arrays and slices index by.
const PAGE_SIZE: usize = crate::mm::PAGE_SIZE as usize;

/// The one ceiling on a file position or size: the page index is a `u32`, so past
/// this it would wrap and alias a low page — seek, write and truncate refuse it.
pub const MAX_FILE_SIZE: u64 = (u32::MAX as u64 + 1) * PAGE_SIZE as u64;

/// A page's bytes, shared with each [`Snapshot`] taken while it was the
/// file's: a write to a shared page writes a copy of it.
type PageData = Arc<Page>;

/// A page's bytes, charged to whoever made them for as long as anything holds
/// them: the file, or a snapshot of it a spawn pages from.
pub struct Page {
    bytes: Box<[u8; PAGE_SIZE]>,
    _charge: Charge,
}

impl core::ops::Deref for Page {
    type Target = [u8; PAGE_SIZE];

    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl Page {
    /// A page of zeros, or `Exhausted`, charged before it is allocated with
    /// its place in its file.
    fn blank() -> Result<Self, Exhausted> {
        let charge = Charge::current(charge::heap(core::alloc::Layout::new::<[u8; PAGE_SIZE]>()) + charge::arc::<Page>() + charge::map_entry::<u32, CachedPage>())?;
        Ok(Self { bytes: blank_page(), _charge: charge })
    }

    /// Make `data` this file's own to write, copying it if a snapshot shares
    /// it; refused, it is left shared and unwritten.
    fn own(data: &mut PageData) -> Result<&mut [u8; PAGE_SIZE], Exhausted> {
        if Arc::get_mut(data).is_none() {
            let mut copy = Self::blank()?;
            *copy.bytes = ***data;
            *data = Arc::new(copy);
        }
        Ok(&mut Arc::get_mut(data).expect("a page this file holds alone").bytes)
    }
}

struct CachedPage {
    data: PageData,
    /// CLOCK's second-chance bit: set on every hit, cleared when the sweep passes it over.
    referenced: bool,
}

struct CachedFile {
    /// Its place in the cache, charged to whoever made it.
    _charge: Charge,
    pages: BTreeMap<u32, CachedPage>,
    size: u64,
    evictable: bool,
    /// Where an evicted page comes back from; `None` means a dropped page cannot be re-read.
    backing: Option<Arc<dyn FileBacking>>,
    ref_count: u32,
    deleted: bool,
    /// When it was last written, for a mount whose pages are the file.
    mtime: u64,
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
}

static FILE_CACHE: Lock<FileCache> = Lock::new(FileCache {
    files: BTreeMap::new(),
    next_id: 1,
    cached_pages: 0,
    // Zero, not `usize::MAX`: an uninstalled budget must fail loudly, not silently allow everything.
    max_pages: 0,
    evictions: 0,
    hand: (0, 0),
});

/// Install the memory budget; must run after the PMM sizes RAM and before any file is opened.
pub fn init() {
    let (total, _) = crate::mm::pmm::stats();
    let max_pages = (((total / 64) / PAGE_SIZE as u64) as usize).clamp(2048, 65536);
    FILE_CACHE.lock().max_pages = max_pages;
    log!("file cache: budget {} pages ({} MiB)", max_pages, max_pages * PAGE_SIZE / (1024 * 1024));
}

/// Allocate a new FileId. The file cache is the sole allocator.
pub fn create_file(evictable: bool, mtime: u64) -> Result<FileId, Exhausted> {
    let charge = Charge::current(charge::map_entry::<FileId, CachedFile>())?;
    let mut cache = FILE_CACHE.lock();
    let id = cache.next_id;
    cache.next_id += 1;
    cache.files.insert(id, CachedFile {
        _charge: charge,
        pages: BTreeMap::new(),
        size: 0,
        evictable,
        backing: None,
        ref_count: 1,
        deleted: false,
        mtime,
    });
    Ok(id)
}

/// A number from the count `FileId`s are drawn from, which no file will ever
/// hold: a kernel volume's node for a name the cache keeps no file for, so no
/// node on a kernel volume is another's (`vfs::FileSystem::node`).
pub fn draw_node() -> u64 {
    let mut cache = FILE_CACHE.lock();
    let node = cache.next_id;
    cache.next_id += 1;
    node
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

/// Increment ref_count for one more open, returning a guard that undoes the
/// increment on drop unless committed: a re-open whose backing lookup fails
/// after this must not pin the file.
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

/// Undo one [`open`]: a re-open decrements a count another handle keeps, never orphaning the file.
fn undo_open(file_id: FileId) {
    let mut cache = FILE_CACHE.lock();
    if let Some(file) = cache.files.get_mut(&file_id) {
        file.ref_count = file.ref_count.saturating_sub(1);
    }
}

/// Drop one open reference. The last of a deleted file takes its pages with
/// it; any other file stays, its pages clean and CLOCK's to evict, so a
/// re-open finds the id its mount keeps for the name.
pub fn release(file_id: FileId) {
    let mut cache = FILE_CACHE.lock();
    let Some(file) = cache.files.get_mut(&file_id) else { return };
    file.ref_count = file.ref_count.saturating_sub(1);
    if file.ref_count == 0 && file.deleted {
        drop_file(&mut cache, file_id);
    }
}

/// When the file was last written, as [`create_file`] and [`touch`] said.
pub fn mtime(file_id: FileId) -> u64 {
    FILE_CACHE.lock().files.get(&file_id).map_or(0, |f| f.mtime)
}

/// Record a write at `mtime`.
pub fn touch(file_id: FileId, mtime: u64) {
    if let Some(file) = FILE_CACHE.lock().files.get_mut(&file_id) {
        file.mtime = mtime;
    }
}

/// Read a file page into `buf`; on `Err` the fetch failed and `buf` holds zeros, not the file's bytes.
/// The copy runs with the cache's lock let go, out of the page as it was: a
/// write meanwhile writes a copy of it (`Page::own`).
pub fn read_page(
    file_id: FileId,
    page_idx: u32,
    offset: usize,
    buf: &mut UserBytesMut,
) -> Result<(), BlockError> {
    let backing = {
        let mut cache = FILE_CACHE.lock();
        let Some(file) = cache.files.get_mut(&file_id) else { return Ok(()) };
        let file_size = file.size;
        if (page_idx as u64) * PAGE_SIZE as u64 >= file_size {
            None
        } else if let Some(page) = file.pages.get_mut(&page_idx) {
            page.referenced = true;
            Some(Ok((Arc::clone(&page.data), valid_bytes_in_page(page_idx, file_size))))
        } else {
            Some(Err(file.backing.clone()))
        }
    };
    let backing = match backing {
        // Beyond file size: zero-fill, no cache insert.
        None => {
            buf.fill_zero(0, buf.len());
            return Ok(());
        }
        Some(Ok((page, avail))) => {
            copy_page_region_to_buf(&page[..], offset, buf, avail);
            return Ok(());
        }
        // Cache miss: fetch from backing with the lock let go, re-lock, insert if still absent.
        Some(Err(backing)) => backing,
    };

    let mut fetched = match Page::blank() {
        Ok(page) => page,
        // A hole is zeros whether or not a page is kept for it.
        Err(Exhausted) if backing.is_none() => {
            buf.fill_zero(0, buf.len());
            return Ok(());
        }
        Err(Exhausted) => {
            buf.fill_zero(0, buf.len());
            return Err(BlockError::Exhausted);
        }
    };
    if let Some(backing) = &backing {
        // A failed fetch must not become a resident page: a later partial write would merge into cached zeros and flush them over the file.
        if let Err(e) = backing.read_page(page_idx as u64 * PAGE_SIZE as u64, &mut fetched.bytes) {
            buf.fill_zero(0, buf.len());
            return Err(e);
        }
    }
    // else: tmpfs miss → zero-filled page (fetched is already zeroed)

    let (page, avail) = {
        let mut cache = FILE_CACHE.lock();
        let mut added = 0;
        let found = {
            let Some(file) = cache.files.get_mut(&file_id) else { return Ok(()) };
            let is_cache = file.is_cache();
            if let Entry::Vacant(slot) = file.pages.entry(page_idx) {
                slot.insert(CachedPage::new(fetched));
                added = is_cache as usize;
            }
            let file_size = file.size;
            let page = file.pages.get_mut(&page_idx).unwrap();
            page.referenced = true;
            (Arc::clone(&page.data), valid_bytes_in_page(page_idx, file_size))
        };
        cache.cached_pages += added;
        evict_if_needed(&mut cache);
        found
    };
    copy_page_region_to_buf(&page[..], offset, buf, avail);
    Ok(())
}

/// Write data into a file page; the lock is not held during disk I/O on a cache miss.
/// `Err` means the page could not be re-read and nothing was written — merging into zeros would destroy 4 KiB of a file that was fine —
/// or that no memory is left to keep it in.
pub fn write_page(file_id: FileId, page_idx: u32, offset: usize, data: &[u8]) -> Result<(), BlockError> {
    // A resident page is written under the same lock acquisition that found it; the fetch path below drops the lock, and a sibling's eviction in that window would otherwise merge the write into a blank page.
    let backing;
    {
        let mut cache = FILE_CACHE.lock();
        {
            let Some(file) = cache.files.get_mut(&file_id) else { return Ok(()) };
            if file.pages.contains_key(&page_idx) {
                apply_write(file, page_idx, offset, data).map_err(|Exhausted| BlockError::Exhausted)?;
                backing = None;
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

    let mut fetched = Page::blank().map_err(|Exhausted| BlockError::Exhausted)?;
    if let Some(backing) = &backing {
        let page_start = page_idx as u64 * PAGE_SIZE as u64;
        // Past the end there is nothing to preserve, so no fetch and no way for one to fail.
        if page_start < backing.file_size() {
            backing.read_page(page_start, &mut fetched.bytes)?;
        }
    }

    // Re-fetching after a sibling's eviction is always correct: a page written is never evicted.
    let mut cache = FILE_CACHE.lock();
    let mut added = 0;
    {
        let Some(file) = cache.files.get_mut(&file_id) else { return Ok(()) };
        let is_cache = file.is_cache();
        if let Entry::Vacant(slot) = file.pages.entry(page_idx) {
            slot.insert(CachedPage::new(fetched));
            added = is_cache as usize;
        }
        // A page this write just made is the file's alone, so this charges nothing.
        apply_write(file, page_idx, offset, data).map_err(|Exhausted| BlockError::Exhausted)?;
    }
    cache.cached_pages += added;
    evict_if_needed(&mut cache);
    Ok(())
}

/// Refused, the page is unchanged: a snapshot shares it and no copy can be
/// charged.
fn apply_write(file: &mut CachedFile, page_idx: u32, offset: usize, data: &[u8]) -> Result<(), Exhausted> {
    let page = file.pages.get_mut(&page_idx).expect("write_page: page not resident");
    let end = (offset + data.len()).min(PAGE_SIZE);
    Page::own(&mut page.data)?[offset..end].copy_from_slice(&data[..end - offset]);
    page.referenced = true;

    let write_end = page_idx as u64 * PAGE_SIZE as u64 + end as u64;
    if write_end > file.size {
        file.size = write_end;
    }
    Ok(())
}

/// Copy a resident page out; `false` leaves `buf` untouched — an absent page is not zeros.
pub fn copy_page_out(file_id: FileId, page_idx: u32, buf: &mut [u8; PAGE_SIZE]) -> bool {
    let cache = FILE_CACHE.lock();
    let Some(page) = cache.files.get(&file_id).and_then(|file| file.pages.get(&page_idx)) else {
        return false;
    };
    *buf = **page.data;
    true
}

/// A file's bytes as they were when it was taken, which no later write,
/// truncation or delete changes: a spawn's program, read without the cache's
/// lock.
pub struct Snapshot {
    size: u64,
    /// The resident pages by index; an absent one is a hole and reads zeros.
    pages: Vec<(u32, PageData)>,
    /// The list, and the snapshot itself.
    _charge: Charge,
}

/// `file_id`'s bytes now: `NotFound` for a file the cache no longer holds,
/// `ResourceExhausted` for one whose list of pages is past one heap
/// allocation or cannot be charged. Only a file with no backing: an absent
/// page of one with a backing was evicted, not a hole.
pub fn snapshot(file_id: FileId) -> Result<Snapshot, SyscallError> {
    let cache = FILE_CACHE.lock();
    let file = cache.files.get(&file_id).ok_or(SyscallError::NotFound)?;
    assert!(file.backing.is_none(), "file cache: a snapshot of file {file_id}, whose absent pages are on its backing");
    if file.pages.len() > crate::mm::MAX_HEAP_ALLOC / core::mem::size_of::<(u32, PageData)>() {
        return Err(SyscallError::ResourceExhausted);
    }
    let charge = Charge::current(charge::arc::<Snapshot>() + charge::array::<(u32, PageData)>(file.pages.len()))?;
    let pages = file.pages.iter().map(|(&idx, page)| (idx, Arc::clone(&page.data))).collect();
    Ok(Snapshot { size: file.size, pages, _charge: charge })
}

impl CopySource for Snapshot {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&self, offset: u64, out: &mut [u8]) {
        assert!(offset.checked_add(out.len() as u64).is_some_and(|end| end <= self.size), "file cache: a snapshot read past its end");
        let mut done = 0;
        while done < out.len() {
            let at = offset + done as u64;
            let (idx, within) = ((at / PAGE_SIZE as u64) as u32, (at % PAGE_SIZE as u64) as usize);
            let n = (PAGE_SIZE - within).min(out.len() - done);
            match self.pages.binary_search_by_key(&idx, |&(i, _)| i) {
                Ok(i) => out[done..done + n].copy_from_slice(&self.pages[i].1[within..within + n]),
                Err(_) => out[done..done + n].fill(0),
            }
            done += n;
        }
    }
}

/// Get the authoritative file size.
pub fn size(file_id: FileId) -> u64 {
    FILE_CACHE.lock().files.get(&file_id).map_or(0, |f| f.size)
}

/// Set file size and drop pages past it. Refused, nothing changes: the page
/// the new end falls inside is shared with a snapshot, and no copy of it to
/// zero can be charged.
pub fn set_size(file_id: FileId, new_size: u64) -> Result<(), Exhausted> {
    let mut cache = FILE_CACHE.lock();
    let dropped;
    {
        let Some(file) = cache.files.get_mut(&file_id) else { return Ok(()) };
        dropped = if new_size < file.size {
            // The page the new end falls inside is kept; zero its bytes past the
            // new end in the one step that sets the size, so a later grow reads
            // the hole as zeros rather than the discarded tail.
            let tail = (new_size % PAGE_SIZE as u64) as usize;
            if tail != 0 {
                let straddled = (new_size / PAGE_SIZE as u64) as u32;
                if let Some(page) = file.pages.get_mut(&straddled) {
                    Page::own(&mut page.data)?[tail..].fill(0);
                }
            }
            // Split off whole, so that no list of the pages is allocated.
            let first_removed = (new_size as usize).div_ceil(PAGE_SIZE) as u32;
            let removed = file.pages.split_off(&first_removed);
            if file.is_cache() { removed.len() } else { 0 }
        } else {
            0
        };
        file.size = new_size;
    }
    cache.cached_pages -= dropped;
    Ok(())
}

/// Mark a file as deleted (unlink). If no handles hold it, free immediately;
/// otherwise the last [`release`] does.
pub fn mark_deleted(file_id: FileId) {
    let mut cache = FILE_CACHE.lock();
    let Some(file) = cache.files.get_mut(&file_id) else { return };
    file.deleted = true;
    if file.ref_count == 0 {
        drop_file(&mut cache, file_id);
    }
}

impl CachedPage {
    fn new(data: Page) -> Self {
        Self { data: Arc::new(data), referenced: false }
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
    let before = cache.evictions;
    while cache.cached_pages > cache.max_pages {
        // A governed page is a read-only mount's and never written, so one
        // revolution always finds one to take.
        assert!(
            evict_one(cache),
            "file cache: {}/{} pages resident and none evictable",
            cache.cached_pages,
            cache.max_pages
        );
    }
    // Once per full turnover, so the rate scales with the budget.
    let turnover = cache.max_pages as u64;
    if cache.evictions != before && (before == 0 || before / turnover != cache.evictions / turnover) {
        log!("file cache: {} evictions, {}/{} pages resident", cache.evictions, cache.cached_pages, cache.max_pages);
    }
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
