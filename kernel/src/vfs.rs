use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use hashbrown::HashMap;

use core::ops::{Deref, DerefMut};
use toyos_abi::syscall::SyscallError;
use crate::durability::Owed;
use crate::file_cache::FileId;
use crate::mm::PAGE_BYTES;
use crate::sync::{Lock, LockGuard};

static VFS: Lock<Option<Vfs>> = Lock::new(None);

pub fn init() {
    *VFS.lock() = Some(Vfs::new());
}

pub struct VfsGuard(LockGuard<'static, Option<Vfs>>);

impl Deref for VfsGuard {
    type Target = Vfs;
    fn deref(&self) -> &Vfs { self.0.as_ref().expect("VFS not initialized") }
}

impl DerefMut for VfsGuard {
    fn deref_mut(&mut self) -> &mut Vfs { self.0.as_mut().expect("VFS not initialized") }
}

pub fn lock() -> VfsGuard {
    VfsGuard(VFS.lock())
}

/// Holds one flush of the staged file inside its size-read/`update_metadata`
/// pair and says which way the race went; 400ms, under the ticket lock's tripwire.
#[cfg(feature = "boot-actuators")]
fn stalled_metadata_window(path: &str, file_id: FileId, size_read: u64) {
    if !crate::actuator::ftruncate_flush_stall() || !path.ends_with("truncate-race.bin") {
        return;
    }
    const STALL_NS: u64 = 400_000_000;
    let until = crate::clock::nanos_since_boot().saturating_add(STALL_NS);
    while crate::clock::nanos_since_boot() < until {
        core::hint::spin_loop();
    }
    let size_after = crate::file_cache::size(file_id);
    if size_after == size_read {
        crate::log!("vfs: STALLED WINDOW HELD — {path} still {size_read} bytes after 400ms");
    } else {
        crate::log!(
            "vfs: STALLED WINDOW BROKEN — a truncate landed inside {path}'s metadata window \
             ({size_read} -> {size_after})"
        );
    }
}

/// A device that would not answer is `Io`; `NotFound` means only that the name is absent.
pub trait FileSystem: Send {
    /// Every name at or under the directory `dir` (`""` for the mount root), or
    /// `ResourceExhausted` above `limit` — which counts what `dir` holds, never
    /// what the mount does. [`under_directory`] is the membership rule.
    fn list(&mut self, dir: &str, limit: usize) -> Result<Vec<(String, u64)>, SyscallError>;

    /// When `name` was last written, in whatever epoch the mount keeps.
    fn file_mtime(&mut self, name: &str) -> Result<u64, SyscallError>;

    /// What `name` points at; `Ok(None)` for a non-link or an absent name, never for a device that would not answer.
    fn read_link(&mut self, name: &str) -> Result<Option<String>, SyscallError>;

    /// Open `name`: the same `FileId` on every open of the same file, plus a backing for cache misses.
    fn open_file(&mut self, name: &str) -> Result<(FileId, Option<alloc::sync::Arc<dyn crate::file_backing::FileBacking>>), SyscallError>;
    /// Create an empty file, registered under `name`.
    fn create(&mut self, name: &str, mtime: u64) -> Result<FileId, SyscallError>;
    /// Release filesystem state for `file_id`, after the file cache dropped its last reference under the VFS lock.
    fn close_file(&mut self, file_id: FileId);

    /// Unlink `name`, or `NotFound` if there was nothing by that name.
    fn delete(&mut self, name: &str) -> Result<(), SyscallError>;
    fn rename(&mut self, old: &str, new: &str) -> Result<(), SyscallError>;

    /// Create the directory `name` on the volume; `NotSupported` from a mount
    /// with no directory representation makes the VFS carry it instead.
    fn create_dir(&mut self, name: &str) -> Result<(), SyscallError>;
    /// Remove the empty directory `name`, refusing a file, a missing name and
    /// a non-empty directory each by its own error.
    fn remove_dir(&mut self, name: &str) -> Result<(), SyscallError>;

    /// Write one dirty page to the device, allocating its block if needed.
    fn write_page(&mut self, file_id: FileId, page_idx: u32, data: &[u8; PAGE_BYTES]) -> Result<(), SyscallError>;
    /// Update file metadata (size, mtime) after flushing dirty pages.
    fn update_metadata(&mut self, file_id: FileId, size: u64, mtime: u64) -> Result<(), SyscallError>;

    fn create_symlink(&mut self, name: &str, target: &str) -> Result<(), SyscallError>;

    /// An implementation of `sync` must not swallow a lower-level failure and report success: the log depends on this call telling the truth about durability.
    fn sync(&mut self) -> Result<(), SyscallError>;

    /// `open_backing` has no default body: an unimplemented one would silently report every file on that mount as missing (the sentinel this trait exists to remove).
    fn open_backing(&mut self, name: &str) -> Result<alloc::sync::Arc<dyn crate::file_backing::FileBacking>, SyscallError>;
}


/// Whether userland may modify a mount; stated at every `mount`, defaulted nowhere.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UserAccess {
    ReadWrite,
    /// Readable, and every userland syscall that would change it is refused.
    KernelOnly,
}

struct Mount {
    fs: Box<dyn FileSystem>,
    access: UserAccess,
    /// The device commit this mount still owes: raised by every flush that may
    /// have reached the device, settled only by a [`FileSystem::sync`] that returned `Ok`.
    commit: Owed,
}

/// A path with every symlink followed, minted only by [`Vfs::resolve_for_open`]:
/// the mount an `OpenTarget` names is the mount opened, never one a link aimed away from.
pub struct OpenTarget(String);

impl OpenTarget {
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn into_string(self) -> String { self.0 }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResolveIntent {
    KernelOrRead,
    UserModify,
}

/// Virtual filesystem that dispatches to named mount points.
pub struct Vfs {
    root: Option<Box<dyn FileSystem>>,
    root_commit: Owed,
    mounts: HashMap<String, Mount>,
    created_dirs: hashbrown::HashSet<String>,
}

/// `MAX_PATH` exists because `resolve_absolute` prepends `cwd` before `normalize`, defeating `MAX_USER_STR`'s per-argument bound unless `cwd` is separately bounded.
pub const MAX_PATH: usize = 4096;

/// The most entries one `FileSystem::list` may materialise for one directory.
pub const MAX_LIST_ENTRIES: usize = 16_384;

/// Whether a mount's `name` is the directory `dir` itself or lies beneath it;
/// an empty `dir` is the mount root, which every name lies under.
pub fn under_directory(name: &str, dir: &str) -> bool {
    dir.is_empty()
        || name == dir
        || (name.starts_with(dir) && name.as_bytes().get(dir.len()) == Some(&b'/'))
}

/// The most directories `created_dirs` holds before `mkdir` refuses: each is a
/// userland-chosen key, bounded the way `list` is by [`MAX_LIST_ENTRIES`].
pub const MAX_CREATED_DIRS: usize = 16_384;

const _: () = assert!(core::mem::size_of::<(String, u64)>() == 32);

/// The `created_dirs` key for a directory — the one construction its writer and readers share.
fn directory(mount: &str, subdir: &str) -> String {
    if subdir.is_empty() {
        format!("/{mount}")
    } else {
        format!("/{mount}/{subdir}")
    }
}

fn normalize(path: &str) -> String {
    // `parts` is the allocation `MAX_PATH` is sized against.
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => { parts.pop(); }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        String::from("/")
    } else {
        format!("/{}", parts.join("/"))
    }
}

impl Vfs {
    fn new() -> Self {
        Self {
            root: None,
            root_commit: Owed::new(),
            mounts: HashMap::new(),
            created_dirs: hashbrown::HashSet::new(),
        }
    }

    pub fn set_root(&mut self, fs: Box<dyn FileSystem>) {
        self.root = Some(fs);
    }

    pub fn mount(&mut self, name: &str, fs: Box<dyn FileSystem>, access: UserAccess) {
        self.mounts.insert(String::from(name), Mount { fs, access, commit: Owed::new() });
    }

    /// May a syscall acting for userland change what is at `path`?
    pub fn user_may_modify(&self, path: &str) -> bool {
        let (mount, _) = self.resolve_path("/", path);
        self.mounts.get(&mount).is_none_or(|m| m.access == UserAccess::ReadWrite)
    }

    fn resolve_fs(&mut self, mount: &str, file: &str) -> Option<(&mut dyn FileSystem, String)> {
        if let Some(mount) = self.mounts.get_mut(mount) {
            return Some((mount.fs.as_mut(), String::from(file)));
        }
        if let Some(root) = self.root.as_deref_mut() {
            let root_path = if file.is_empty() {
                String::from(mount)
            } else {
                format!("{}/{}", mount, file)
            };
            return Some((root, root_path));
        }
        None
    }

    pub fn resolve_absolute(&self, cwd: &str, path: &str) -> String {
        if path.starts_with('/') {
            normalize(path)
        } else if cwd == "/" {
            normalize(&format!("/{}", path))
        } else {
            normalize(&format!("{}/{}", cwd, path))
        }
    }

    pub fn resolve_path(&self, cwd: &str, arg: &str) -> (String, String) {
        let full = if arg.starts_with('/') {
            normalize(arg)
        } else if cwd == "/" {
            normalize(&format!("/{}", arg))
        } else {
            normalize(&format!("{}/{}", cwd, arg))
        };

        if full == "/" {
            return (String::new(), String::new());
        }

        let without_leading = &full[1..];
        if let Some(pos) = without_leading.find('/') {
            let mount = &without_leading[..pos];
            let file = &without_leading[pos + 1..];
            (String::from(mount), String::from(file))
        } else {
            (String::from(without_leading), String::new())
        }
    }

    pub fn cd(&mut self, cwd: &str, target: &str) -> Result<String, SyscallError> {
        let (mount, subdir) = self.resolve_path(cwd, target);

        if mount.is_empty() {
            return Ok(String::from("/"));
        }

        let abs = directory(&mount, &subdir);

        // Refused rather than truncated: a shortened path names a different directory.
        if abs.len() > MAX_PATH {
            return Err(SyscallError::InvalidArgument);
        }

        if self.created_dirs.contains(&abs) {
            return Ok(abs);
        }

        let is_named = self.mounts.contains_key(&mount);
        if subdir.is_empty() && is_named {
            return Ok(abs);
        }

        let (fs, fs_path) = self.resolve_fs(&mount, &subdir).ok_or(SyscallError::NotFound)?;
        let prefix = format!("{}/", fs_path);
        // A mount too large to list cannot be entered either: the answer needs the same allocation.
        let names = fs.list(&fs_path, MAX_LIST_ENTRIES)?;
        if names.iter().any(|(name, _)| name.starts_with(&prefix) || *name == fs_path) {
            return Ok(abs);
        }

        Err(SyscallError::NotFound)
    }

    /// `list` refuses above [`MAX_LIST_ENTRIES`] rather than truncating because a short listing is a confidently wrong answer to a caller checking existence or deleting a tree.
    pub fn list(&mut self, cwd: &str, path: &str) -> Result<Vec<(String, u64)>, SyscallError> {
        let (mount, subdir) = if path.is_empty() {
            self.resolve_path(cwd, "")
        } else {
            self.resolve_path(cwd, path)
        };

        if mount.is_empty() {
            let mut result = Vec::new();
            let mut seen_dirs = hashbrown::HashSet::new();

            for name in self.mounts.keys() {
                let dir_name = format!("{}/", name);
                if seen_dirs.insert(dir_name.clone()) {
                    result.push((dir_name, 0));
                }
            }

            if let Some(root) = self.root.as_deref_mut() {
                for (name, _size) in root.list("", MAX_LIST_ENTRIES)? {
                    if let Some(slash_pos) = name.find('/') {
                        let dir_name = format!("{}/", &name[..slash_pos]);
                        if seen_dirs.insert(dir_name.clone()) {
                            result.push((dir_name, 0));
                        }
                    }
                }
            }

            return Ok(result);
        }

        let (fs, fs_path) = self.resolve_fs(&mount, &subdir)
            .ok_or(SyscallError::NotFound)?;
        let all_files = fs.list(&fs_path, MAX_LIST_ENTRIES)?;

        let prefix = if fs_path.is_empty() {
            String::new()
        } else {
            format!("{}/", fs_path)
        };

        fn under_prefix<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
            if prefix.is_empty() { Some(name) } else { name.strip_prefix(prefix) }
        }

        // Dedup only removes entries, so `matching` is a true upper bound on the final result count.
        let matching = all_files.iter().filter(|(n, _)| under_prefix(n, &prefix).is_some()).count();
        let mut result = Vec::with_capacity(matching);
        let mut seen_dirs = hashbrown::HashSet::new();
        let mut saw_self = false;

        for (name, size) in &all_files {
            let Some(rest) = under_prefix(name, &prefix) else { continue };

            // The listed directory's own entry: proof it exists, not a child.
            if rest.is_empty() {
                saw_self = true;
                continue;
            }
            if let Some(slash_pos) = rest.find('/') {
                let dir_name = format!("{}/", &rest[..slash_pos]);
                if seen_dirs.insert(dir_name.clone()) {
                    result.push((dir_name, 0));
                }
            } else {
                result.push((String::from(rest), *size));
            }
        }

        // An empty directory's witnesses: its own listing entry on a mount
        // that represents directories, `created_dirs` on one the VFS carries.
        if result.is_empty()
            && !saw_self
            && !prefix.is_empty()
            && !self.created_dirs.contains(&directory(&mount, &subdir))
        {
            return Err(SyscallError::NotFound);
        }
        Ok(result)
    }

    pub fn resolve_for_open(&mut self, path: &str, intent: ResolveIntent) -> Result<OpenTarget, SyscallError> {
        let target = self.resolve_for_open_depth(path, 0)?;
        if intent == ResolveIntent::UserModify && !self.user_may_modify(target.as_str()) {
            return Err(SyscallError::PermissionDenied);
        }
        Ok(target)
    }

    fn resolve_for_open_depth(&mut self, path: &str, depth: u32) -> Result<OpenTarget, SyscallError> {
        if depth > 10 { return Err(SyscallError::InvalidArgument); }
        let (mount, file) = self.resolve_path("/", path);
        if mount.is_empty() { return Err(SyscallError::NotFound); }
        let is_named = self.mounts.contains_key(&mount);
        let (fs, fs_path) = self.resolve_fs(&mount, &file).ok_or(SyscallError::NotFound)?;
        if fs_path.is_empty() { return Err(SyscallError::NotFound); }
        if let Some(target) = fs.read_link(&fs_path)? {
            let next = if is_named {
                format!("/{}/{}", mount, target)
            } else {
                format!("/{}", target)
            };
            return self.resolve_for_open_depth(&next, depth + 1);
        }
        Ok(OpenTarget(if file.is_empty() { format!("/{mount}") } else { format!("/{mount}/{file}") }))
    }

    fn fs_for_target(&mut self, target: &OpenTarget) -> Result<(&mut dyn FileSystem, String), SyscallError> {
        let (mount, file) = self.resolve_path("/", target.as_str());
        if mount.is_empty() { return Err(SyscallError::NotFound); }
        let (fs, fs_path) = self.resolve_fs(&mount, &file).ok_or(SyscallError::NotFound)?;
        if fs_path.is_empty() { return Err(SyscallError::NotFound); }
        Ok((fs, fs_path))
    }

    pub fn open_target(&mut self, target: &OpenTarget) -> Result<FileId, SyscallError> {
        let (fs, fs_path) = self.fs_for_target(target)?;
        let (file_id, backing) = fs.open_file(&fs_path)?;
        if let Some(backing) = backing {
            crate::file_cache::set_backing(file_id, backing);
        }
        Ok(file_id)
    }

    pub fn mtime_target(&mut self, target: &OpenTarget) -> Result<u64, SyscallError> {
        let (fs, fs_path) = self.fs_for_target(target)?;
        fs.file_mtime(&fs_path)
    }

    /// Create a new empty file. Returns FileId.
    pub fn create_file(&mut self, path: &str, mtime: u64) -> Result<FileId, SyscallError> {
        let (mount, file) = self.resolve_path("/", path);
        if mount.is_empty() { return Err(SyscallError::InvalidArgument); }
        let (fs, fs_path) = self.resolve_fs(&mount, &file).ok_or(SyscallError::NotFound)?;
        if fs_path.is_empty() { return Err(SyscallError::InvalidArgument); }
        fs.create(&fs_path, mtime)
    }

    /// No early return on an empty dirty set: `ftruncate` changes the size without dirtying a page.
    /// A refused attempt restores nothing, because nothing was cleared: debt is
    /// settled per page against what was copied, and for the file only past `update_metadata`.
    pub fn flush_file(&mut self, path: &str, file_id: FileId, mtime: u64) -> Result<(), SyscallError> {
        let plan = crate::file_cache::begin_flush(file_id);
        let (mount, file) = self.resolve_path("/", path);
        if mount.is_empty() { return Err(SyscallError::InvalidArgument); }
        // Raised before the first write, not after the last: a flush that failed
        // half-way may already have reached the device's cache.
        self.commit_of(&mount).record_write();
        let (fs, fs_path) = self.resolve_fs(&mount, &file).ok_or(SyscallError::NotFound)?;
        if fs_path.is_empty() { return Err(SyscallError::InvalidArgument); }

        // On the heap: the idle loop's 16 KiB stack has no guard page.
        let mut heap = alloc::vec![0u8; PAGE_BYTES].into_boxed_slice();
        let buf: &mut [u8; PAGE_BYTES] = (&mut heap[..]).try_into().expect("PAGE_BYTES bytes");
        let mut flushed: Vec<(u32, crate::durability::Settlement)> =
            Vec::with_capacity(plan.pages.len());
        for &page_idx in &plan.pages {
            if let Some(copied) = crate::file_cache::copy_page_out(file_id, page_idx, buf) {
                fs.write_page(file_id, page_idx, buf)?;
                flushed.push((page_idx, copied));
            }
        }
        crate::file_cache::settle_pages(file_id, &flushed);

        let size = crate::file_cache::size(file_id);
        #[cfg(feature = "boot-actuators")]
        stalled_metadata_window(path, file_id, size);
        fs.update_metadata(file_id, size, mtime)?;
        crate::file_cache::settle_file(file_id, plan.file);

        // A refusal is logged, not returned: the bytes are on the device; only evictability is lost.
        if !crate::file_cache::has_backing(file_id) {
            match fs.open_backing(&fs_path) {
                Ok(backing) => crate::file_cache::set_backing(file_id, backing),
                Err(e) => log!("vfs: {path} was flushed but has no backing to evict through: {e}"),
            }
        }
        Ok(())
    }

    /// Close a file (release filesystem state when last ref drops).
    pub fn close_file(&mut self, path: &str, file_id: FileId) {
        let (mount, file) = self.resolve_path("/", path);
        if mount.is_empty() { return; }
        if let Some((fs, _fs_path)) = self.resolve_fs(&mount, &file) {
            fs.close_file(file_id);
        }
    }

    /// Unlink `path` on its mount.
    pub fn delete_file(&mut self, path: &str) -> Result<(), SyscallError> {
        let (mount, file) = self.resolve_path("/", path);
        if mount.is_empty() { return Err(SyscallError::InvalidArgument); }
        let (fs, fs_path) = self.resolve_fs(&mount, &file).ok_or(SyscallError::NotFound)?;
        if fs_path.is_empty() { return Err(SyscallError::InvalidArgument); }
        fs.delete(&fs_path)
    }

    pub fn rename(&mut self, old_path: &str, new_path: &str) -> Result<(), SyscallError> {
        let (old_mount, old_file) = self.resolve_path("/", old_path);
        let (new_mount, new_file) = self.resolve_path("/", new_path);
        if old_mount.is_empty() || new_mount.is_empty() { return Err(SyscallError::InvalidArgument); }
        if old_mount != new_mount { return Err(SyscallError::NotSupported); }
        let is_named = self.mounts.contains_key(&old_mount);
        let (fs, old_fs_path) = self.resolve_fs(&old_mount, &old_file).ok_or(SyscallError::NotFound)?;
        let new_fs_path = if is_named {
            String::from(&new_file)
        } else if new_file.is_empty() {
            String::from(&new_mount)
        } else {
            format!("{}/{}", new_mount, new_file)
        };
        if old_fs_path.is_empty() || new_fs_path.is_empty() { return Err(SyscallError::InvalidArgument); }
        fs.rename(&old_fs_path, &new_fs_path)
    }

    /// The `Result` is as much the point as the bound: a caller that discards it and reports success anyway turns the bound into a silent failure.
    pub fn create_dir(&mut self, path: &str) -> Result<(), SyscallError> {
        if path.len() > MAX_PATH {
            return Err(SyscallError::InvalidArgument);
        }
        let (mount, subdir) = self.resolve_path("/", path);
        // `/` and a mount root already exist.
        if subdir.is_empty() {
            return Err(SyscallError::AlreadyExists);
        }
        if let Some((fs, fs_path)) = self.resolve_fs(&mount, &subdir) {
            match fs.create_dir(&fs_path) {
                // No directory representation on this mount; carried below.
                Err(SyscallError::NotSupported) => {}
                outcome => return outcome,
            }
        }
        // A new key past the cap is refused rather than grown; a repeat of one already held costs nothing and is let through.
        if !self.created_dirs.contains(path) && self.created_dirs.len() >= MAX_CREATED_DIRS {
            return Err(SyscallError::ResourceExhausted);
        }
        self.created_dirs.insert(String::from(path));
        Ok(())
    }

    /// Remove an empty directory, reporting the real outcome — the `Result` is
    /// the point, as in [`Self::create_dir`]. A mount root, a missing name, a
    /// file, and a non-empty directory are each refused, not reported as removed.
    pub fn remove_dir(&mut self, path: &str) -> Result<(), SyscallError> {
        let (mount, subdir) = self.resolve_path("/", path);
        // A mount point (and `/`) is not a directory a caller may remove.
        if subdir.is_empty() {
            return Err(SyscallError::InvalidArgument);
        }
        let dir = directory(&mount, &subdir);

        let forwarded = {
            let (fs, fs_path) = self.resolve_fs(&mount, &subdir).ok_or(SyscallError::NotFound)?;
            fs.remove_dir(&fs_path)
        };
        match forwarded {
            // No directory representation on this mount; judged below from
            // the listing and `created_dirs`, as `create_dir` carried it.
            Err(SyscallError::NotSupported) => {}
            Ok(()) => {
                self.created_dirs.remove(&dir);
                return Ok(());
            }
            outcome => return outcome,
        }

        let (fs, fs_path) = self.resolve_fs(&mount, &subdir).ok_or(SyscallError::NotFound)?;
        let names = fs.list(&fs_path, MAX_LIST_ENTRIES)?;
        let child_prefix = format!("{fs_path}/");
        let is_file = names.iter().any(|(n, _)| *n == fs_path);
        // A listing mount's own `name/` self-entry is not a child, or every empty directory reads non-empty.
        let has_file_child =
            names.iter().any(|(n, _)| n.starts_with(&child_prefix) && *n != child_prefix);

        // The name resolves to a file, not a directory.
        if is_file {
            return Err(SyscallError::InvalidArgument);
        }

        let created_prefix = format!("{dir}/");
        let has_created_child = self.created_dirs.iter().any(|d| d.starts_with(&created_prefix));
        if !(self.created_dirs.contains(&dir) || has_file_child || has_created_child) {
            return Err(SyscallError::NotFound);
        }
        if has_file_child || has_created_child {
            return Err(SyscallError::InvalidArgument);
        }
        self.created_dirs.remove(&dir);
        Ok(())
    }

    pub fn create_symlink(&mut self, path: &str, target: &str) -> Result<(), SyscallError> {
        let (mount, file) = self.resolve_path("/", path);
        if mount.is_empty() {
            return Err(SyscallError::InvalidArgument);
        }
        let (fs, fs_path) = self.resolve_fs(&mount, &file).ok_or(SyscallError::NotFound)?;
        if fs_path.is_empty() { return Err(SyscallError::InvalidArgument); }
        fs.create_symlink(&fs_path, target)
    }

    pub fn read_link(&mut self, path: &str) -> Result<Option<String>, SyscallError> {
        let (mount, file) = self.resolve_path("/", path);
        if mount.is_empty() {
            return Ok(None);
        }
        let Some((fs, fs_path)) = self.resolve_fs(&mount, &file) else { return Ok(None) };
        if fs_path.is_empty() { return Ok(None); }
        fs.read_link(&fs_path)
    }

    pub fn delete(&mut self, path: &str) -> Result<(), SyscallError> {
        self.delete_file(path)
    }

    /// The commit debt `mount` names — the named mount's, or the root's.
    fn commit_of(&mut self, mount: &str) -> &mut Owed {
        match self.mounts.get_mut(mount) {
            Some(m) => &mut m.commit,
            None => &mut self.root_commit,
        }
    }

    /// Whether `SYS_FSYNC` still owes this file work: its own flush, or the
    /// device commit its mount has raised and not settled — which is how a
    /// failed `sync` keeps the next fsync honest with every page flushed.
    pub fn durability_owed(&self, path: &str, file_id: FileId) -> bool {
        if crate::file_cache::flush_owed(file_id) {
            return true;
        }
        let (mount, _) = self.resolve_path("/", path);
        match self.mounts.get(&mount) {
            Some(m) => m.commit.is_owed(),
            None => self.root_commit.is_owed(),
        }
    }

    /// Make one named mount's writes durable.
    pub fn sync_mount(&mut self, name: &str) -> Result<(), SyscallError> {
        let mount = self.mounts.get_mut(name).ok_or(SyscallError::NotFound)?;
        let upto = mount.commit.snapshot();
        mount.fs.sync()?;
        mount.commit.settle(upto);
        Ok(())
    }

    /// Is there a filesystem mounted under `name`?
    pub fn has_mount(&self, name: &str) -> bool {
        self.mounts.contains_key(name)
    }

    /// `/bin/logd` publishes `LOG_DURABLE_NS` off this call's result and a panicking kernel waits on that word, so `sync_for_path` must reach the device's write cache, not stop at the page cache.
    pub fn sync_for_path(&mut self, path: &str) -> Result<(), SyscallError> {
        let (mount, _) = self.resolve_path("/", path);
        if self.mounts.contains_key(&mount) {
            return self.sync_mount(&mount);
        }
        // No root is not an error: the write being made durable cannot have happened.
        match &mut self.root {
            Some(root) => {
                let upto = self.root_commit.snapshot();
                root.sync()?;
                self.root_commit.settle(upto);
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// A refusal is logged, not returned, so one mount failing does not stop the rest.
    pub fn sync_all(&mut self) {
        if let Some(root) = &mut self.root {
            let upto = self.root_commit.snapshot();
            match root.sync() {
                Ok(()) => self.root_commit.settle(upto),
                Err(e) => log!("vfs: the root filesystem would not sync: {e}"),
            }
        }
        for (name, mount) in self.mounts.iter_mut() {
            let upto = mount.commit.snapshot();
            match mount.fs.sync() {
                Ok(()) => mount.commit.settle(upto),
                Err(e) => log!("vfs: /{name} would not sync: {e}"),
            }
        }
    }

    /// The write-back queue is drained whole, not filtered by path, because it is keyed by the name a handle was opened under, and a symlink, rename, or relative open names the same file differently.
    pub fn open_backing(&mut self, path: &str) -> Result<alloc::sync::Arc<dyn crate::file_backing::FileBacking>, SyscallError> {
        crate::writeback::drain_held(self);
        let target = self.resolve_for_open(path, ResolveIntent::KernelOrRead)?;
        let (fs, fs_path) = self.fs_for_target(&target)?;
        fs.open_backing(&fs_path)
    }
}
