//! `crate::files`' state and its C interface, in the copy that owns the
//! descriptor table.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use toyos_abi::syscall::{self, FileType, SyscallError};

use super::namespace::Routed;
use super::{encode, At, Error, Open, Stat, MAX_LINKS, SEEK_CURRENT, SEEK_END, SEEK_START};
use crate::fd::{self, Fd, Object, SERVED_FILE};
use crate::fs::{self, Base, Change, Dir, Fid, Refused};
use crate::sync::{Lock, Turnstile};

/// One directory capability this process holds, connected: threads sharing
/// it are served in turn.
struct Capability {
    dir: Turnstile<Dir>,
}

/// Every capability of the view this process has connected: bounded by the
/// view, as only a held directory is asked for.
static CAPABILITIES: Lock<BTreeMap<String, Arc<Capability>>> = Lock::new(BTreeMap::new());

/// A path's capability, starting directory and relative name.
enum Place { Served(Arc<Capability>, Base, String), Directory(String) }

/// A file or directory held open by its server, with a paged holder where supplied.
struct Served {
    cap: Arc<Capability>,
    fid: Fid,
    position: Position,
    holder: Option<crate::paged::Paged>,
}

enum Held {
    Served(Served),
    Directory(String),
}

impl Drop for Served {
    fn drop(&mut self) {
        self.cap.dir.lock().close(self.fid);
    }
}

/// Namespace inspection is bounded and immutable; no service is opened to
/// discover its names. Failed inspection is kept as an error, not an empty view.
static DIRECTORIES: crate::endow::Once<Result<super::namespace::Directories, Error>> = crate::endow::Once::new();

fn directories() -> Result<&'static super::namespace::Directories, Error> {
    DIRECTORIES.get_or_init(|| {
        let names = match crate::endow::namespace() {
            Some(namespace) => namespace.names().map_err(Error::Kernel)?,
            None => Vec::new(),
        };
        Ok(super::namespace::Directories::new(names))
    }).as_ref().map_err(|e| *e)
}

fn directory_stat(path: &str) -> Result<Stat, Error> {
    Ok(Stat {
        file_type: FileType::Directory, size: 0, nlink: 1,
        atime: 0, mtime: 0, ctime: 0, volume: 0,
        node: directories()?.node(path).expect("a routed namespace directory"),
    })
}

fn read_only<T>(_: &str) -> Result<T, Error> { Err(Error::Served(fs::Error::ReadOnly)) }
fn is_directory<T>(_: &str) -> Result<T, Error> { Err(Error::Served(fs::Error::IsADirectory)) }

/// A served file's offset: kept here while this process is the
/// description's only holder, so a seek costs no request and a read or a
/// write goes at it as a positioned one; the server's from the moment the
/// description is shared with another process, which is for good, and a
/// directory's from the start, as the server refuses to move one.
///
/// [`Position::export`] and [`Position::stream`] share descriptions, so each
/// hands the offset over before another process can use it.
///
/// Reached only under the capability's turn, so a read or write and the
/// offset's move are one step against every other thread's.
struct Position {
    /// Every write lands at the end, which only the server knows.
    append: bool,
    at: Lock<Offset>,
}

enum Offset {
    Here(u64),
    Server,
}

impl Position {
    fn new(append: bool, directory: bool) -> Self {
        let at = if directory { Offset::Server } else { Offset::Here(0) };
        Self { append, at: Lock::new(at) }
    }

    fn read(&self, dir: &mut Dir, fid: Fid, out: &mut [u8]) -> Result<usize, fs::Error> {
        let mut at = self.at.lock();
        match *at {
            Offset::Here(offset) => {
                let n = dir.pread(fid, offset, out)?;
                *at = Offset::Here(offset + n as u64);
                Ok(n)
            }
            Offset::Server => dir.read(fid, out),
        }
    }

    /// A read at the local offset through the file's paged object, which
    /// takes no turn on the capability: `None` where the offset is the
    /// server's or the object would not answer, and the server is asked.
    fn read_held(&self, holder: &crate::paged::Paged, out: &mut [u8]) -> Option<usize> {
        let mut at = self.at.lock();
        let Offset::Here(offset) = *at else { return None };
        let n = holder.read(out, offset).ok()?;
        *at = Offset::Here(offset + n as u64);
        Some(n)
    }

    fn write(&self, dir: &mut Dir, fid: Fid, data: &[u8]) -> Result<usize, fs::Error> {
        let mut at = self.at.lock();
        match *at {
            Offset::Here(offset) if !self.append => {
                let n = dir.pwrite(fid, offset, data)?;
                *at = Offset::Here(offset + n as u64);
                Ok(n)
            }
            // The server writes at the end and answers where that left it.
            Offset::Here(_) => dir.write(fid, data).map(|w| {
                *at = Offset::Here(w.offset);
                w.len
            }),
            Offset::Server => dir.write(fid, data).map(|w| w.len),
        }
    }

    /// Where a seek lands, by the rule the server keeps for its own offsets:
    /// at or below [`fs::MAX_FILE_BYTES`] and not before the start. From the
    /// end, the server is asked, since only it knows the size.
    fn seek(&self, dir: &mut Dir, fid: Fid, whence: u64, delta: i64) -> Result<u64, fs::Error> {
        let mut at = self.at.lock();
        let Offset::Here(offset) = *at else { return dir.seek(fid, whence, delta) };
        let landed = match whence {
            fs::SEEK_START => 0u64.checked_add_signed(delta),
            fs::SEEK_CURRENT => offset.checked_add_signed(delta),
            _ => Some(dir.seek(fid, whence, delta)?),
        };
        let landed = landed.filter(|&l| l <= fs::MAX_FILE_BYTES).ok_or(fs::Error::InvalidRequest)?;
        *at = Offset::Here(landed);
        Ok(landed)
    }

    /// Share this description with another process. Once handed over, every
    /// read, write and seek uses the server's one offset.
    fn export(&self, dir: &mut Dir, fid: Fid) -> Result<crate::ipc::Connection, fs::Error> {
        let mut at = self.at.lock();
        if let Offset::Here(offset) = *at {
            dir.seek(fid, fs::SEEK_START, offset as i64)?;
            *at = Offset::Server;
        }
        dir.export(&[fid])
    }

    /// A stream the server writes at the description's offset for another
    /// process ([`Dir::stream`]): the offset is handed to the server first,
    /// and is its from then on.
    fn stream(&self, dir: &mut Dir, fid: Fid) -> Result<crate::Pipe, fs::Error> {
        let mut at = self.at.lock();
        if let Offset::Here(offset) = *at {
            dir.seek(fid, fs::SEEK_START, offset as i64)?;
            *at = Offset::Server;
        }
        dir.stream(fid)
    }
}

/// What descriptors name: a held object's word is its index here, and the
/// slot is emptied when the table lets the object go.
static OPEN: Lock<Vec<Option<Arc<Held>>>> = Lock::new(Vec::new());

/// A descriptor's object let go: what it held goes with it, closed outside
/// the lock, as a close asks the server.
extern "C" fn release_held(word: u64) {
    let held = usize::try_from(word).ok().and_then(|i| OPEN.lock().get_mut(i).and_then(Option::take));
    assert!(held.is_some(), "toyos::files: a released object {word} that was not held");
    drop(held);
}

/// What `word` names, while a descriptor names it.
fn held(word: u64) -> Option<Arc<Held>> {
    OPEN.lock().get(usize::try_from(word).ok()?)?.clone()
}

/// Process-owned working directory, shared by every SDK copy through this owner,
/// starting where the spawn's environment says ([`super::starting_directory`]).
static CWD: Lock<Option<String>> = Lock::new(None);

fn cwd() -> String {
    let mut cwd = CWD.lock();
    cwd.get_or_insert_with(|| {
        super::starting_directory(crate::startup::env()).expect("the spawn stated a starting directory that is not absolute UTF-8")
    }).clone()
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_getcwd(out: *mut u8, len: usize) -> usize {
    let cwd = cwd();
    if cwd.len() <= len {
        // SAFETY: the caller lends `len` writable bytes for the call.
        unsafe { core::ptr::copy_nonoverlapping(cwd.as_ptr(), out, cwd.len()) };
    }
    cwd.len()
}

fn utf8(path: &[u8]) -> Result<&str, Error> {
    core::str::from_utf8(path).map_err(|_| Error::NotUtf8)
}

/// The held capability named `fs:<prefix>`, connected once and kept; a
/// refused connect is the caller's error. The connect waits for the server's
/// hello, so it is made outside the lock: of two threads that connect at once
/// the first connection kept is the one used.
fn capability(prefix: &str) -> Result<Arc<Capability>, Error> {
    if let Some(found) = CAPABILITIES.lock().get(prefix) {
        return Ok(Arc::clone(found));
    }
    let names = crate::endow::namespace().ok_or(Error::Served(fs::Error::NotHeld))?;
    let name = alloc::format!("{}{prefix}", fs::CAPABILITY_PREFIX);
    let dir = Dir::connect(names, &name).map_err(Error::Served)?;
    let found = Arc::new(Capability { dir: Turnstile::new(dir) });
    Ok(Arc::clone(CAPABILITIES.lock().entry(String::from(prefix)).or_insert(found)))
}

/// Which server the absolute, lexically clean `abs` is on, and the path there.
fn route(abs: &str) -> Result<Place, Error> {
    Ok(match super::namespace::route(abs, directories()?, capability)? {
        Routed::Served(cap, rel) => Place::Served(cap, Base::Root, rel),
        Routed::Directory => Place::Directory(String::from(abs)),
    })
}

/// Where `path` from `at` is resolved: the empty path names nothing
/// ([`super::named`]). A descriptor that names no directory is
/// [`Error::NotADirectory`] for any path but an absolute one.
fn place(at: At, path: &[u8]) -> Result<Place, Error> {
    let path = super::named(path)?;
    match at {
        _ if path.starts_with('/') => route(&super::absolute("/", path)),
        At::Cwd => route(&super::absolute(&cwd(), path)),
        At::Fd(fd) => from_dir(fd, path),
    }
}

/// `path` from the directory `fd` names; the empty path is that directory.
fn from_dir(fd: Fd, path: &str) -> Result<Place, Error> {
    let named = fd::with(fd, |o| match o.tag() {
        SERVED_FILE if o.kernel_handle().is_none() => held(o.word()),
        _ => None,
    });
    match named.map_err(Error::Fd)?.as_deref() {
        Some(Held::Served(Served { cap, fid, .. })) => {
            let rel = fs::wire_path(path).ok_or(Error::Served(fs::Error::NameTooLong))?;
            Ok(Place::Served(Arc::clone(cap), Base::Fid(*fid), rel))
        }
        Some(Held::Directory(dir)) => route(&super::absolute(dir, path)),
        None => Err(Error::NotADirectory),
    }
}

/// Ask whoever serves `path` from `at`, following an absolute link a server
/// meets to wherever it lands in this process's own table. `served` runs on
/// the capability's turn, so nothing it does may reach the descriptor table,
/// whose release of an open file takes that turn again.
fn on_path<T>(
    at: At,
    path: &[u8],
    mut synthetic: impl FnMut(&str) -> Result<T, Error>,
    mut served: impl FnMut(&Arc<Capability>, &mut Dir, Base, &str) -> Result<T, Refused>,
) -> Result<T, Error> {
    let mut place = place(at, path)?;
    for _ in 0..MAX_LINKS {
        let (cap, base, rel) = match place {
            Place::Served(cap, base, rel) => (cap, base, rel),
            Place::Directory(path) => return synthetic(&path),
        };
        let mut dir = cap.dir.lock();
        match served(&cap, &mut dir, base, &rel) {
            Ok(answer) => return Ok(answer),
            Err(Refused::Error(e)) => return Err(Error::Served(e)),
            Err(Refused::Follow(len)) => {
                let mut buf = [0u8; fs::MAX_PATH];
                let target = dir.follow_target(len, &mut buf);
                let target = super::absolute("/", utf8(target)?);
                drop(dir);
                place = route(&target)?;
            }
        }
    }
    Err(Error::Loop)
}

/// `kernel` on `fd`'s handle, held for the call, or `served` on what a server
/// holds open for it.
fn on_fd<T>(
    fd: Fd,
    kernel: impl FnOnce(crate::BorrowedHandle<'_>) -> Result<T, SyscallError>,
    synthetic: impl FnOnce(&str) -> Result<T, Error>,
    served: impl FnOnce(&mut Dir, Fid, &Position) -> Result<T, fs::Error>,
) -> Result<T, Error> {
    on_fd_held(fd, kernel, synthetic, |_, _| None, served)
}

/// [`on_fd`], trying `holding` first on a served file whose paged object
/// this descriptor holds, without the capability's turn, so a reader waiting
/// for a page holds up no request of another thread's; `None` from it, and
/// the server is asked.
fn on_fd_held<T>(
    fd: Fd,
    kernel: impl FnOnce(crate::BorrowedHandle<'_>) -> Result<T, SyscallError>,
    synthetic: impl FnOnce(&str) -> Result<T, Error>,
    holding: impl FnOnce(&crate::paged::Paged, &Position) -> Option<T>,
    served: impl FnOnce(&mut Dir, Fid, &Position) -> Result<T, fs::Error>,
) -> Result<T, Error> {
    // A kernel call is made while the descriptor's object is held; an object
    // of this module's is taken out of it, counted, and asked after.
    let named = fd::with(fd, |o| match (o.kernel_handle(), o.tag()) {
        (Some(handle), _) => Ok(Err(kernel(handle).map_err(Error::Kernel))),
        (None, SERVED_FILE) => {
            Ok(Ok(held(o.word()).expect("toyos::files: a held descriptor names an object of this module's")))
        }
        (None, _) => Err(Error::NotAFile),
    });
    match named.map_err(Error::Fd)?? {
        Err(answer) => answer,
        Ok(held) => match &*held {
            Held::Directory(path) => synthetic(path),
            Held::Served(held) => match held.holder.as_ref().and_then(|h| holding(h, &held.position)) {
                Some(answer) => Ok(answer),
                None => served(&mut held.cap.dir.lock(), held.fid, &held.position).map_err(Error::Served),
            },
        },
    }
}

/// A descriptor for `held`; on a full table it is let go again, which takes
/// its capability's turn, so no turn may be held here.
fn install(held: Held, cloexec: bool) -> Result<Fd, fd::Error> {
    fd::install(object(held), !cloexec, 0)
}

fn object(held: Held) -> Object {
    let held = Arc::new(held);
    let word = {
        let mut open = OPEN.lock();
        match open.iter().position(Option::is_none) {
            Some(free) => {
                open[free] = Some(held);
                free
            }
            None => {
                open.push(Some(held));
                open.len() - 1
            }
        }
    };
    Object::tagged(SERVED_FILE, word as u64, release_held)
}

/// Take up the one description exported for an endowed file descriptor.
/// This constructs an object without entering the descriptor table again.
pub(super) fn adopt_exported(handle: crate::OwnedHandle) -> Result<Object, Error> {
    let mut dir = Dir::adopt(crate::ipc::Connection(handle)).map_err(Error::Served)?;
    let fid = Fid { id: 1, generation: dir.generation() };
    dir.fstat(fid).map_err(Error::Served)?;
    Ok(object(Held::Served(Served {
        cap: Arc::new(Capability { dir: Turnstile::new(dir) }),
        fid,
        position: Position { append: false, at: Lock::new(Offset::Server) },
        holder: None,
    })))
}

#[no_mangle]
extern "C" fn toyos_fs_export(word: u64) -> i64 {
    let held = held(word).expect("a held descriptor names this file object");
    let exported = match &*held {
        Held::Served(file) => file.position.export(&mut file.cap.dir.lock(), file.fid).map_err(Error::Served),
        Held::Directory(_) => Err(Error::Served(fs::Error::Unsupported)),
    };
    answer(exported.map(|conn| u64::from(conn.into_raw().0)))
}

/// What a server answered, as this module says it.
fn stat_of(stat: fs::Stat) -> Stat {
    let file_type = match stat.kind {
        fs::KIND_FILE => FileType::File,
        fs::KIND_DIR => FileType::Directory,
        fs::KIND_SYMLINK => FileType::Symlink,
        _ => FileType::Unknown,
    };
    Stat {
        file_type,
        size: stat.size,
        nlink: stat.nlink,
        atime: stat.atime,
        mtime: stat.mtime,
        ctime: stat.ctime,
        volume: stat.volume,
        node: stat.node,
    }
}

fn answer(result: Result<u64, Error>) -> i64 {
    match result {
        Ok(n) => n as i64,
        Err(e) => encode(e),
    }
}

/// `len` bytes at `ptr`, as the caller lends them for the call.
///
/// # Safety
/// `ptr` names `len` readable bytes for the call.
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    // SAFETY: the caller's.
    unsafe { core::slice::from_raw_parts(ptr, len) }
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_open(at: Fd, path: *const u8, len: usize, how: Open) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    let opened = on_path(
        At::of(at),
        path,
        |path| {
            if how.write || how.append || how.create || how.create_new || how.truncate || how.unnamed {
                return Err(Error::Served(fs::Error::ReadOnly));
            }
            Ok(Held::Directory(String::from(path)))
        },
        |cap, dir, base, rel| {
            let mut flags = 0;
            if how.read { flags |= fs::O_READ; }
            if how.write { flags |= fs::O_WRITE; }
            if how.append { flags |= fs::O_APPEND; }
            if how.create { flags |= fs::O_CREATE; }
            if how.create_new { flags |= fs::O_CREATE_NEW; }
            if how.truncate { flags |= fs::O_TRUNCATE; }
            if how.directory { flags |= fs::O_DIRECTORY; }
            let opened = match how.unnamed {
                true => dir.create_unnamed(base, rel, flags)?,
                false => dir.open(base, rel, flags)?,
            };
            let position = Position::new(how.append, opened.stat.kind == fs::KIND_DIR);
            // What a reply carries is a holder only as the kernel answers for
            // it; a pager's process reads through the server.
            let holder = match opened.holder.map(crate::paged::Paged::received) {
                None | Some(Err(crate::paged::NotAHolder::Refused | crate::paged::NotAHolder::Ended)) => None,
                Some(Ok(paged)) => Some(paged),
                Some(Err(crate::paged::NotAHolder::Other)) => {
                    dir.close(opened.fid);
                    return Err(Refused::Error(fs::Error::Malformed));
                }
            };
            Ok(Held::Served(Served { cap: Arc::clone(cap), fid: opened.fid, position, holder }))
        },
    );
    // Install outside the capability turn: refusal closes the opened fid.
    let installed = opened.and_then(|held| install(held, how.cloexec).map_err(Error::Fd));
    answer(installed.map(|fd| fd as u64))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_stat(at: Fd, path: *const u8, len: usize, follow: bool, out: *mut Stat) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    let stat = match (At::of(at), path.is_empty()) {
        // What the descriptor names itself.
        (At::Fd(fd), true) => on_fd(
            fd,
            |h| syscall::fstat(h.raw()).map(Stat::of_kernel),
            directory_stat,
            |dir, fid, _| dir.fstat(fid).map(stat_of),
        ),
        (at, _) => on_path(
            at,
            path,
            directory_stat,
            |_, dir, base, rel| dir.stat(base, rel, follow).map(stat_of),
        ),
    };
    answer(stat.map(|stat| {
        // SAFETY: the caller's live place.
        unsafe { out.write(stat) };
        0
    }))
}

/// `f` on the stream `fd` names, held for the call, as a read or a write of
/// its descriptor does it: through its region, since its connection carries
/// frames. `None` for a descriptor that names no stream.
fn on_stream<T>(fd: Fd, f: impl FnOnce(crate::BorrowedHandle<'_>) -> Result<T, crate::net::stream::Refused>) -> Option<Result<T, Error>> {
    let streamed = fd::with(fd, |o| {
        o.handle().filter(|_| o.tag() == fd::STREAM).map(|handle| f(handle).map_err(Error::Stream))
    });
    streamed.ok().flatten()
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_read(fd: Fd, buf: *mut u8, len: usize) -> i64 {
    // SAFETY: the caller lends `len` writable bytes for the call. The
    // closures that run, run one after another, and each slice is dead
    // before the next is made, so no two live at once.
    let out = || unsafe { core::slice::from_raw_parts_mut(buf, len) };
    let blocking = crate::net::stream::Wait::Blocking(None);
    if let Some(n) = on_stream(fd, |h| crate::net::stream::read(h, out(), blocking)) {
        return answer(n.map(|n| n as u64));
    }
    let n = on_fd_held(
        fd,
        |h| syscall::read(h.raw(), out()),
        is_directory,
        |holder, position| position.read_held(holder, out()),
        |dir, fid, position| position.read(dir, fid, out()),
    );
    answer(n.map(|n| n as u64))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_write(fd: Fd, buf: *const u8, len: usize) -> i64 {
    // SAFETY: the caller's.
    let buf = unsafe { bytes(buf, len) };
    let blocking = crate::net::stream::Wait::Blocking(None);
    if let Some(n) = on_stream(fd, |h| crate::net::stream::write(h, buf, blocking)) {
        return answer(n.map(|n| n as u64));
    }
    let n = on_fd(fd, |h| syscall::write(h.raw(), buf), is_directory, |dir, fid, position| position.write(dir, fid, buf));
    answer(n.map(|n| n as u64))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_pread(fd: Fd, buf: *mut u8, len: usize, offset: u64) -> i64 {
    // SAFETY: the caller lends `len` writable bytes for the call. The
    // closures that run, run one after another, and each slice is dead
    // before the next is made, so no two live at once.
    let out = || unsafe { core::slice::from_raw_parts_mut(buf, len) };
    let n = on_fd_held(
        fd,
        |_| Err(SyscallError::NotSupported),
        is_directory,
        |holder, _| holder.read(out(), offset).ok(),
        |dir, fid, _| dir.pread(fid, offset, out()),
    );
    answer(n.map(|n| n as u64))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_pwrite(fd: Fd, buf: *const u8, len: usize, offset: u64) -> i64 {
    // SAFETY: the caller's.
    let buf = unsafe { bytes(buf, len) };
    let n = on_fd(fd, |_| Err(SyscallError::NotSupported), is_directory, |dir, fid, _| dir.pwrite(fid, offset, buf));
    answer(n.map(|n| n as u64))
}

#[no_mangle]
extern "C" fn toyos_fs_seek(fd: Fd, whence: u32, delta: i64) -> i64 {
    let served = match whence {
        SEEK_START => fs::SEEK_START,
        SEEK_CURRENT => fs::SEEK_CURRENT,
        SEEK_END => fs::SEEK_END,
        _ => return encode(Error::Kernel(SyscallError::InvalidArgument)),
    };
    answer(on_fd(fd, |_| Err(SyscallError::NotSupported), is_directory, |dir, fid, position| position.seek(dir, fid, served, delta)))
}

#[no_mangle]
extern "C" fn toyos_fs_truncate(fd: Fd, size: u64) -> i64 {
    answer(on_fd(fd, |_| Err(SyscallError::NotSupported), is_directory, |dir, fid, _| dir.truncate(fid, size)).map(|()| 0))
}

#[no_mangle]
extern "C" fn toyos_fs_fsync(fd: Fd) -> i64 {
    answer(on_fd(fd, |_| Err(SyscallError::NotSupported), is_directory, |dir, fid, _| dir.fsync(fid)).map(|()| 0))
}

#[no_mangle]
extern "C" fn toyos_fs_stream(fd: Fd) -> i64 {
    let handle = on_fd(fd, |h| h.try_clone_to_owned().map(crate::OwnedHandle::into_raw), is_directory, |dir, fid, position| {
        position.stream(dir, fid).map(|pipe| pipe.into_raw())
    });
    answer(handle.map(|h| u64::from(h.0)))
}

#[no_mangle]
extern "C" fn toyos_fs_image(fd: Fd) -> i64 {
    let image = on_fd(fd, |_| Err(SyscallError::NotSupported), is_directory, |dir, fid, _| dir.image(fid).map(|h| h.into_raw()));
    answer(image.map(|h| u64::from(h.0)))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_mkdir(at: Fd, path: *const u8, len: usize) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    let made = on_path(At::of(at), path, read_only, |_, dir, base, rel| dir.mkdir(base, rel));
    answer(made.map(|()| 0))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_unlink(at: Fd, path: *const u8, len: usize, is_dir: bool) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    let removed = on_path(
        At::of(at),
        path,
        read_only,
        |_, dir, base, rel| dir.unlink(base, rel, is_dir),
    );
    answer(removed.map(|()| 0))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_chdir(path: *const u8, len: usize) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    let changed = super::named(path).and_then(|path| {
        let abs = super::absolute(&cwd(), path);
        if abs.len() > fs::MAX_PATH {
            return Err(Error::Served(fs::Error::NameTooLong));
        }
        let stat = on_path(At::Cwd, abs.as_bytes(), directory_stat, |_, dir, base, rel| dir.stat(base, rel, true).map(stat_of))?;
        if stat.file_type != FileType::Directory {
            return Err(Error::NotADirectory);
        }
        *CWD.lock() = Some(abs);
        Ok(())
    });
    answer(changed.map(|()| 0))
}

/// Two paths on one capability, with their starting directories.
struct Pair(Arc<Capability>, (Base, String), (Base, String));

fn pair(from: (At, &[u8]), to: (At, &[u8])) -> Result<Pair, Error> {
    pair_of(place(from.0, from.1)?, place(to.0, to.1)?)
}

fn pair_of(from: Place, to: Place) -> Result<Pair, Error> {
    match (from, to) {
        (Place::Served(ca, ba, a), Place::Served(cb, bb, b)) if Arc::ptr_eq(&ca, &cb) => Ok(Pair(ca, (ba, a), (bb, b))),
        _ => Err(Error::CrossesServers),
    }
}

/// A served answer to a call no link may redirect: a link to follow out of
/// the directory leads to another server.
fn crossing(refused: Refused) -> Error {
    match refused {
        Refused::Error(e) => Error::Served(e),
        Refused::Follow(_) => Error::CrossesServers,
    }
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_rename(from_at: Fd, from: *const u8, from_len: usize, to_at: Fd, to: *const u8, to_len: usize) -> i64 {
    // SAFETY: the caller's.
    let (from, to) = unsafe { (bytes(from, from_len), bytes(to, to_len)) };
    let renamed = pair((At::of(from_at), from), (At::of(to_at), to)).and_then(|pair| match pair {
        Pair(cap, (ba, a), (bb, b)) => cap.dir.lock().rename((ba, &a), (bb, &b)).map_err(crossing),
    });
    answer(renamed.map(|()| 0))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_link(
    from_at: Fd,
    from: *const u8,
    from_len: usize,
    follow: bool,
    to_at: Fd,
    to: *const u8,
    to_len: usize,
) -> i64 {
    // SAFETY: the caller's.
    let (from, to) = unsafe { (bytes(from, from_len), bytes(to, to_len)) };
    // An empty `from` from a descriptor is what it names (`AT_EMPTY_PATH`).
    let from_place = match (At::of(from_at), from.is_empty()) {
        (At::Fd(fd), true) => from_dir(fd, ""),
        (at, _) => place(at, from),
    };
    let linked = from_place.and_then(|from| pair_of(from, place(At::of(to_at), to)?)).and_then(|pair| match pair {
        Pair(cap, (ba, a), (bb, b)) => cap.dir.lock().link((ba, &a), follow, (bb, &b), None).map_err(crossing),
    });
    answer(linked.map(|()| 0))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_symlink(target: *const u8, target_len: usize, at: Fd, link: *const u8, link_len: usize) -> i64 {
    // SAFETY: the caller's.
    let (target, link) = unsafe { (bytes(target, target_len), bytes(link, link_len)) };
    let made = utf8(target).and_then(|target| {
        on_path(At::of(at), link, read_only, |_, dir, base, rel| {
            dir.symlink(target, (base, rel))
        })
    });
    answer(made.map(|()| 0))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_readlink(at: Fd, path: *const u8, len: usize, out: *mut u8, cap: usize) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    // SAFETY: the caller lends `cap` writable bytes for the call, and each
    // closure makes the slice only for its own run.
    let target = || unsafe { core::slice::from_raw_parts_mut(out, cap) };
    let n = on_path(At::of(at), path, |_| Err(Error::Served(fs::Error::NotALink)), |_, dir, base, rel| {
        let mut buf = [0u8; fs::MAX_PATH];
        let n = dir.read_link(base, rel, &mut buf)?;
        let out = target();
        let n = n.min(out.len());
        out[..n].copy_from_slice(&buf[..n]);
        Ok(n)
    });
    answer(n.map(|n| n as u64))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_set_times(at: Fd, path: *const u8, len: usize, follow: bool, flags: u64, atime: u64, mtime: u64) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    let request = fs::Request { flags, atime, mtime, ..fs::Request::new() };
    let Some(change) = Change::decode(&request).filter(|c| c.size.is_none()) else {
        return encode(Error::Served(fs::Error::InvalidRequest));
    };
    let set = match (At::of(at), path.is_empty()) {
        (At::Fd(fd), true) => from_dir(fd, "").and_then(|place| match place {
            Place::Served(cap, base, rel) => cap.dir.lock().set_attr(base, &rel, follow, &change).map_err(crossing),
            Place::Directory(_) => Err(Error::Served(fs::Error::ReadOnly)),
        }),
        (at, _) => on_path(at, path, read_only, |_, dir, base, rel| dir.set_attr(base, rel, follow, &change)),
    };
    answer(set.map(|()| 0))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_readdir(fd: Fd, cursor: u64, out: *mut u8, cap: usize, next: *mut u64) -> i64 {
    let listing = on_fd(
        fd,
        |_| Err(SyscallError::InvalidArgument),
        |path| {
            let mut out = Vec::new();
            if cursor == 0 {
                for (name, node) in directories()?.children(path) {
                    fs::encode_entry(&mut out, &fs::Entry { kind: fs::KIND_DIR, size: 0, node, name })
                        .expect("namespace service names fit directory entries");
                }
            }
            Ok((out, None))
        },
        |dir, fid, _| {
            let mut batch = Vec::new();
            dir.read_dir(fid, cursor, cap, &mut batch).map(|next| (batch, next))
        },
    );
    answer(listing.map(|(listing, after)| {
        if listing.len() <= cap {
            // SAFETY: the caller lends `cap` writable bytes and one `u64` for
            // the call.
            unsafe {
                core::ptr::copy_nonoverlapping(listing.as_ptr(), out, listing.len());
                next.write(after.unwrap_or(fs::READDIR_END));
            }
        }
        listing.len() as u64
    }))
}
