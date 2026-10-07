//! Files a file server holds: the wire between a program and the server behind
//! one of its directory capabilities, and the client end of it.
//!
//! **A directory capability is a connector in the program's namespace**, named
//! [`CAPABILITY_PREFIX`] and the absolute directory it serves (`fs:/home`).
//! Each is a connector to its role's one port, which the supervisor minted with
//! a [`Grant`]: the directory, and whose share of the server it spends. The
//! kernel stamps that on every connection made through it and answers it to the
//! port's acceptor alone, so the server reads what was granted off the
//! connection and nothing the client says. A program names a file only under a
//! directory it holds, and the kernel's part is who holds which connector.
//!
//! **The server resolves; the client only asks.** A path on the wire is
//! relative to the capability's directory and canonical — no empty, `.` or
//! `..` component — and the server refuses any other ([`canonical`] is the one
//! rule both ends apply). A symlink whose target stays inside the directory is
//! followed by the server; one that is absolute is answered [`LINK`] with the
//! target and the rest of the path in the window, and resolved again in the
//! client's own table, so it reaches nothing the client does not hold.
//!
//! **One connection per directory per process, a request and then its
//! reply.** The client lends the server a [`WINDOW_BYTES`] region at connect
//! ([`HELLO`]); paths, data and listings travel through it and the frames stay
//! small and fixed. The window is shared memory both sides write, so each side
//! copies what it reads out of it once and acts on the copy.
//!
//! **A server that ends is survivable and not invisible.** [`Dir`] connects
//! again through the same connector — the supervisor keeps a file server's ports open
//! across its restart — and counts its connections in a generation. A file id
//! from an earlier generation names nothing on the new connection. Nothing
//! here keeps a write the server acknowledged and never made durable: that is
//! what `Fsync` is for.

use toyos_abi::syscall::{self, SyscallError, MAX_BADGE, MAX_SERVICE_NAME};

use crate::ipc::{Connection, IpcError};
use crate::ipc_payload;
use crate::namespace::Namespace;
use crate::shm::SharedMemory;
use crate::volatile::Window;
use crate::{AsHandle, OwnedHandle, Pipe, RawHandle};

/// What a directory capability's namespace name begins with.
pub const CAPABILITY_PREFIX: &str = "fs:";

/// The data window a client lends its server: one shared-memory page, the size
/// shared memory comes in.
pub const WINDOW_BYTES: usize = 2 << 20;

/// The longest path either end puts on the wire.
pub const MAX_PATH: usize = 4096;

/// The largest offset a file has, on every server: a page index that fits a
/// `u32`. A seek past it is refused where the offset is kept, and a write or a
/// truncate past it where the file is.
pub const MAX_FILE_BYTES: u64 = (u32::MAX as u64 + 1) * 4096;

// Requests, by frame type.
pub const HELLO: u32 = 1;
pub const OPEN: u32 = 2;
pub const CLOSE: u32 = 3;
pub const READ: u32 = 4;
pub const WRITE: u32 = 5;
pub const STAT: u32 = 6;
pub const LSTAT: u32 = 7;
pub const FSTAT: u32 = 8;
pub const TRUNCATE: u32 = 9;
pub const FSYNC: u32 = 10;
pub const READDIR: u32 = 11;
pub const MKDIR: u32 = 12;
pub const RMDIR: u32 = 13;
pub const UNLINK: u32 = 14;
pub const RENAME: u32 = 15;
pub const SYMLINK: u32 = 16;
pub const READLINK: u32 = 17;
pub const STREAM: u32 = 18;
pub const SYNC: u32 = 19;

// Replies, by frame type.
pub const REPLY: u32 = 0x100;
/// The path met an absolute symlink: the window holds the path to resolve
/// instead, `value` bytes long.
pub const LINK: u32 = 0x101;

// Open flags.
pub const O_READ: u64 = 1;
pub const O_WRITE: u64 = 2;
pub const O_APPEND: u64 = 4;
pub const O_CREATE: u64 = 8;
pub const O_TRUNCATE: u64 = 16;
pub const O_CREATE_NEW: u64 = 32;
pub const O_KNOWN: u64 = O_READ | O_WRITE | O_APPEND | O_CREATE | O_TRUNCATE | O_CREATE_NEW;

// Node kinds.
pub const KIND_FILE: u64 = 1;
pub const KIND_DIR: u64 = 2;
pub const KIND_SYMLINK: u64 = 3;

/// [`HELLO`]'s answer: the capability's rights.
pub const RIGHT_WRITE: u64 = 1;

ipc_payload! {
    /// Every request's words; what each means is its frame type's. Paths are
    /// in the window: the first `len` bytes, and for [`RENAME`] and
    /// [`SYMLINK`] a second `len2` bytes after it.
    pub struct Request {
        pub fid: u64,
        pub offset: u64,
        pub len: u64,
        pub len2: u64,
        pub flags: u64,
    }

    /// Every reply's words. `status` is 0 or a [`SyscallError`]'s wire value.
    pub struct Reply {
        pub status: u64,
        pub kind: u64,
        pub value: u64,
        pub value2: u64,
        pub mtime: u64,
    }
}

impl Request {
    pub const fn new() -> Self {
        Self { fid: 0, offset: 0, len: 0, len2: 0, flags: 0 }
    }
}

impl Reply {
    pub const fn ok() -> Self {
        Self { status: 0, kind: 0, value: 0, value2: 0, mtime: 0 }
    }

    pub const fn refused(e: SyscallError) -> Self {
        Self { status: e.to_u64(), kind: 0, value: 0, value2: 0, mtime: 0 }
    }

    fn result(self) -> Result<Self, SyscallError> {
        match self.status {
            0 => Ok(self),
            raw => Err(SyscallError::from_u64(raw).unwrap_or(SyscallError::Unknown)),
        }
    }
}

/// What a connection to a file server's role port was granted: the badge the
/// supervisor mints the connector with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant<'a> {
    /// The share of the server it spends: the supervisor mints one number for
    /// one service it starts itself or one login session, and for every
    /// launch made from it that opens no session.
    pub share: u64,
    /// The directory, as a path on the role's volume, every path on the
    /// connection is resolved beneath: `home`, or the empty path for a volume
    /// served whole. [`canonical`], and at most [`MAX_GRANT_ROOT`] bytes.
    pub root: &'a str,
}

/// The format [`Grant::encode`] writes. Carried because a swap replaces a file
/// server and not the supervisor, so one server reads grants another build
/// minted, and an older one is refused by name rather than read as this one.
const GRANT_VERSION: u8 = 2;

/// The longest root a grant carries: what one badge holds past the version and
/// the share.
pub const MAX_GRANT_ROOT: usize = MAX_BADGE - 1 - 8;

impl<'a> Grant<'a> {
    /// The version, the share, then the root: `None` for a root no grant
    /// can carry.
    pub fn encode<'b>(&self, out: &'b mut [u8; MAX_BADGE]) -> Option<&'b [u8]> {
        if self.root.len() > MAX_GRANT_ROOT || !canonical(self.root) {
            return None;
        }
        out[0] = GRANT_VERSION;
        out[1..9].copy_from_slice(&self.share.to_le_bytes());
        let end = 9 + self.root.len();
        out[9..end].copy_from_slice(self.root.as_bytes());
        Some(&out[..end])
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        let (&version, rest) = bytes.split_first()?;
        if version != GRANT_VERSION || rest.len() < 8 || rest.len() - 8 > MAX_GRANT_ROOT {
            return None;
        }
        let share = u64::from_le_bytes(rest[..8].try_into().expect("eight bytes"));
        let root = core::str::from_utf8(&rest[8..]).ok()?;
        canonical(root).then_some(Self { share, root })
    }
}

/// Whether `path` is a relative path the wire may carry: components of at
/// least one byte, none of them `.` or `..`, no leading or trailing `/`, and
/// the empty path for the capability's own directory.
pub fn canonical(path: &str) -> bool {
    path.len() <= MAX_PATH
        && (path.is_empty() || path.split('/').all(|c| !c.is_empty() && c != "." && c != ".."))
}

// **Who owns the window when.** The server, from the moment a request is sent
// until its reply is received; the client, from then until it sends the next.
// The send and the receive are syscalls, which order the two sides, so honest
// peers never touch the window at once. A dishonest one races only bytes it
// could have written anyway: each side copies what it reads out of the window
// once, into memory of its own, and acts on that copy alone. Nothing here polls
// the window or waits on it, so it is copied as plain bytes — one fetch of each,
// as a volatile loop would be, and no reference is ever formed over it.

/// Copy `data` into `window` at `offset`.
pub fn window_put(window: Window, offset: usize, data: &[u8]) {
    let target = window.sub(offset, data.len());
    // SAFETY: `sub` bounded `offset + data.len()` inside the window, which its
    // constructor's contract says is mapped; `data` is this process's own
    // memory and never the window, so the two do not overlap.
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), target.as_ptr(), data.len()) }
}

/// Copy `out.len()` bytes out of `window` at `offset`.
pub fn window_take(window: Window, offset: usize, out: &mut [u8]) {
    let source = window.sub(offset, out.len());
    // SAFETY: as in `window_put`, with the two sides swapped.
    unsafe { core::ptr::copy_nonoverlapping(source.as_ptr(), out.as_mut_ptr(), out.len()) }
}

/// What a file or directory is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub kind: u64,
    pub size: u64,
    pub mtime: u64,
}

/// A write answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Written {
    /// How many bytes it took.
    pub len: usize,
    /// The offset after them: for a file opened to append, the file's end.
    pub offset: u64,
}

/// An open answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened {
    pub fid: u64,
    pub stat: Stat,
    /// The connection this id belongs to: [`Dir::generation`] when it opened.
    pub generation: u64,
}

/// Why a request on a path was not answered with its result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The server's word.
    Error(SyscallError),
    /// The path met an absolute symlink; [`Dir::link_target`] holds the
    /// absolute path, `.0` bytes long, to resolve in the client's own table.
    Link(usize),
}

impl From<SyscallError> for Refused {
    fn from(e: SyscallError) -> Self {
        Refused::Error(e)
    }
}

/// One directory capability, connected.
pub struct Dir {
    names: &'static Namespace,
    name: [u8; MAX_SERVICE_NAME],
    name_len: usize,
    conn: Option<Connection>,
    window: SharedMemory,
    generation: u64,
    writable: bool,
}

impl Dir {
    /// Connect to the directory `names` calls `name`. `NotFound` is a name this
    /// process was not given.
    pub fn connect(names: &'static Namespace, name: &str) -> Result<Self, SyscallError> {
        if name.len() > MAX_SERVICE_NAME {
            return Err(SyscallError::InvalidArgument);
        }
        let mut buf = [0u8; MAX_SERVICE_NAME];
        buf[..name.len()].copy_from_slice(name.as_bytes());
        let window = SharedMemory::create(WINDOW_BYTES)?;
        let mut dir =
            Self { names, name: buf, name_len: name.len(), conn: None, window, generation: 0, writable: false };
        dir.reconnect()?;
        Ok(dir)
    }

    /// The capability's namespace name.
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("")
    }

    /// Counts this directory's connections; a file id is good only on the one
    /// it was answered on.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the capability lets this process change what it names.
    pub fn writable(&self) -> bool {
        self.writable
    }

    fn window(&self) -> Window {
        // SAFETY: the region is `WINDOW_BYTES` long and mapped for as long as
        // `self.window` lives, which every window taken here does not outlive.
        unsafe { Window::new(self.window.as_ptr(), WINDOW_BYTES) }
    }

    /// Open a fresh connection through the same connector, and lend it the
    /// window again. The old one's file ids name nothing from here on.
    fn reconnect(&mut self) -> Result<(), SyscallError> {
        self.conn = None;
        let name = core::str::from_utf8(&self.name[..self.name_len]).map_err(|_| SyscallError::InvalidArgument)?;
        let conn = self.names.open(name)?;
        let reply = hello(&conn, &self.window)?;
        self.writable = reply.value & RIGHT_WRITE != 0;
        self.conn = Some(conn);
        self.generation += 1;
        Ok(())
    }

    /// One request and its reply on the current connection. A connection that
    /// has ended answers `Gone` and is dropped, so the next call reconnects.
    fn call(&mut self, op: u32, request: &Request) -> Result<(u32, Reply), SyscallError> {
        if self.conn.is_none() {
            self.reconnect()?;
        }
        let conn = self.conn.as_ref().expect("connected above");
        let answered = conn.send(op, request).map_err(transport).and_then(|()| {
            let header = conn.recv_header().map_err(transport)?;
            let reply: Reply = conn.recv_payload(&header).map_err(transport)?;
            Ok((header.msg_type, reply))
        });
        if answered.is_err() {
            self.conn = None;
        }
        answered
    }

    /// A request naming paths: put them in the window first. A server that has
    /// gone is asked again once on a fresh connection, since a path means the
    /// same thing on both.
    fn path_call(&mut self, op: u32, mut request: Request, first: &str, second: &str) -> Result<Reply, Refused> {
        if !canonical(first) || !canonical(second) {
            return Err(Refused::Error(SyscallError::InvalidArgument));
        }
        let mut attempt = 0;
        loop {
            window_put(self.window(), 0, first.as_bytes());
            window_put(self.window(), first.len(), second.as_bytes());
            request.len = first.len() as u64;
            request.len2 = second.len() as u64;
            match self.call(op, &request) {
                Ok((LINK, reply)) => return Err(Refused::Link(reply.value.min(MAX_PATH as u64) as usize)),
                Ok((REPLY, reply)) => return reply.result().map_err(Refused::Error),
                Ok(_) => return Err(Refused::Error(SyscallError::Unknown)),
                Err(SyscallError::Gone) if attempt == 0 => {
                    attempt += 1;
                    self.reconnect()?;
                }
                Err(e) => return Err(Refused::Error(e)),
            }
        }
    }

    /// The absolute path a [`Refused::Link`] of `len` bytes left in the window.
    pub fn link_target<'b>(&self, len: usize, buf: &'b mut [u8; MAX_PATH]) -> &'b [u8] {
        window_take(self.window(), 0, &mut buf[..len]);
        &buf[..len]
    }

    /// A call on a file id: `Gone` when `generation` is not this connection's.
    fn fid_call(&mut self, op: u32, generation: u64, request: &Request) -> Result<Reply, SyscallError> {
        if generation != self.generation || self.conn.is_none() {
            return Err(SyscallError::Gone);
        }
        match self.call(op, request)? {
            (REPLY, reply) => reply.result(),
            _ => Err(SyscallError::Unknown),
        }
    }

    pub fn open(&mut self, path: &str, flags: u64) -> Result<Opened, Refused> {
        let reply = self.path_call(OPEN, Request { flags, ..Request::new() }, path, "")?;
        Ok(Opened { fid: reply.value, stat: stat_of(&reply), generation: self.generation })
    }

    pub fn close(&mut self, fid: u64, generation: u64) {
        let _ = self.fid_call(CLOSE, generation, &Request { fid, ..Request::new() });
    }

    /// Read at most `out.len()` bytes, and at most the window, at `offset`.
    pub fn read(&mut self, fid: u64, generation: u64, offset: u64, out: &mut [u8]) -> Result<usize, SyscallError> {
        let len = out.len().min(WINDOW_BYTES);
        let reply = self.fid_call(READ, generation, &Request { fid, offset, len: len as u64, ..Request::new() })?;
        let n = (reply.value as usize).min(len);
        window_take(self.window(), 0, &mut out[..n]);
        Ok(n)
    }

    /// Write at most the window of `data` at `offset`, or at the end for a file
    /// opened to append. Answers how much, and the offset after it.
    pub fn write(&mut self, fid: u64, generation: u64, offset: u64, data: &[u8]) -> Result<Written, SyscallError> {
        let len = data.len().min(WINDOW_BYTES);
        window_put(self.window(), 0, &data[..len]);
        let reply = self.fid_call(WRITE, generation, &Request { fid, offset, len: len as u64, ..Request::new() })?;
        Ok(Written { len: (reply.value as usize).min(len), offset: reply.value2 })
    }

    pub fn fstat(&mut self, fid: u64, generation: u64) -> Result<Stat, SyscallError> {
        self.fid_call(FSTAT, generation, &Request { fid, ..Request::new() }).map(|r| stat_of(&r))
    }

    pub fn truncate(&mut self, fid: u64, generation: u64, size: u64) -> Result<(), SyscallError> {
        self.fid_call(TRUNCATE, generation, &Request { fid, offset: size, ..Request::new() }).map(drop)
    }

    /// Make what this file was written durable on the volume.
    pub fn fsync(&mut self, fid: u64, generation: u64) -> Result<(), SyscallError> {
        self.fid_call(FSYNC, generation, &Request { fid, ..Request::new() }).map(drop)
    }

    /// Make the whole volume durable.
    pub fn sync(&mut self) -> Result<(), Refused> {
        self.path_call(SYNC, Request::new(), "", "").map(drop)
    }

    /// A pipe whose bytes the server appends to this file from `offset` on,
    /// for a program's standard output: the write end.
    pub fn stream(&mut self, fid: u64, generation: u64, offset: u64) -> Result<Pipe, SyscallError> {
        self.fid_call(STREAM, generation, &Request { fid, offset, ..Request::new() })?;
        let conn = self.conn.as_ref().ok_or(SyscallError::Gone)?;
        let [end] = conn.recv_handles_exact::<1>().ok_or(SyscallError::Unknown)?;
        Ok(Pipe(crate::OwnedHandle(end)))
    }

    pub fn stat(&mut self, path: &str, follow: bool) -> Result<Stat, Refused> {
        let op = if follow { STAT } else { LSTAT };
        self.path_call(op, Request::new(), path, "").map(|r| stat_of(&r))
    }

    /// The listing of `path`, [`for_each_entry`]'s encoding, into `out` when it
    /// fits. Answers the listing's length either way, so a caller whose buffer
    /// was short asks again with one that long.
    pub fn read_dir(&mut self, path: &str, out: &mut [u8]) -> Result<usize, Refused> {
        let reply = self.path_call(READDIR, Request::new(), path, "")?;
        let n = (reply.value as usize).min(WINDOW_BYTES);
        if n <= out.len() {
            window_take(self.window(), 0, &mut out[..n]);
        }
        Ok(n)
    }

    pub fn mkdir(&mut self, path: &str) -> Result<(), Refused> {
        self.path_call(MKDIR, Request::new(), path, "").map(drop)
    }

    pub fn rmdir(&mut self, path: &str) -> Result<(), Refused> {
        self.path_call(RMDIR, Request::new(), path, "").map(drop)
    }

    pub fn unlink(&mut self, path: &str) -> Result<(), Refused> {
        self.path_call(UNLINK, Request::new(), path, "").map(drop)
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), Refused> {
        self.path_call(RENAME, Request::new(), from, to).map(drop)
    }

    /// Make `link` a symlink holding `target`, which is stored as written and
    /// resolved by whoever follows it.
    pub fn symlink(&mut self, target: &str, link: &str) -> Result<(), Refused> {
        if target.is_empty() || target.len() > MAX_PATH {
            return Err(Refused::Error(SyscallError::InvalidArgument));
        }
        // The target is stored, not resolved, so it is not held to `canonical`.
        if !canonical(link) {
            return Err(Refused::Error(SyscallError::InvalidArgument));
        }
        let mut request = Request::new();
        window_put(self.window(), 0, link.as_bytes());
        window_put(self.window(), link.len(), target.as_bytes());
        request.len = link.len() as u64;
        request.len2 = target.len() as u64;
        match self.call(SYMLINK, &request) {
            Ok((REPLY, reply)) => reply.result().map(drop).map_err(Refused::Error),
            Ok(_) => Err(Refused::Error(SyscallError::Unknown)),
            Err(e) => Err(Refused::Error(e)),
        }
    }

    /// What the symlink at `path` holds, into `out`.
    pub fn read_link(&mut self, path: &str, out: &mut [u8; MAX_PATH]) -> Result<usize, Refused> {
        let reply = self.path_call(READLINK, Request::new(), path, "")?;
        let n = (reply.value as usize).min(MAX_PATH);
        window_take(self.window(), 0, &mut out[..n]);
        Ok(n)
    }
}

/// Each entry of a listing [`Dir::read_dir`] answered: `(kind, size, name)`.
/// An entry is its kind byte, its size, its name's length and the name.
pub fn for_each_entry(listing: &[u8], mut f: impl FnMut(u64, u64, &str)) -> Result<(), SyscallError> {
    let mut at = 0;
    while at < listing.len() {
        let head = listing.get(at..at + 11).ok_or(SyscallError::Unknown)?;
        let kind = head[0] as u64;
        let size = u64::from_le_bytes(head[1..9].try_into().expect("eight bytes"));
        let len = u16::from_le_bytes([head[9], head[10]]) as usize;
        let name = listing.get(at + 11..at + 11 + len).ok_or(SyscallError::Unknown)?;
        f(kind, size, core::str::from_utf8(name).map_err(|_| SyscallError::Unknown)?);
        at += 11 + len;
    }
    Ok(())
}

/// One listing entry's encoding, for the server: `None` once `out` is full.
pub fn encode_entry(out: &mut [u8], kind: u64, size: u64, name: &str) -> Option<usize> {
    let need = 11 + name.len();
    if need > out.len() || name.len() > u16::MAX as usize {
        return None;
    }
    out[0] = kind as u8;
    out[1..9].copy_from_slice(&size.to_le_bytes());
    out[9..11].copy_from_slice(&(name.len() as u16).to_le_bytes());
    out[11..need].copy_from_slice(name.as_bytes());
    Some(need)
}

fn stat_of(reply: &Reply) -> Stat {
    Stat { kind: reply.kind, size: reply.value2, mtime: reply.mtime }
}

/// Lend `window` to the server at the other end of `conn` ([`HELLO`]), and
/// answer what it granted, or why it refused.
///
/// A server that refuses a connection as it takes it answers before it lets
/// go, so a hello that finds it gone still reads why.
pub fn hello(conn: &Connection, window: &SharedMemory) -> Result<Reply, SyscallError> {
    let lent = OwnedHandle(window.share()?);
    // Not `send_with_handles`, which cannot say which send was refused: a
    // refused handle send leaves `lent` in this table, and it closes as it drops.
    let sent = match syscall::handle_send(conn.as_handle(), &[lent.raw()]) {
        Ok(()) => {
            lent.into_raw();
            conn.send(HELLO, &Request::new()).map_err(transport)
        }
        Err(e) => Err(e),
    };
    match sent {
        Ok(()) | Err(SyscallError::Gone) => {}
        Err(e) => return Err(e),
    }
    receive(conn)?.result()
}

fn receive(conn: &Connection) -> Result<Reply, SyscallError> {
    let header = conn.recv_header().map_err(transport)?;
    if header.msg_type != REPLY {
        return Err(SyscallError::Unknown);
    }
    conn.recv_payload(&header).map_err(transport)
}

/// Every way a connection's transport can fail is the server having gone.
fn transport(e: IpcError) -> SyscallError {
    match e {
        IpcError::Disconnected => SyscallError::Gone,
        IpcError::Syscall(SyscallError::Gone) => SyscallError::Gone,
        IpcError::Syscall(e) => e,
        IpcError::Malformed | IpcError::TooLarge => SyscallError::Unknown,
    }
}

impl AsHandle for Dir {
    fn as_handle(&self) -> RawHandle {
        self.conn.as_ref().map_or(toyos_abi::HANDLE_INVALID, |c| c.as_handle())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every offset and length a path or a short read puts in the window,
    /// aligned or not, round-trips byte for byte and touches nothing beside.
    #[test]
    fn the_window_copies_every_length_at_every_offset() {
        let mut backing = vec![0u64; 64];
        let base = backing.as_mut_ptr() as *mut u8;
        // SAFETY: 512 bytes of a live, 8-aligned allocation this test owns.
        let window = unsafe { Window::new(base, 512) };
        for offset in 0..24 {
            for len in 0..40 {
                window.zero();
                let data: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(7).wrapping_add(1)).collect();
                window_put(window, offset, &data);
                let mut back = vec![0u8; len];
                window_take(window, offset, &mut back);
                assert_eq!(back, data, "{len} bytes at {offset}");
                let mut whole = vec![0u8; 512];
                window_take(window, 0, &mut whole);
                assert!(whole[..offset].iter().all(|&b| b == 0), "before {offset}");
                assert!(whole[offset + len..].iter().all(|&b| b == 0), "after {offset}+{len}");
            }
        }
    }

    #[test]
    fn a_path_is_canonical_only_without_empty_dot_or_dotdot_components() {
        for good in ["", "a", "a/b", "toy/Documents/x.txt", "..a", "a..", ".x"] {
            assert!(canonical(good), "{good:?}");
        }
        for bad in ["/a", "a/", "a//b", ".", "..", "a/./b", "a/../b", "../x"] {
            assert!(!canonical(bad), "{bad:?}");
        }
        assert!(!canonical(&"a".repeat(MAX_PATH + 1)));
    }

    #[test]
    fn a_grant_round_trips_at_every_bound() {
        let longest = "r".repeat(MAX_GRANT_ROOT);
        for (share, root) in [(0, ""), (1, "home"), (u64::MAX, "home/toy/Documents"), (7, longest.as_str())] {
            let grant = Grant { share, root };
            let mut out = [0u8; MAX_BADGE];
            let bytes = grant.encode(&mut out).expect("a root a grant carries");
            assert_eq!(Grant::decode(bytes), Some(grant), "{root:?}");
        }
    }

    #[test]
    fn no_grant_carries_a_root_the_wire_refuses_or_one_past_the_badge() {
        let mut out = [0u8; MAX_BADGE];
        let past = "r".repeat(MAX_GRANT_ROOT + 1);
        for root in ["/home", "home/", "a//b", ".", "a/../b", past.as_str()] {
            assert_eq!(Grant { share: 1, root }.encode(&mut out), None, "{root:?}");
        }
    }

    #[test]
    fn bytes_no_grant_was_encoded_as_are_refused() {
        let mut out = [0u8; MAX_BADGE];
        let good = Grant { share: 3, root: "home" }.encode(&mut out).unwrap().to_vec();
        // Shorter than a version and a share.
        for n in 0..9 {
            assert_eq!(Grant::decode(&good[..n]), None, "{n} bytes");
        }
        // Another version.
        let mut other = good.clone();
        other[0] = GRANT_VERSION + 1;
        assert_eq!(Grant::decode(&other), None);
        // A root the wire refuses, or not UTF-8.
        for root in [&b"/home"[..], b"a/../b", b"a//b", b"\xff"] {
            let mut bad = good[..9].to_vec();
            bad.extend_from_slice(root);
            assert_eq!(Grant::decode(&bad), None, "{root:?}");
        }
        // One byte past the longest root.
        let mut long = good[..9].to_vec();
        long.extend(core::iter::repeat_n(b'r', MAX_GRANT_ROOT + 1));
        assert_eq!(Grant::decode(&long), None);
    }

    #[test]
    fn a_listing_entry_round_trips_and_a_short_one_is_refused() {
        let mut buf = [0u8; 64];
        let n = encode_entry(&mut buf, KIND_DIR, 42, "Documents").unwrap();
        let mut seen = Vec::new();
        for_each_entry(&buf[..n], |k, s, name| seen.push((k, s, name.to_string()))).unwrap();
        assert_eq!(seen, [(KIND_DIR, 42, "Documents".to_string())]);
        assert!(for_each_entry(&buf[..n - 1], |_, _, _| {}).is_err());
        assert_eq!(encode_entry(&mut buf[..10], KIND_FILE, 0, "x"), None);
    }
}
