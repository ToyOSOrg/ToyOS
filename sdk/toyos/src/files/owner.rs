//! `crate::files`' state and its C interface, in the copy that owns the
//! descriptor table.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use toyos_abi::syscall::{self, FileType, OpenFlags, SeekFrom, Stat, SyscallError};
use toyos_abi::RawHandle;

use super::{encode, Error, Open, MAX_CAPABILITY_DEPTH, MAX_LINKS, SEEK_CURRENT, SEEK_END, SEEK_START};
use crate::fd::{self, Fd, Object, SERVED_FILE};
use crate::fs::{self, Dir, Refused};
use crate::sync::{Lock, Turnstile};

/// One directory capability this process holds, connected: threads sharing
/// it are served in turn.
struct Capability {
    dir: Turnstile<Dir>,
}

/// Every capability this process has looked for, found or not.
static CAPABILITIES: Lock<BTreeMap<String, Option<Arc<Capability>>>> = Lock::new(BTreeMap::new());

/// Where a path is served.
enum Route {
    Kernel(String),
    Served(Arc<Capability>, String),
}

/// A file a file server holds open, which a descriptor names.
struct OpenFile {
    cap: Arc<Capability>,
    fid: u64,
    generation: u64,
}

impl Drop for OpenFile {
    fn drop(&mut self) {
        self.cap.dir.lock().close(self.fid, self.generation);
    }
}

/// The open files descriptors name: a served file's object word is its
/// index here, and the slot is emptied when the table lets the object go.
static OPEN: Lock<Vec<Option<Arc<OpenFile>>>> = Lock::new(Vec::new());

/// A descriptor's object let go: the open file goes with it, closed outside
/// the lock, as a close asks the server.
extern "C" fn release_file(word: u64) {
    let file = usize::try_from(word).ok().and_then(|i| OPEN.lock().get_mut(i).and_then(Option::take));
    assert!(file.is_some(), "toyos::files: a released open file {word} that was not held");
    drop(file);
}

/// The open file `word` names, while a descriptor names it.
fn open_file(word: u64) -> Option<Arc<OpenFile>> {
    OPEN.lock().get(usize::try_from(word).ok()?)?.clone()
}

/// The working directory, as the kernel keeps it.
fn cwd() -> String {
    let mut buf = alloc::vec![0u8; syscall::MAX_PATH];
    let n = syscall::getcwd(&mut buf);
    String::from_utf8_lossy(&buf[..n.min(buf.len())]).into_owned()
}

fn utf8(path: &[u8]) -> Result<&str, Error> {
    core::str::from_utf8(path).map_err(|_| Error::NotUtf8)
}

/// The capability named `fs:<prefix>`, remembering the answer either way.
/// The connect waits for the server's hello, so it is made outside the lock:
/// of two threads that connect at once the first answer kept is the one used.
fn capability(prefix: &str) -> Result<Option<Arc<Capability>>, Error> {
    if let Some(found) = CAPABILITIES.lock().get(prefix) {
        return Ok(found.clone());
    }
    let Some(names) = crate::endow::namespace() else { return Ok(None) };
    let name = alloc::format!("{}{prefix}", fs::CAPABILITY_PREFIX);
    let found = match Dir::connect(names, &name) {
        Ok(dir) => Some(Arc::new(Capability { dir: Turnstile::new(dir) })),
        Err(fs::Error::NotHeld) => None,
        Err(e) => return Err(Error::Served(e)),
    };
    Ok(CAPABILITIES.lock().entry(String::from(prefix)).or_insert(found).clone())
}

/// Which server `abs` is on, and the path there.
fn route(abs: &str) -> Result<Route, Error> {
    let parts: Vec<&str> = abs.split('/').filter(|p| !p.is_empty()).collect();
    for depth in (1..=parts.len().min(MAX_CAPABILITY_DEPTH)).rev() {
        let prefix = alloc::format!("/{}", parts[..depth].join("/"));
        if let Some(cap) = capability(&prefix)? {
            return Ok(Route::Served(cap, parts[depth..].join("/")));
        }
    }
    Ok(Route::Kernel(String::from(abs)))
}

/// Ask whoever serves `path`, following an absolute link a server meets to
/// wherever it lands in this process's own table. `served` runs on the
/// capability's turn, so nothing it does may reach the descriptor table,
/// whose release of an open file takes that turn again.
fn on_path<T>(
    path: &[u8],
    mut kernel: impl FnMut(&str) -> Result<T, SyscallError>,
    mut served: impl FnMut(&Arc<Capability>, &mut Dir, &str) -> Result<T, Refused>,
) -> Result<T, Error> {
    let mut abs = super::absolute(&cwd(), utf8(path)?);
    for _ in 0..MAX_LINKS {
        let (cap, rel) = match route(&abs)? {
            Route::Kernel(abs) => return kernel(&abs).map_err(Error::Kernel),
            Route::Served(cap, rel) => (cap, rel),
        };
        let mut dir = cap.dir.lock();
        match served(&cap, &mut dir, &rel) {
            Ok(answer) => return Ok(answer),
            Err(Refused::Error(e)) => return Err(Error::Served(e)),
            Err(Refused::Link(len)) => {
                let mut buf = [0u8; fs::MAX_PATH];
                let target = dir.link_target(len, &mut buf);
                abs = super::absolute("/", utf8(target)?);
            }
        }
    }
    Err(Error::Loop)
}

/// `kernel` on `fd`'s handle, held for the call, or `served` on its open file.
fn on_fd<T>(
    fd: Fd,
    kernel: impl FnOnce(RawHandle) -> Result<T, SyscallError>,
    served: impl FnOnce(&mut Dir, u64, u64) -> Result<T, fs::Error>,
) -> Result<T, Error> {
    // A kernel call is made while the descriptor's object is held; an open
    // file is taken out of it, counted, and asked after.
    let named = fd::with(fd, |o| match (o.kernel_handle(), o.tag) {
        (Some(handle), _) => Ok(Err(kernel(handle).map_err(Error::Kernel))),
        (None, SERVED_FILE) => {
            Ok(Ok(open_file(o.word).expect("toyos::files: a held descriptor names an open file")))
        }
        (None, _) => Err(Error::NotAFile),
    });
    match named.map_err(Error::Fd)?? {
        Err(answer) => answer,
        Ok(file) => served(&mut file.cap.dir.lock(), file.fid, file.generation).map_err(Error::Served),
    }
}

/// What an open opened, before a descriptor names it.
enum Opened {
    Kernel(RawHandle),
    Served(Arc<Capability>, fs::Opened),
}

/// A descriptor for an open file a server answered: on a full table the
/// file is closed again, which takes its capability's turn, so no turn may be
/// held here.
fn served_fd(cap: Arc<Capability>, opened: fs::Opened, cloexec: bool) -> Result<Fd, fd::Error> {
    let file = Arc::new(OpenFile { cap, fid: opened.fid, generation: opened.generation });
    let word = {
        let mut open = OPEN.lock();
        match open.iter().position(Option::is_none) {
            Some(free) => {
                open[free] = Some(file);
                free
            }
            None => {
                open.push(Some(file));
                open.len() - 1
            }
        }
    };
    let object = Object::tagged(SERVED_FILE, word as u64, release_file);
    fd::install(object, !cloexec, 0)
}

/// What a server answered, as the kernel's `Stat` says it.
fn stat_of(stat: fs::Stat) -> Stat {
    let file_type = match stat.kind {
        fs::KIND_FILE => FileType::File,
        fs::KIND_DIR => FileType::Directory,
        fs::KIND_SYMLINK => FileType::Symlink,
        _ => FileType::Unknown,
    };
    Stat { file_type, size: stat.size, mtime: stat.mtime, volume: stat.volume, node: stat.node }
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
unsafe extern "C" fn toyos_fs_open(path: *const u8, len: usize, how: Open) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    let opened = on_path(
        path,
        |abs| {
            let mut flags = OpenFlags(0);
            if how.read { flags |= OpenFlags::READ; }
            if how.write || how.append { flags |= OpenFlags::WRITE; }
            if how.append { flags |= OpenFlags::APPEND; }
            if how.create || how.create_new { flags |= OpenFlags::CREATE; }
            if how.create_new { flags |= OpenFlags::EXCLUSIVE; }
            if how.truncate { flags |= OpenFlags::TRUNCATE; }
            syscall::open(abs.as_bytes(), flags).map(Opened::Kernel)
        },
        |cap, dir, rel| {
            let mut flags = 0;
            if how.read { flags |= fs::O_READ; }
            if how.write { flags |= fs::O_WRITE; }
            if how.append { flags |= fs::O_APPEND; }
            if how.create { flags |= fs::O_CREATE; }
            if how.create_new { flags |= fs::O_CREATE_NEW; }
            if how.truncate { flags |= fs::O_TRUNCATE; }
            dir.open(rel, flags).map(|opened| Opened::Served(Arc::clone(cap), opened))
        },
    );
    // Installed once the turn is let go: on a full table the open file is
    // closed again (`served_fd`), and a kernel one by the table.
    let installed = opened.and_then(|opened| {
        match opened {
            Opened::Kernel(handle) => fd::install(Object::handle(handle), !how.cloexec, 0),
            Opened::Served(cap, opened) => served_fd(cap, opened, how.cloexec),
        }
        .map_err(Error::Fd)
    });
    answer(installed.map(|fd| fd as u64))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_stat(path: *const u8, len: usize, follow: bool, out: *mut Stat) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    let stat = on_path(path, |abs| syscall::stat(abs.as_bytes(), follow), |_, dir, rel| dir.stat(rel, follow).map(stat_of));
    answer(stat.map(|stat| {
        // SAFETY: the caller's live place.
        unsafe { out.write(stat) };
        0
    }))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_fstat(fd: Fd, out: *mut Stat) -> i64 {
    let stat = on_fd(fd, syscall::fstat, |dir, fid, generation| dir.fstat(fid, generation).map(stat_of));
    answer(stat.map(|stat| {
        // SAFETY: the caller's live place.
        unsafe { out.write(stat) };
        0
    }))
}

/// `f` on the stream `fd` names, held for the call, as a read or a write of
/// its descriptor does it: through its region, since its connection carries
/// frames. `None` for a descriptor that names no stream.
fn on_stream<T>(fd: Fd, f: impl FnOnce(RawHandle) -> Result<T, crate::net::stream::Refused>) -> Option<Result<T, Error>> {
    let streamed = fd::with(fd, |o| {
        (o.tag == fd::STREAM).then(|| f(o.handle).map_err(Error::Stream))
    });
    streamed.ok().flatten()
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_read(fd: Fd, buf: *mut u8, len: usize) -> i64 {
    // SAFETY: the caller lends `len` writable bytes for the call, and only
    // one of the closures runs.
    let out = || unsafe { core::slice::from_raw_parts_mut(buf, len) };
    let blocking = crate::net::stream::Wait::Blocking(None);
    if let Some(n) = on_stream(fd, |h| crate::net::stream::read(h, out(), blocking)) {
        return answer(n.map(|n| n as u64));
    }
    let n = on_fd(fd, |h| syscall::read(h, out()), |dir, fid, generation| dir.read(fid, generation, out()));
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
    let n = on_fd(fd, |h| syscall::write(h, buf), |dir, fid, generation| dir.write(fid, generation, buf).map(|w| w.len));
    answer(n.map(|n| n as u64))
}

#[no_mangle]
extern "C" fn toyos_fs_seek(fd: Fd, whence: u32, delta: i64) -> i64 {
    let (kernel, served) = match whence {
        SEEK_START => (SeekFrom::Start(delta as u64), fs::SEEK_START),
        SEEK_CURRENT => (SeekFrom::Current(delta), fs::SEEK_CURRENT),
        SEEK_END => (SeekFrom::End(delta), fs::SEEK_END),
        _ => return encode(Error::Kernel(SyscallError::InvalidArgument)),
    };
    answer(on_fd(fd, |h| syscall::seek(h, kernel), |dir, fid, generation| dir.seek(fid, generation, served, delta)))
}

#[no_mangle]
extern "C" fn toyos_fs_truncate(fd: Fd, size: u64) -> i64 {
    answer(on_fd(fd, |h| syscall::ftruncate(h, size), |dir, fid, generation| dir.truncate(fid, generation, size)).map(|()| 0))
}

#[no_mangle]
extern "C" fn toyos_fs_fsync(fd: Fd) -> i64 {
    answer(on_fd(fd, syscall::fsync, |dir, fid, generation| dir.fsync(fid, generation)).map(|()| 0))
}

#[no_mangle]
extern "C" fn toyos_fs_stream(fd: Fd) -> i64 {
    let handle = on_fd(fd, syscall::dup, |dir, fid, generation| dir.stream(fid, generation).map(|pipe| pipe.into_raw()));
    answer(handle.map(|h| u64::from(h.0)))
}

/// A call on one path, with nothing to answer but whether it was made.
///
/// # Safety
/// `path` names `len` readable bytes for the call.
unsafe fn path_call(
    path: *const u8,
    len: usize,
    kernel: impl FnMut(&str) -> Result<(), SyscallError>,
    served: impl FnMut(&mut Dir, &str) -> Result<(), Refused>,
) -> i64 {
    let mut served = served;
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    answer(on_path(path, kernel, |_, dir, rel| served(dir, rel)).map(|()| 0))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_mkdir(path: *const u8, len: usize) -> i64 {
    // SAFETY: the caller's.
    unsafe { path_call(path, len, |abs| syscall::mkdir(abs.as_bytes()), |dir, rel| dir.mkdir(rel)) }
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_rmdir(path: *const u8, len: usize) -> i64 {
    // SAFETY: the caller's.
    unsafe { path_call(path, len, |abs| syscall::rmdir(abs.as_bytes()), |dir, rel| dir.rmdir(rel)) }
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_unlink(path: *const u8, len: usize) -> i64 {
    // SAFETY: the caller's.
    unsafe { path_call(path, len, |abs| syscall::delete(abs.as_bytes()), |dir, rel| dir.unlink(rel)) }
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_chdir(path: *const u8, len: usize) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    let changed = utf8(path).and_then(|path| {
        let abs = super::absolute(&cwd(), path);
        if let Route::Served(..) = route(&abs)? {
            let stat = on_path(abs.as_bytes(), |abs| syscall::stat(abs.as_bytes(), true), |_, dir, rel| dir.stat(rel, true).map(stat_of))?;
            if stat.file_type != FileType::Directory {
                return Err(Error::Served(fs::Error::NotADirectory));
            }
        }
        syscall::chdir(abs.as_bytes()).map_err(Error::Kernel)
    });
    answer(changed.map(|()| 0))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_rename(from: *const u8, from_len: usize, to: *const u8, to_len: usize) -> i64 {
    // SAFETY: the caller's.
    let (from, to) = unsafe { (bytes(from, from_len), bytes(to, to_len)) };
    let renamed = (|| {
        let cwd = cwd();
        let (from, to) = (super::absolute(&cwd, utf8(from)?), super::absolute(&cwd, utf8(to)?));
        match (route(&from)?, route(&to)?) {
            // The kernel refuses a rename between its own mounts as
            // unsupported, which is a rename across servers as POSIX reads it.
            (Route::Kernel(a), Route::Kernel(b)) => syscall::rename(a.as_bytes(), b.as_bytes()).map_err(|e| match e {
                SyscallError::NotSupported => Error::CrossesServers,
                e => Error::Kernel(e),
            }),
            (Route::Served(ca, a), Route::Served(cb, b)) if Arc::ptr_eq(&ca, &cb) => {
                ca.dir.lock().rename(&a, &b).map_err(|refused| match refused {
                    Refused::Error(e) => Error::Served(e),
                    // A link out of the directory leads to another server.
                    Refused::Link(_) => Error::CrossesServers,
                })
            }
            _ => Err(Error::CrossesServers),
        }
    })();
    answer(renamed.map(|()| 0))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_symlink(target: *const u8, target_len: usize, link: *const u8, link_len: usize) -> i64 {
    // SAFETY: the caller's.
    let (target, link) = unsafe { (bytes(target, target_len), bytes(link, link_len)) };
    let made = utf8(target).and_then(|target| {
        on_path(link, |abs| syscall::symlink(target.as_bytes(), abs.as_bytes()), |_, dir, rel| {
            dir.symlink(target, rel).map_err(Refused::Error)
        })
    });
    answer(made.map(|()| 0))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_readlink(path: *const u8, len: usize, out: *mut u8, cap: usize) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    // SAFETY: the caller lends `cap` writable bytes for the call, and each
    // closure makes the slice only for its own run.
    let target = || unsafe { core::slice::from_raw_parts_mut(out, cap) };
    let n = on_path(path, |abs| syscall::readlink(abs.as_bytes(), target()), |_, dir, rel| {
        let mut buf = [0u8; fs::MAX_PATH];
        let n = dir.read_link(rel, &mut buf)?;
        let out = target();
        let n = n.min(out.len());
        out[..n].copy_from_slice(&buf[..n]);
        Ok(n)
    });
    answer(n.map(|n| n as u64))
}

/// A listing too large for the first buffer is asked again at the size it
/// named; a directory that outgrows its own listing this many times is
/// refused rather than waited on.
const LIST_ATTEMPTS: usize = 4;

#[no_mangle]
unsafe extern "C" fn toyos_fs_readdir(path: *const u8, len: usize, out: *mut u8, cap: usize) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    let listing = on_path(
        path,
        |abs| {
            let mut buf = alloc::vec![0u8; 64 * 1024];
            for _ in 0..LIST_ATTEMPTS {
                let n = syscall::readdir(abs.as_bytes(), &mut buf)?;
                if n <= buf.len() {
                    // The kernel's listing, in the served one's encoding.
                    let (mut out, mut at, mut entry) = (Vec::new(), 0, alloc::vec![0u8; 11 + fs::MAX_PATH]);
                    while let Some(d) = syscall::dirent(&buf[..n], &mut at) {
                        let kind = if d.is_dir { fs::KIND_DIR } else { fs::KIND_FILE };
                        let name = core::str::from_utf8(d.name).unwrap_or("");
                        let n = fs::encode_entry(&mut entry, kind, 0, name).ok_or(SyscallError::InvalidArgument)?;
                        out.extend_from_slice(&entry[..n]);
                    }
                    return Ok(out);
                }
                buf.resize(n, 0);
            }
            Err(SyscallError::ResourceExhausted)
        },
        |_, dir, rel| {
            let mut buf = alloc::vec![0u8; 64 * 1024];
            for _ in 0..LIST_ATTEMPTS {
                let n = dir.read_dir(rel, &mut buf)?;
                if n <= buf.len() {
                    buf.truncate(n);
                    return Ok(buf);
                }
                buf.resize(n, 0);
            }
            Err(Refused::Error(fs::Error::LimitExceeded))
        },
    );
    answer(listing.map(|listing| {
        if listing.len() <= cap {
            // SAFETY: the caller lends `cap` writable bytes for the call.
            unsafe { core::ptr::copy_nonoverlapping(listing.as_ptr(), out, listing.len()) };
        }
        listing.len() as u64
    }))
}

#[no_mangle]
unsafe extern "C" fn toyos_fs_is_served(path: *const u8, len: usize) -> i64 {
    // SAFETY: the caller's.
    let path = unsafe { bytes(path, len) };
    let served = utf8(path).and_then(|path| route(&super::absolute(&cwd(), path)));
    answer(served.map(|route| u64::from(matches!(route, Route::Served(..)))))
}
