use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use hashbrown::HashMap;

use bcachefs::{BlockIO, BlockBuf, BlockNum, DeviceError, FsError, Mounted, ReadWrite, ReadOnly, Formatted, SliceBlockIO, Extent};
use crate::file_backing::{FileBacking, FileBlocks, NvmeBacking, InitrdBacking};
use crate::mm::PAGE_BYTES;
use crate::file_cache::{self, FileId, Residency};
use crate::fs_rename::{self, Committed, ReplaceRename};
use crate::page_cache;
use toyos_abi::syscall::SyscallError;

use crate::vfs::FileSystem;

/// BlockIO implementation that wraps the kernel's global PageCache.
pub struct PageCacheBlockIO;

/// The one conversion between the kernel's block verdict and the crate's;
/// keeping the retry discriminant is the point — a `BudgetExpired` collapsed
/// into the device's own word turned into permanent write loss once (F9).
impl From<crate::block::BlockError> for DeviceError {
    fn from(e: crate::block::BlockError) -> Self {
        match e {
            crate::block::BlockError::Device => DeviceError::Failed,
            crate::block::BlockError::BudgetExpired => DeviceError::Refused,
        }
    }
}

/// Errors propagate unchanged; nothing here invents a value for a refused transfer.
impl BlockIO for PageCacheBlockIO {
    fn read_block(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), DeviceError> {
        let mut guard = page_cache::lock();
        let (cache, dev) = guard.cache_and_dev();
        let page = cache.read(dev, block.raw()).map_err(DeviceError::from)?;
        buf.as_bytes_mut().copy_from_slice(page);
        Ok(())
    }

    fn write_block(&self, block: BlockNum, buf: &BlockBuf) -> Result<(), DeviceError> {
        let mut guard = page_cache::lock();
        let (cache, dev) = guard.cache_and_dev();
        let page = cache.write_new(dev, block.raw()).map_err(DeviceError::from)?;
        page.copy_from_slice(buf.as_bytes());
        Ok(())
    }

    fn block_count(&self) -> u64 {
        let guard = page_cache::lock();
        guard.block_count()
    }

    fn sync(&self) -> Result<(), DeviceError> {
        let mut guard = page_cache::lock();
        let (cache, dev) = guard.cache_and_dev();
        cache.sync(dev).map_err(DeviceError::from)
    }
}

/// A budget refusal is `WouldBlock` — the word every retry loop keys on — and
/// only the device's own word is `Io`; `kernel/CLAUDE.md` states the rule.
fn as_device_refusal(e: DeviceError) -> SyscallError {
    match e {
        DeviceError::Failed => SyscallError::Io,
        DeviceError::Refused => SyscallError::WouldBlock,
    }
}

/// Exhaustive match: corruption maps to `Io`, never `NotFound` — a btree that won't decode isn't "not there".
fn as_syscall_error(err: &FsError) -> SyscallError {
    match err {
        FsError::NotFound => SyscallError::NotFound,
        FsError::NoSpace { .. } | FsError::EntryTooLarge { .. } => SyscallError::ResourceExhausted,
        FsError::NameTooLong { .. } => SyscallError::InvalidArgument,
        FsError::DeviceRead(_, e) | FsError::DeviceWrite(_, e) | FsError::DeviceSync(e) => {
            as_device_refusal(*e)
        }
        FsError::BadMagic { .. }
        | FsError::UnsupportedVersion(_)
        | FsError::ChecksumMismatch { .. }
        | FsError::CorruptedKey(_)
        | FsError::CorruptedNode(_)
        | FsError::BlockOffDevice { .. }
        | FsError::NotEnoughBlocks { .. }
        | FsError::TreeTooDeep(_)
        | FsError::BadSuperblock { .. }
        | FsError::NodeOverfull { .. } => SyscallError::Io,
    }
}

/// Logs the error's detail, then maps it to the `SyscallError` a caller can act on.
fn mapped<T>(op: &str, name: &str, result: Result<T, FsError>) -> Result<T, SyscallError> {
    result.map_err(|err| {
        log!("bcachefs: {} of '{}' failed: {:?}", op, name, err);
        as_syscall_error(&err)
    })
}

/// The same, for a `bcachefs` call whose `Ok(None)` means the name is absent.
fn present<T>(op: &str, name: &str, result: Result<Option<T>, FsError>) -> Result<T, SyscallError> {
    mapped(op, name, result)?.ok_or(SyscallError::NotFound)
}

/// Per-open-file cached resolution state.
struct OpenFileInfo {
    name: String,
    blocks: Arc<FileBlocks>,
}

/// VFS adapter for read-write bcachefs on NVMe.
pub struct BcacheFsAdapter {
    fs: Mounted<PageCacheBlockIO, ReadWrite>,
    open_files: HashMap<FileId, OpenFileInfo>,
    name_to_id: HashMap<String, FileId>,
    /// The `FileBlocks` every backing for a name shares; keyed by name because
    /// `open_backing` hands one out without opening a file at all. `Weak` so the
    /// entry costs nothing once the last backing drops.
    blocks: HashMap<String, Weak<FileBlocks>>,
}

impl BcacheFsAdapter {
    pub fn new(fs: Mounted<PageCacheBlockIO, ReadWrite>) -> Self {
        Self {
            fs,
            open_files: HashMap::new(),
            name_to_id: HashMap::new(),
            blocks: HashMap::new(),
        }
    }

    /// The cell every backing for `name` reads through; made from `extents` on first use.
    fn blocks_for(&mut self, name: &str, extents: Vec<Extent>) -> Arc<FileBlocks> {
        // Swept here, not on a timer: this is the only place the map grows.
        self.blocks.retain(|_, weak| weak.strong_count() > 0);

        if let Some(live) = self.blocks.get(name).and_then(Weak::upgrade) {
            return live;
        }
        let blocks = FileBlocks::new(extents);
        self.blocks.insert(String::from(name), Arc::downgrade(&blocks));
        blocks
    }

    /// Give up every backing that reads `name`'s blocks; call before the blocks are
    /// reused, or the next file's backing reads its data.
    fn revoke(&mut self, name: &str) {
        if let Some(blocks) = self.blocks.remove(name).as_ref().and_then(Weak::upgrade) {
            blocks.revoke();
        }
    }
}

/// `bcachefs::Mounted::rename` replaces atomically and resolves the source
/// before it touches the tree, so the move commits before the displaced
/// destination's in-memory state is freed: a rename that fails to find its
/// source frees nothing.
impl ReplaceRename for BcacheFsAdapter {
    type Displaced = Option<FileId>;

    fn source_present(&mut self, old: &str) -> Result<bool, SyscallError> {
        if self.name_to_id.contains_key(old) {
            return Ok(true);
        }
        Ok(mapped("exists", old, self.fs.file_mtime(old))?.is_some())
    }

    fn same_object(&mut self, old: &str, new: &str) -> Result<bool, SyscallError> {
        // bcachefs hashes the exact name; equal strings are the one entry.
        Ok(old == new)
    }

    fn commit(&mut self, old: &str, new: &str) -> Result<Committed<Option<FileId>>, SyscallError> {
        // Capture the displaced destination's in-memory id but free nothing: the
        // backend move replaces the tree entry atomically, and only its success
        // licenses freeing what the old destination held.
        let displaced = self.name_to_id.get(new).copied();
        mapped("rename", old, self.fs.rename(old, new))?;
        Ok(Committed::new(displaced))
    }

    fn release(&mut self, old: &str, new: &str, committed: Committed<Option<FileId>>) {
        if let Some(target_id) = committed.into_displaced() {
            if file_cache::mark_deleted(target_id) == Residency::Gone {
                self.open_files.remove(&target_id);
            }
            self.name_to_id.remove(new);
        }
        // The old destination's backings read blocks the move has reassigned.
        self.revoke(new);
        if let Some(file_id) = self.name_to_id.remove(old) {
            self.name_to_id.insert(String::from(new), file_id);
            if let Some(info) = self.open_files.get_mut(&file_id) {
                info.name = String::from(new);
            }
        }
        if let Some(blocks) = self.blocks.remove(old) {
            self.blocks.insert(String::from(new), blocks);
        }
    }
}

impl FileSystem for BcacheFsAdapter {
    /// Checked after the work, not before: `bcachefs::Mounted::list` exposes no count to check first.
    fn list(&mut self, limit: usize) -> Result<Vec<(String, u64)>, SyscallError> {
        // An empty listing on error would be a lie indistinguishable from an empty directory.
        let names = mapped("list", "/", self.fs.list())?;
        if names.len() > limit {
            return Err(SyscallError::ResourceExhausted);
        }
        Ok(names)
    }

    fn file_mtime(&mut self, name: &str) -> Result<u64, SyscallError> {
        present("file_mtime", name, self.fs.file_mtime(name))
    }

    fn read_link(&mut self, name: &str) -> Result<Option<String>, SyscallError> {
        mapped("read_link", name, self.fs.read_link(name))
    }

    fn open_file(&mut self, name: &str) -> Result<(FileId, Option<Arc<dyn FileBacking>>), SyscallError> {
        if let Some(&file_id) = self.name_to_id.get(name) {
            let held = file_cache::open(file_id);
            let info = self.open_files.get(&file_id).ok_or(SyscallError::NotFound)?;
            let backing = Arc::new(NvmeBacking::new(
                Arc::clone(&info.blocks),
                file_cache::size(file_id),
            ));
            held.commit();
            return Ok((file_id, Some(backing)));
        }

        let (extents, size) = present("open", name, self.fs.file_extents(name))?;
        let blocks = self.blocks_for(name, extents);
        let file_id = file_cache::create_file(true); // evictable
        file_cache::set_size(file_id, size);

        self.name_to_id.insert(String::from(name), file_id);
        self.open_files.insert(file_id, OpenFileInfo {
            name: String::from(name),
            blocks: Arc::clone(&blocks),
        });

        Ok((file_id, Some(Arc::new(NvmeBacking::new(blocks, size)))))
    }

    fn create(&mut self, name: &str, mtime: u64) -> Result<FileId, SyscallError> {
        if let Some(&file_id) = self.name_to_id.get(name) {
            return Ok(file_id);
        }

        // `Mounted::create` frees whatever answered to this name; revoke first.
        self.revoke(name);
        mapped("create", name, self.fs.create(name, &[], mtime))?;

        let file_id = file_cache::create_file(true);
        self.name_to_id.insert(String::from(name), file_id);
        let blocks = self.blocks_for(name, Vec::new());
        self.open_files.insert(file_id, OpenFileInfo {
            name: String::from(name),
            blocks,
        });
        Ok(file_id)
    }

    fn close_file(&mut self, file_id: FileId) {
        if let Some(info) = self.open_files.remove(&file_id) {
            self.name_to_id.remove(&info.name);
        }
    }

    fn delete(&mut self, name: &str) -> Result<(), SyscallError> {
        if let Some(&file_id) = self.name_to_id.get(name) {
            if file_cache::mark_deleted(file_id) == Residency::Gone {
                self.open_files.remove(&file_id);
            }
            self.name_to_id.remove(name);
        }
        self.revoke(name);
        if mapped("delete", name, self.fs.delete(name))? {
            Ok(())
        } else {
            Err(SyscallError::NotFound)
        }
    }

    fn rename(&mut self, old: &str, new: &str) -> Result<(), SyscallError> {
        fs_rename::replace_rename(self, old, new)
    }

    fn write_page(&mut self, file_id: FileId, page_idx: u32, data: &[u8; PAGE_BYTES]) -> Result<(), SyscallError> {
        let info = self.open_files.get(&file_id).ok_or(SyscallError::NotFound)?;
        let name = info.name.clone();
        let blocks = Arc::clone(&info.blocks);
        let block = blocks
            .with(|extents| self.fs.resolve_or_alloc_block(extents, page_idx))
            .ok_or(SyscallError::NotFound)?;
        let block = mapped("block allocation", &name, block)?;
        page_cache::raw_block_write(block, data).map_err(|e| {
            log!("bcachefs: write of block {block} for '{name}' refused: {e:?}");
            as_device_refusal(DeviceError::from(e))
        })
    }

    fn update_metadata(&mut self, file_id: FileId, size: u64, mtime: u64) -> Result<(), SyscallError> {
        let info = self.open_files.get(&file_id).ok_or(SyscallError::NotFound)?;
        let name = info.name.clone();
        let extents = Arc::clone(&info.blocks)
            .with(|extents| extents.clone())
            .ok_or(SyscallError::NotFound)?;
        mapped("update_metadata", &name, self.fs.update_metadata(&name, &extents, size, mtime))
    }

    fn create_symlink(&mut self, name: &str, target: &str) -> Result<(), SyscallError> {
        // As `create`: displaces whatever answered to this name; revoke first.
        self.revoke(name);
        mapped("create_symlink", name, self.fs.create_symlink(name, target))
    }

    fn sync(&mut self) -> Result<(), SyscallError> {
        mapped("sync", "/", self.fs.sync())
    }

    fn open_backing(&mut self, name: &str) -> Result<Arc<dyn FileBacking>, SyscallError> {
        let (extents, size) = present("open_backing", name, self.fs.file_extents(name))?;
        let blocks = self.blocks_for(name, extents);
        Ok(Arc::new(NvmeBacking::new(blocks, size)))
    }
}

/// VFS adapter for read-only bcachefs (initrd mounted in memory).
///
/// Holds the image, not its base address, so every backing bounds-checks against the same length.
pub struct ReadOnlyBcacheFsAdapter {
    fs: Mounted<SliceBlockIO, ReadOnly>,
    image: SliceBlockIO,
    name_to_id: HashMap<String, FileId>,
}

impl ReadOnlyBcacheFsAdapter {
    pub fn new(fs: Mounted<SliceBlockIO, ReadOnly>, image: SliceBlockIO) -> Self {
        Self { fs, image, name_to_id: HashMap::new() }
    }
}

impl FileSystem for ReadOnlyBcacheFsAdapter {
    /// Checked after the work, not before: `bcachefs::Mounted::list` exposes no count to check first.
    fn list(&mut self, limit: usize) -> Result<Vec<(String, u64)>, SyscallError> {
        let names = mapped("list", "/", self.fs.list())?;
        if names.len() > limit {
            return Err(SyscallError::ResourceExhausted);
        }
        Ok(names)
    }

    fn file_mtime(&mut self, name: &str) -> Result<u64, SyscallError> {
        present("file_mtime", name, self.fs.file_mtime(name))
    }

    fn read_link(&mut self, name: &str) -> Result<Option<String>, SyscallError> {
        mapped("read_link", name, self.fs.read_link(name))
    }

    fn open_file(&mut self, name: &str) -> Result<(FileId, Option<Arc<dyn FileBacking>>), SyscallError> {
        let (extents, size) = present("open", name, self.fs.file_extents(name))?;
        if let Some(&file_id) = self.name_to_id.get(name) {
            file_cache::open(file_id).commit();
            let backing = Arc::new(InitrdBacking::new(self.image, extents, size));
            return Ok((file_id, Some(backing)));
        }

        let file_id = file_cache::create_file(true);
        file_cache::set_size(file_id, size);

        self.name_to_id.insert(String::from(name), file_id);

        let backing = Arc::new(InitrdBacking::new(self.image, extents, size));
        Ok((file_id, Some(backing)))
    }

    fn create(&mut self, _name: &str, _mtime: u64) -> Result<FileId, SyscallError> {
        Err(SyscallError::PermissionDenied)
    }

    fn close_file(&mut self, file_id: FileId) {
        let name = self.name_to_id.iter()
            .find(|(_, &v)| v == file_id)
            .map(|(k, _)| k.clone());
        if let Some(name) = name {
            self.name_to_id.remove(&name);
        }
    }

    /// `PermissionDenied`, not `Io`: retrying this write is never right, unlike a device retry.
    fn delete(&mut self, _name: &str) -> Result<(), SyscallError> {
        Err(SyscallError::PermissionDenied)
    }

    fn rename(&mut self, _old: &str, _new: &str) -> Result<(), SyscallError> {
        Err(SyscallError::PermissionDenied)
    }

    fn write_page(&mut self, _file_id: FileId, _page_idx: u32, _data: &[u8; PAGE_BYTES]) -> Result<(), SyscallError> {
        Err(SyscallError::PermissionDenied)
    }

    fn update_metadata(&mut self, _file_id: FileId, _size: u64, _mtime: u64) -> Result<(), SyscallError> {
        Err(SyscallError::PermissionDenied)
    }

    fn create_symlink(&mut self, _name: &str, _target: &str) -> Result<(), SyscallError> {
        Err(SyscallError::PermissionDenied)
    }

    fn sync(&mut self) -> Result<(), SyscallError> {
        Ok(())
    }

    fn open_backing(&mut self, name: &str) -> Result<Arc<dyn FileBacking>, SyscallError> {
        let (extents, size) = present("open_backing", name, self.fs.file_extents(name))?;
        Ok(Arc::new(InitrdBacking::new(self.image, extents, size)))
    }
}

/// Format a new bcachefs filesystem on the NVMe device via PageCache.
///
/// Destroys everything on the device; only [`probe`] may call it, and only on [`Storage::Designated`].
fn format() -> Option<Mounted<PageCacheBlockIO, ReadWrite>> {
    match Formatted::format(PageCacheBlockIO) {
        Ok(fs) => Some(fs.mount()),
        Err(err) => {
            // A half-written volume is not one to mount; `open_home` falls back to tmpfs.
            log!("storage: formatting the designated device failed: {:?}", err);
            None
        }
    }
}

/// Try to mount an existing bcachefs filesystem from NVMe.
fn mount() -> Option<Mounted<PageCacheBlockIO, ReadWrite>> {
    let io = PageCacheBlockIO;
    Mounted::<PageCacheBlockIO, ReadWrite>::open(io).ok()
}

/// What the machine's block device is, as far as we are entitled to care.
///
/// No fourth arm and no default that writes: `Foreign` covers both someone else's disk and a blank one.
pub enum Storage {
    /// A ToyOS volume, mounted read-write, identified by its own superblock.
    Ours(Mounted<PageCacheBlockIO, ReadWrite>),
    /// Carries a designation stamp naming its own size: consent to destroy what is here.
    Designated,
    /// Anything else. Never written to, under any circumstances.
    Foreign,
}

/// Decide what the device is, from one read of block 0.
///
/// A failed mount is not consent: an unformatted disk, another OS, and a corrupt volume all read as `None`.
/// One read decides all three because bcachefs's own superblock also lives at block 0.
pub fn probe() -> Storage {
    if let Some(fs) = mount() {
        log!("storage: mounted the ToyOS volume at block 0");
        return Storage::Ours(fs);
    }
    if designated() {
        log!("storage: block 0 designates this device for ToyOS — formatting it");
        return Storage::Designated;
    }
    log!(
        "storage: no ToyOS volume and no designation stamp at block 0 — this disk is not \
         ours and nothing will be written to it"
    );
    Storage::Foreign
}

/// Whether block 0 carries a designation stamp for a device of *this* size.
///
/// The size is checked so a copied image cannot designate a different disk.
fn designated() -> bool {
    let mut guard = page_cache::lock();
    let blocks = guard.block_count();
    let (cache, dev) = guard.cache_and_dev();
    // A read error is not consent to format.
    let Ok(block0) = cache.read(dev, 0) else {
        log!("storage: block 0 could not be read; this disk is not ours to format");
        return false;
    };

    let magic = bcachefs::DESIGNATION_MAGIC;
    if block0.len() < bcachefs::DESIGNATION_BLOCKS_OFFSET + 8
        || block0[..magic.len()] != magic
    {
        return false;
    }
    let mut stamped = [0u8; 8];
    stamped.copy_from_slice(
        &block0[bcachefs::DESIGNATION_BLOCKS_OFFSET..bcachefs::DESIGNATION_BLOCKS_OFFSET + 8],
    );
    let stamped = u64::from_le_bytes(stamped);
    if stamped != blocks {
        log!(
            "storage: a designation stamp at block 0 names {} blocks, but this device has {} — \
             ignoring it",
            stamped, blocks
        );
        return false;
    }
    true
}

/// The `/home` filesystem, and the only path on which `format` runs.
///
/// `None` means the device is not ours; the caller falls back to a volatile tmpfs
/// rather than panicking or formatting without consent.
pub fn open_home() -> Option<Mounted<PageCacheBlockIO, ReadWrite>> {
    match probe() {
        Storage::Ours(fs) => Some(fs),
        Storage::Designated => format(),
        Storage::Foreign => None,
    }
}

/// Mount a read-only bcachefs filesystem from an image already in memory (initrd).
///
/// Takes a `SliceBlockIO`, not `(ptr, len)`: the unsafety claim belongs at `SliceBlockIO::new`.
pub fn mount_initrd(image: SliceBlockIO) -> Mounted<SliceBlockIO, ReadOnly> {
    Mounted::<SliceBlockIO, ReadOnly>::open(image).expect("Failed to mount bcachefs initrd")
}
