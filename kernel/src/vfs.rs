use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use core::ops::{Deref, DerefMut};
use toyos_abi::syscall::{FileType, SyscallError, MAX_LINKS, MAX_PATH};
use crate::file_cache::{self, FileId};
use crate::mm::charge::{self, Charge};
use crate::object::ops::Stat;
use crate::sync::{Lock, LockGuard};
use kernel::dirnodes::Directories;

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

/// A device that would not answer is `Io`; `NotFound` means only that the name is absent.
pub trait FileSystem: Send {
    /// Every name [`under_directory`] puts at or under `dir` (`""` is the mount
    /// root), or `ResourceExhausted` above `limit`, which counts only those,
    /// or where `charge` cannot grow by what [`listed_heap`] says each takes.
    fn list(&mut self, dir: &str, limit: usize, charge: &mut Charge) -> Result<Vec<String>, SyscallError>;

    /// The number of the file or symlink `name`, not followed, as
    /// `toyos_abi::syscall::Stat::node` says: a file's is the `FileId`
    /// [`open_file`](Self::open_file) answers, a symlink's one drawn by
    /// `file_cache::draw_node`; `NotFound` for anything else.
    fn node(&mut self, name: &str) -> Result<u64, SyscallError>;

    /// Whether `dir` (`""` is the mount root) is a directory: a working
    /// directory is judged by this, on every spawn and `SYS_CHDIR`, under the
    /// VFS lock. Nothing beneath it is materialised and nothing caps it; what it costs is
    /// the mount's.
    fn is_dir(&mut self, dir: &str) -> Result<bool, SyscallError>;

    /// When `name` was last written, as `toyos_abi::syscall::Stat::mtime` says,
    /// to the precision the mount stores.
    fn file_mtime(&mut self, name: &str) -> Result<u64, SyscallError>;

    /// What `name` points at; `Ok(None)` for a non-link or an absent name, never for a device that would not answer.
    fn read_link(&mut self, name: &str) -> Result<Option<String>, SyscallError>;

    /// Open `name`: the same `FileId` on every open of the same file, plus a backing for cache misses.
    fn open_file(&mut self, name: &str) -> Result<(FileId, Option<alloc::sync::Arc<dyn crate::file_backing::FileBacking>>), SyscallError>;
    /// Create an empty file, registered under `name`.
    fn create(&mut self, name: &str, mtime: u64) -> Result<FileId, SyscallError>;

    /// Unlink `name`, or `NotFound` if there was nothing by that name.
    fn delete(&mut self, name: &str) -> Result<(), SyscallError>;
    fn rename(&mut self, old: &str, new: &str) -> Result<(), SyscallError>;

    /// Create the directory `name` on the volume; `NotSupported` from a mount
    /// with no directory representation makes the VFS carry it instead.
    fn create_dir(&mut self, name: &str) -> Result<(), SyscallError>;
    /// Remove the empty directory `name`, refusing a file, a missing name and
    /// a non-empty directory each by its own error.
    fn remove_dir(&mut self, name: &str) -> Result<(), SyscallError>;

    fn create_symlink(&mut self, name: &str, target: &str) -> Result<(), SyscallError>;

    /// `open_backing` has no default body: an unimplemented one would silently report every file on that mount as missing (the sentinel this trait exists to remove).
    fn open_backing(&mut self, name: &str) -> Result<alloc::sync::Arc<dyn crate::file_backing::FileBacking>, SyscallError>;
}


/// The entries `/` has, in listing order. `/` is synthesized rather than
/// mounted: it is no filesystem, and nothing outside this set can be mounted.
pub const ROOT_ENTRIES: [&str; 9] =
    ["apps", "boot", "config", "home", "log", "media", "state", "system", "tmp"];

struct Mount {
    fs: Box<dyn FileSystem>,
}

/// A name at `/`, and where in its filesystem that name begins.
#[derive(Clone, Copy)]
struct MountPoint {
    fs: usize,
    /// Empty for a filesystem one name reaches; the name itself for one several
    /// reach, which is how DATA is `/apps` and `/home` at once.
    prefix: &'static str,
}

impl MountPoint {
    fn path(&self, file: &str) -> String {
        match (self.prefix.is_empty(), file.is_empty()) {
            (true, _) => String::from(file),
            (false, true) => String::from(self.prefix),
            (false, false) => format!("{}/{}", self.prefix, file),
        }
    }

    /// The volume its files are on, as `toyos_abi::syscall::Stat::volume`
    /// says: its filesystem's index plus one, so 0 stays "no volume" and every
    /// kernel mount is below a file server's bit 63.
    fn volume(&self) -> u64 {
        self.fs as u64 + 1
    }
}

/// The volume `/` and every [`ROOT_ENTRIES`] name nothing is mounted at are
/// on: the directories the VFS makes itself, below a file server's bit 63 and
/// above every [`MountPoint::volume`].
const NAMESPACE_VOLUME: u64 = (1 << 63) - 1;

/// The node of `/` on [`NAMESPACE_VOLUME`].
const ROOT_NODE: u64 = 1;

/// The node on [`NAMESPACE_VOLUME`] of the [`ROOT_ENTRIES`] name at `slot`.
fn namespace_node(slot: usize) -> u64 {
    ROOT_NODE + 1 + slot as u64
}

/// A path with every symlink followed, minted only by [`Vfs::resolve_for_open`]:
/// the mount an `OpenTarget` names is the mount opened, never one a link aimed away from.
pub struct OpenTarget(String);

impl OpenTarget {
    pub fn as_str(&self) -> &str { &self.0 }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResolveIntent {
    KernelOrRead,
    UserModify,
}

/// Virtual filesystem that dispatches to the mount points `/` synthesizes.
pub struct Vfs {
    /// Indexed by position in [`ROOT_ENTRIES`], so a name outside that set has
    /// no slot to be mounted in.
    at: [Option<MountPoint>; ROOT_ENTRIES.len()],
    /// Each mounted filesystem once, however many names reach it.
    mounts: Vec<Mount>,
    /// What `mkdir` left on a mount that keeps no directories, each charged
    /// to whoever made it.
    created_dirs: BTreeMap<String, Charge>,
    /// Every mounted directory's node, by absolute path.
    dirs: Directories<Charge>,
}

/// The most entries one `FileSystem::list` may materialise for one directory.
pub const MAX_LIST_ENTRIES: usize = 16_384;

/// Whether `name` is the directory `dir` or lies beneath it; `dir` empty is the mount root.
pub fn under_directory(name: &str, dir: &str) -> bool {
    dir.is_empty()
        || name == dir
        || (name.starts_with(dir) && name.as_bytes().get(dir.len()) == Some(&b'/'))
}

/// The heap a listing's entry of `name` takes, held while the call that
/// listed it runs: its bytes, and its place in a list that doubles.
pub fn listed_heap<T>(name: &str) -> usize {
    charge::array::<u8>(name.len()) + 2 * core::mem::size_of::<T>()
}

/// The heap a listing's set of the directories it has seen takes for one.
fn seen_heap(name: &str) -> usize {
    charge::array::<u8>(name.len()) + charge::map_entry::<String, ()>()
}

/// The charge of a directory a map of `V` beside its path holds.
fn dir_charge<V>(path: &str) -> Result<Charge, SyscallError> {
    Ok(Charge::current(charge::map_entry::<String, V>() + charge::array::<u8>(path.len()))?)
}

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
            at: [const { None }; ROOT_ENTRIES.len()],
            mounts: Vec::new(),
            created_dirs: BTreeMap::new(),
            dirs: Directories::new(),
        }
    }

    /// Mount one filesystem under `names`, each a [`ROOT_ENTRIES`] name. Several
    /// names put each under its own directory of it.
    pub fn mount(&mut self, names: &[&'static str], fs: Box<dyn FileSystem>) {
        let index = self.mounts.len();
        self.mounts.push(Mount { fs });
        for name in names {
            let prefix = if names.len() == 1 { "" } else { *name };
            let slot = Self::entry(name).expect("a mount name is one of the root's entries");
            self.at[slot] = Some(MountPoint { fs: index, prefix });
        }
    }

    fn entry(name: &str) -> Option<usize> {
        ROOT_ENTRIES.iter().position(|e| *e == name)
    }

    /// What is mounted at `name`; `None` for a bare mount point and a name `/` has not.
    fn point(&self, name: &str) -> Option<MountPoint> {
        self.at[Self::entry(name)?]
    }

    /// May a syscall acting for userland change what is at `path`? `/` itself,
    /// and a bare mount point, answer no: the read-only refusal, not a missing name.
    pub fn user_may_modify(&self, path: &str) -> bool {
        let (mount, _) = self.resolve_path("/", path);
        self.point(&mount).is_some()
    }

    fn resolve_fs(&mut self, mount: &str, file: &str) -> Option<(&mut dyn FileSystem, String)> {
        let point = self.point(mount)?;
        Some((self.mounts[point.fs].fs.as_mut(), point.path(file)))
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

        // A mount point exists whether or not anything is mounted on it; and
        // under one this kernel mounts nothing at, the directory is a file
        // server's, which the caller's client asked and this kernel cannot.
        if Self::entry(&mount).is_some() && self.point(&mount).is_none() {
            return Ok(abs);
        }

        if self.is_mounted_dir(&mount, &subdir)? { Ok(abs) } else { Err(SyscallError::NotFound) }
    }

    /// Whether `/mount/subdir` is a directory of what is mounted at `mount`:
    /// its root, one `mkdir` left in `created_dirs`, or one the filesystem holds.
    fn is_mounted_dir(&mut self, mount: &str, subdir: &str) -> Result<bool, SyscallError> {
        let Some(point) = self.point(mount) else { return Ok(false) };
        Ok(subdir.is_empty()
            || self.created_dirs.contains_key(&directory(mount, subdir))
            || self.mounts[point.fs].fs.is_dir(&point.path(subdir))?)
    }

    /// The node of the mounted directory at the absolute `path`
    /// ([`Directories`]), which the caller found is one; refused past
    /// [`Directories`]'s bounds or where the caller cannot be charged for it.
    fn dir_node(&mut self, path: &str) -> Result<u64, SyscallError> {
        self.dirs.of(path, file_cache::draw_node, dir_charge::<(u64, Charge)>)
    }

    /// A name at the absolute `path` was removed or moved away: every
    /// directory that ended with it loses its node. A mount that cannot answer
    /// keeps the node, since forgetting a directory that stands renumbers it.
    fn forget_ended_dirs(&mut self, path: &str) {
        let mut dirs = core::mem::take(&mut self.dirs);
        dirs.removed(path, |dir| {
            let (mount, subdir) = self.resolve_path("/", dir);
            self.is_mounted_dir(&mount, &subdir).unwrap_or(true)
        });
        self.dirs = dirs;
    }

    /// Each entry of the directory, a directory's name ending in `/`, and its
    /// node, as [`Self::stat`] answers it.
    ///
    /// `list` refuses above [`MAX_LIST_ENTRIES`] rather than truncating because a short listing is a confidently wrong answer to a caller checking existence or deleting a tree.
    /// What the listing takes of the heap grows `charge`, which the caller
    /// holds until it has let the listing go: refused, `ResourceExhausted`.
    pub fn list(&mut self, cwd: &str, path: &str, charge: &mut Charge) -> Result<Vec<(String, u64)>, SyscallError> {
        let (mount, subdir) = if path.is_empty() {
            self.resolve_path(cwd, "")
        } else {
            self.resolve_path(cwd, path)
        };

        if mount.is_empty() {
            let mut entries = Vec::with_capacity(ROOT_ENTRIES.len());
            for (slot, name) in ROOT_ENTRIES.iter().enumerate() {
                let node = match self.point(name) {
                    Some(_) => self.dir_node(&format!("/{name}"))?,
                    None => namespace_node(slot),
                };
                entries.push((format!("{name}/"), node));
            }
            return Ok(entries);
        }

        let Some(point) = self.point(&mount) else {
            // A mount point with nothing mounted on it is an empty directory;
            // anything under it, and any other name, is not there at all.
            return match Self::entry(&mount) {
                Some(_) if subdir.is_empty() => Ok(Vec::new()),
                _ => Err(SyscallError::NotFound),
            };
        };
        let fs_path = point.path(&subdir);
        let all_files = self.mounts[point.fs].fs.list(&fs_path, MAX_LIST_ENTRIES, charge)?;
        let listed = directory(&mount, &subdir);

        let prefix = if fs_path.is_empty() {
            String::new()
        } else {
            format!("{}/", fs_path)
        };

        fn under_prefix<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
            if prefix.is_empty() { Some(name) } else { name.strip_prefix(prefix) }
        }

        // Dedup only removes entries, so `matching` is a true upper bound on the final result count.
        let matching = all_files.iter().filter(|n| under_prefix(n, &prefix).is_some()).count();
        let mut result = Vec::with_capacity(matching);
        let mut seen_dirs = BTreeSet::new();
        let mut saw_self = false;

        for name in &all_files {
            let Some(rest) = under_prefix(name, &prefix) else { continue };

            // The listed directory's own entry: proof it exists, not a child.
            if rest.is_empty() {
                saw_self = true;
                continue;
            }
            if let Some(slash_pos) = rest.find('/') {
                let child = &rest[..slash_pos];
                let dir_name = format!("{child}/");
                if !seen_dirs.contains(&dir_name) {
                    charge.grow(listed_heap::<(String, u64)>(&dir_name) + seen_heap(&dir_name))?;
                    seen_dirs.insert(dir_name.clone());
                    let node = self.dir_node(&format!("{listed}/{child}"))?;
                    result.push((dir_name, node));
                }
            } else {
                charge.grow(listed_heap::<(String, u64)>(rest))?;
                result.push((String::from(rest), self.mounts[point.fs].fs.node(name)?));
            }
        }

        // A directory the VFS carries is its parent's entry whether or not a
        // file is under it yet.
        let parent = format!("{listed}/");
        for dir in self.created_dirs.range(parent.clone()..).map(|(dir, _)| dir) {
            let Some(rest) = dir.strip_prefix(parent.as_str()) else { break };
            let child = rest.split('/').next().unwrap_or(rest);
            if !child.is_empty() {
                let dir_name = format!("{child}/");
                if !seen_dirs.contains(&dir_name) {
                    if result.len() == MAX_LIST_ENTRIES {
                        return Err(SyscallError::ResourceExhausted);
                    }
                    charge.grow(listed_heap::<(String, u64)>(&dir_name) + seen_heap(&dir_name))?;
                    seen_dirs.insert(dir_name.clone());
                    result.push((dir_name, self.dirs.of(&format!("{parent}{child}"), file_cache::draw_node, dir_charge::<(u64, Charge)>)?));
                }
            }
        }

        // An empty directory's witnesses: its own listing entry on a mount
        // that represents directories, `created_dirs` on one the VFS carries.
        if result.is_empty()
            && !saw_self
            && !subdir.is_empty()
            && !self.created_dirs.contains_key(&directory(&mount, &subdir))
        {
            return Err(SyscallError::NotFound);
        }
        Ok(result)
    }

    pub fn resolve_for_open(&mut self, path: &str, intent: ResolveIntent) -> Result<OpenTarget, SyscallError> {
        // Before the resolution as well as after: a name at `/` resolves to
        // nothing, and "no such file" is the wrong answer to "may I make one".
        if intent == ResolveIntent::UserModify && !self.user_may_modify(path) {
            return Err(SyscallError::PermissionDenied);
        }
        let (mount, file) = self.followed(path)?;
        // A mount point is a directory, never a file to open.
        if file.is_empty() || self.point(&mount).is_none() {
            return Err(SyscallError::NotFound);
        }
        let target = OpenTarget(format!("/{mount}/{file}"));
        if intent == ResolveIntent::UserModify && !self.user_may_modify(target.as_str()) {
            return Err(SyscallError::PermissionDenied);
        }
        Ok(target)
    }

    /// The mount and the path under it that the absolute `path` names once
    /// every symlink it ends in is followed, [`MAX_LINKS`] at most.
    fn followed(&mut self, path: &str) -> Result<(String, String), SyscallError> {
        let (mut mount, mut file) = self.resolve_path("/", path);
        for _ in 0..=MAX_LINKS {
            let link = match self.resolve_fs(&mount, &file) {
                Some((fs, fs_path)) if !file.is_empty() => fs.read_link(&fs_path)?,
                _ => None,
            };
            let Some(target) = link else { return Ok((mount, file)) };
            // An absolute target names the whole hierarchy; a relative one is
            // read against the mount the link is on.
            let next = if target.starts_with('/') { target } else { format!("/{mount}/{target}") };
            (mount, file) = self.resolve_path("/", &next);
        }
        Err(SyscallError::InvalidArgument)
    }

    /// What the absolute `path` names, a final symlink followed when `follow`
    /// says so, as `toyos_abi::syscall::Stat` describes it.
    pub fn stat(&mut self, path: &str, follow: bool) -> Result<Stat, SyscallError> {
        let (mount, file) = if follow { self.followed(path)? } else { self.resolve_path("/", path) };
        let named = |file_type: FileType, size, mtime, volume, node| {
            Stat { file_type: file_type as u64, size, mtime, volume, node }
        };
        if mount.is_empty() {
            return Ok(named(FileType::Directory, 0, 0, NAMESPACE_VOLUME, ROOT_NODE));
        }
        let Some(point) = self.point(&mount) else {
            return match Self::entry(&mount) {
                Some(slot) if file.is_empty() => {
                    Ok(named(FileType::Directory, 0, 0, NAMESPACE_VOLUME, namespace_node(slot)))
                }
                _ => Err(SyscallError::NotFound),
            };
        };
        let volume = point.volume();
        if !file.is_empty() {
            let fs_path = point.path(&file);
            let fs = &mut self.mounts[point.fs].fs;
            if let Some(target) = fs.read_link(&fs_path)? {
                return Ok(named(FileType::Symlink, target.len() as u64, 0, volume, fs.node(&fs_path)?));
            }
            match fs.file_mtime(&fs_path) {
                Ok(mtime) => {
                    // A file's node is its `FileId`.
                    let node = fs.node(&fs_path)?;
                    return Ok(named(FileType::File, file_cache::size(node), mtime, volume, node));
                }
                Err(SyscallError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        if !self.is_mounted_dir(&mount, &file)? {
            return Err(SyscallError::NotFound);
        }
        Ok(named(FileType::Directory, 0, 0, volume, self.dir_node(&directory(&mount, &file))?))
    }

    /// `target`'s filesystem, its path there, and its volume ([`MountPoint::volume`]).
    fn fs_for_target(&mut self, target: &OpenTarget) -> Result<(&mut dyn FileSystem, String, u64), SyscallError> {
        let (mount, file) = self.resolve_path("/", target.as_str());
        if file.is_empty() { return Err(SyscallError::NotFound); }
        let point = self.point(&mount).ok_or(SyscallError::NotFound)?;
        Ok((self.mounts[point.fs].fs.as_mut(), point.path(&file), point.volume()))
    }

    /// The file `target` names, and the volume it is on.
    pub fn open_target(&mut self, target: &OpenTarget) -> Result<(FileId, u64), SyscallError> {
        let (fs, fs_path, volume) = self.fs_for_target(target)?;
        let (file_id, backing) = fs.open_file(&fs_path)?;
        if let Some(backing) = backing {
            crate::file_cache::set_backing(file_id, backing);
        }
        Ok((file_id, volume))
    }

    pub fn mtime_target(&mut self, target: &OpenTarget) -> Result<u64, SyscallError> {
        let (fs, fs_path, _) = self.fs_for_target(target)?;
        fs.file_mtime(&fs_path)
    }

    /// Create a new empty file: it, and the volume it is on.
    pub fn create_file(&mut self, path: &str, mtime: u64) -> Result<(FileId, u64), SyscallError> {
        let (mount, file) = self.resolve_path("/", path);
        if file.is_empty() { return Err(SyscallError::InvalidArgument); }
        let point = self.point(&mount).ok_or(SyscallError::NotFound)?;
        let file_id = self.mounts[point.fs].fs.create(&point.path(&file), mtime)?;
        Ok((file_id, point.volume()))
    }

    /// Unlink `path` on its mount, every directory keeping its node: for a
    /// caller that puts a file back at once (`ops::open`'s truncate).
    pub fn delete_file(&mut self, path: &str) -> Result<(), SyscallError> {
        let (mount, file) = self.resolve_path("/", path);
        if file.is_empty() { return Err(SyscallError::InvalidArgument); }
        let (fs, fs_path) = self.resolve_fs(&mount, &file).ok_or(SyscallError::NotFound)?;
        fs.delete(&fs_path)
    }

    pub fn rename(&mut self, old_path: &str, new_path: &str) -> Result<(), SyscallError> {
        let (old_mount, old_file) = self.resolve_path("/", old_path);
        let (new_mount, new_file) = self.resolve_path("/", new_path);
        if old_file.is_empty() || new_file.is_empty() { return Err(SyscallError::InvalidArgument); }
        if old_mount != new_mount { return Err(SyscallError::NotSupported); }
        let point = self.point(&old_mount).ok_or(SyscallError::NotFound)?;
        let (old_fs_path, new_fs_path) = (point.path(&old_file), point.path(&new_file));
        self.mounts[point.fs].fs.rename(&old_fs_path, &new_fs_path)?;
        self.forget_ended_dirs(&directory(&old_mount, &old_file));
        Ok(())
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
        // A new key is charged to its maker; a repeat of one already held costs nothing.
        if !self.created_dirs.contains_key(path) {
            let charge = dir_charge::<Charge>(path)?;
            self.created_dirs.insert(String::from(path), charge);
        }
        Ok(())
    }

    /// Remove an empty directory, reporting the real outcome — the `Result` is
    /// the point, as in [`Self::create_dir`]. A mount root, a missing name, a
    /// file, and a non-empty directory are each refused, not reported as removed.
    pub fn remove_dir(&mut self, path: &str) -> Result<(), SyscallError> {
        self.remove_empty_dir(path)?;
        self.forget_ended_dirs(&normalize(path));
        Ok(())
    }

    fn remove_empty_dir(&mut self, path: &str) -> Result<(), SyscallError> {
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
        let mut listing = Charge::none(&charge::current());
        let names = fs.list(&fs_path, MAX_LIST_ENTRIES, &mut listing)?;
        let child_prefix = format!("{fs_path}/");
        let is_file = names.contains(&fs_path);
        // A listing mount's own `name/` self-entry is not a child, or every empty directory reads non-empty.
        let has_file_child =
            names.iter().any(|n| n.starts_with(&child_prefix) && *n != child_prefix);

        // The name resolves to a file, not a directory.
        if is_file {
            return Err(SyscallError::InvalidArgument);
        }

        let created_prefix = format!("{dir}/");
        // The first carried directory at or after `dir/` in key order is the
        // only one that can lie beneath it.
        let has_created_child = self
            .created_dirs
            .range::<str, _>((core::ops::Bound::Included(created_prefix.as_str()), core::ops::Bound::Unbounded))
            .next()
            .is_some_and(|(d, _)| d.starts_with(&created_prefix));
        if !(self.created_dirs.contains_key(&dir) || has_file_child || has_created_child) {
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
        if file.is_empty() {
            return Err(SyscallError::InvalidArgument);
        }
        let (fs, fs_path) = self.resolve_fs(&mount, &file).ok_or(SyscallError::NotFound)?;
        fs.create_symlink(&fs_path, target)
    }

    pub fn read_link(&mut self, path: &str) -> Result<Option<String>, SyscallError> {
        let (mount, file) = self.resolve_path("/", path);
        if file.is_empty() {
            return Ok(None);
        }
        let Some((fs, fs_path)) = self.resolve_fs(&mount, &file) else { return Ok(None) };
        fs.read_link(&fs_path)
    }

    /// Unlink `path`, and forget the node of every directory that ended with it.
    pub fn delete(&mut self, path: &str) -> Result<(), SyscallError> {
        self.delete_file(path)?;
        self.forget_ended_dirs(&normalize(path));
        Ok(())
    }

    pub fn open_backing(&mut self, path: &str) -> Result<alloc::sync::Arc<dyn crate::file_backing::FileBacking>, SyscallError> {
        let target = self.resolve_for_open(path, ResolveIntent::KernelOrRead)?;
        let (fs, fs_path, _) = self.fs_for_target(&target)?;
        fs.open_backing(&fs_path)
    }
}
