//! Files a file server holds: the wire between a program and the server behind
//! one of its directory capabilities, and the client end of it.
//!
//! **A directory capability is a connector in the program's namespace**, named
//! [`CAPABILITY_PREFIX`] and the absolute directory it serves (`fs:/home`).
//! Each is a connector to its role's one port, which the supervisor minted with
//! a [`Grant`]: the directory, whether it may be changed, and whose share of
//! the server it spends. The kernel stamps that on every connection made through it and answers it to the
//! port's acceptor alone, so the server reads what was granted off the
//! connection and nothing the client says. A program names a file only under a
//! directory it holds, and the kernel's part is who holds which connector.
//!
//! **A path is resolved from a directory the connection holds**: [`ROOT`], the
//! grant's directory, or a directory it opened, by its file id. The server
//! resolves; the client only asks. A path on the wire is [`relative`]: no
//! empty or `.` component. A `..` is the parent of the directory it is met in,
//! which the server knows by the directory itself and not by the path spelled
//! to it; one that would climb above the grant's directory is refused
//! [`Error::Escapes`]. A symlink whose target stays inside the directory is
//! followed by the server; one that is absolute is answered [`FOLLOW`] with
//! the target and the rest of the path, and resolved again in the client's own
//! table, so it reaches nothing the client does not hold.
//!
//! **An open file or directory is a description the server keeps**: the
//! node, the access the open fixed and the offset. A connection names
//! descriptions by id, and [`DUP`] and [`EXPORT`] make second ids that share
//! one description's offset, on the same connection or on a connection made
//! for another process. An exported connection was given descriptions and no
//! directory: a path on it is refused [`Error::NoDirectory`].
//!
//! **A request carries a tag and its span of the window.** The client lends
//! the server a [`WINDOW_BYTES`] region at connect ([`HELLO`]); a request's
//! paths, data and answer travel in the span it names, and the frames stay
//! small and fixed. The reply carries the request's tag; a [`CLOSE`] has
//! none, so a close costs its client a send and no wait and is never
//! outstanding. A client keeps at most [`MAX_OUTSTANDING`] requests
//! unanswered on one connection and lets no two of their spans overlap; a
//! server reads a request's input out of its
//! span before it writes its answer there. The window is shared memory both
//! sides write, so each side copies what it reads out of it once and acts on
//! the copy.
//!
//! **A server that ends is survivable and not invisible.** [`Dir`] connects
//! again through the same connector — the supervisor keeps a file server's ports open
//! across its restart — and counts its connections in a generation. A file id
//! from an earlier generation names nothing on the new connection. Nothing
//! here keeps a write the server acknowledged and never made durable: that is
//! what `Fsync` is for.

use alloc::vec::Vec;

use toyos_abi::syscall::{SyscallError, MAX_BADGE, MAX_SERVICE_NAME};

use crate::ipc::{Connection, FrameRx, IpcError};
use crate::ipc_payload;
use crate::namespace::Namespace;
use crate::shm::{Bytes, SharedMemory};
use crate::Pipe;

/// What a directory capability's namespace name begins with.
pub const CAPABILITY_PREFIX: &str = "fs:";

/// The data window a client lends its server: one shared-memory page, the size
/// shared memory comes in.
pub const WINDOW_BYTES: usize = 2 << 20;

/// The longest path either end puts on the wire.
pub use toyos_abi::syscall::MAX_PATH;

/// The largest offset a file has, on every server: a page index that fits a
/// `u32`. A seek past it is refused where the offset is kept, and a write or a
/// truncate past it where the file is.
pub const MAX_FILE_BYTES: u64 = (u32::MAX as u64 + 1) * 4096;

/// The requests a client may have unanswered on one connection at once.
pub const MAX_OUTSTANDING: usize = 16;

/// The file id of the directory the connection's grant names. No open answers
/// it, and it is never closed.
pub const ROOT: u64 = 0;

// Requests, by frame type. A request naming a path names it in `fid` and the
// first `len` bytes of its span: from the directory `fid` is, or with no path
// the file or directory `fid` is itself. `fid2` and `len2` bytes after the
// first path name a second the same way.

/// Lend the window: it travels in the frame. `value` answers the rights the
/// grant gives ([`RIGHT_WRITE`]).
pub const HELLO: u32 = 1;
/// Open the path for [`O_READ`] and the other `O_` `flags`: `value` answers
/// the new file id, and the reply what it opened. A directory opens only to
/// read.
pub const OPEN: u32 = 2;
/// A file with no name, in the directory the path names, opened by `flags` as
/// [`OPEN`] opens: a later [`LINK`] gives it one, and nothing else ever does.
pub const CREATE_UNNAMED: u32 = 3;
/// Let the id `fid` go. Answered with no frame: a close the volume refuses
/// keeps its node for the next sync, which the server says in its log.
pub const CLOSE: u32 = 4;
/// Read the span's length at the description's offset into the span, and move
/// the offset past it: `value` answers how much.
pub const READ: u32 = 5;
/// Write the span at the description's offset, or at the end for a file opened
/// to append, and move the offset past it: `value` answers how much, and
/// `value2` the offset after it.
pub const WRITE: u32 = 6;
/// [`READ`] at `offset`, which the description's offset does not follow.
pub const PREAD: u32 = 7;
/// [`WRITE`] at `offset`, whatever the description appends to, and leaving its
/// offset where it was.
pub const PWRITE: u32 = 8;
/// Move the description's offset: `flags` is [`SEEK_START`], [`SEEK_CURRENT`]
/// or [`SEEK_END`], `offset` the signed distance from it; `value` is where it
/// landed.
pub const SEEK: u32 = 9;
/// A second id on the description `fid` names: `value`.
pub const DUP: u32 = 10;
/// A connection to this server holding the descriptions of the `len` ids in
/// the span, little-endian `u64`s, as ids 1.. in that order: the reply
/// carries its other end. It spends a served slot of this connection's share
/// from here, and names no path.
pub const EXPORT: u32 = 11;
/// What the path names, a symlink's target where `flags` has [`FOLLOW_LAST`].
pub const STAT: u32 = 12;
/// Change what the path names, [`FOLLOW_LAST`] as for [`STAT`]: its size to
/// `offset` ([`SET_SIZE`], only through an id opened to write and no path),
/// its access time to `atime` and its modification time to `mtime`, or either
/// to the server's now.
pub const SETATTR: u32 = 13;
/// Make what this file was written durable on the volume.
pub const FSYNC: u32 = 14;
/// Make the whole volume durable.
pub const SYNC: u32 = 15;
/// The free space of the volume behind the capability ([`Room`]).
pub const ROOM: u32 = 16;
/// A pipe whose bytes the server writes to the file at the description's
/// offset: the reply carries its write end.
pub const STREAM: u32 = 17;
/// The entries of the directory `fid` opened from the cursor `offset`, 0 at
/// its start, [`for_each_entry`]'s encoding in the span: `value` answers its
/// length and `value2` the cursor to ask from next, [`READDIR_END`] after the
/// last. A first entry past the span is refused [`Error::LimitExceeded`].
pub const READDIR: u32 = 18;
pub const MKDIR: u32 = 19;
/// Remove the name: a directory's only with [`UNLINK_DIR`], which removes
/// nothing else.
pub const UNLINK: u32 = 20;
/// Give the first path's node the second path's name, replacing a file or a
/// symlink there; never a directory.
pub const RENAME: u32 = 21;
/// Make the first path a symlink holding the second's bytes, stored as
/// written.
pub const SYMLINK: u32 = 22;
/// What the symlink at the path holds, into the span: `value` its length.
pub const READLINK: u32 = 23;
/// Give the first path's node, [`FOLLOW_LAST`] as for [`STAT`], the second path's
/// name too, which must be free; with [`LINK_REPLACE`], take the name from
/// the node the description `expected` names, and only if it still names it.
pub const LINK: u32 = 24;
/// The open file's bytes as a memory object nobody writes, its pages the
/// volume's own and its length the file's: the reply carries it. Refused
/// [`Error::Unsupported`] by a volume that holds the file in no such object,
/// whose bytes [`READ`] gives.
pub const IMAGE: u32 = 25;

pub const SEEK_START: u64 = 0;
pub const SEEK_CURRENT: u64 = 1;
pub const SEEK_END: u64 = 2;

/// [`READDIR`]'s cursor after a directory's last entry.
pub const READDIR_END: u64 = u64::MAX;

// Replies, by frame type.
pub const REPLY: u32 = 0x100;
/// The path met an absolute symlink: the span holds the path to resolve
/// instead, `value` bytes long.
pub const FOLLOW: u32 = 0x101;

// Open flags.
pub const O_READ: u64 = 1;
pub const O_WRITE: u64 = 2;
pub const O_APPEND: u64 = 4;
pub const O_CREATE: u64 = 8;
pub const O_TRUNCATE: u64 = 16;
pub const O_CREATE_NEW: u64 = 32;
/// Refused [`Error::NotADirectory`] unless the path names a directory.
pub const O_DIRECTORY: u64 = 64;
pub const O_KNOWN: u64 = O_READ | O_WRITE | O_APPEND | O_CREATE | O_TRUNCATE | O_CREATE_NEW | O_DIRECTORY;

/// [`STAT`]'s, [`SETATTR`]'s and [`LINK`]'s: follow a symlink in the path's
/// last component.
pub const FOLLOW_LAST: u64 = 1;
// [`SETATTR`]'s, beside [`FOLLOW_LAST`].
pub const SET_SIZE: u64 = 2;
pub const SET_ATIME: u64 = 4;
pub const SET_ATIME_NOW: u64 = 8;
pub const SET_MTIME: u64 = 16;
pub const SET_MTIME_NOW: u64 = 32;
pub const SETATTR_KNOWN: u64 = FOLLOW_LAST | SET_SIZE | SET_ATIME | SET_ATIME_NOW | SET_MTIME | SET_MTIME_NOW;
/// [`LINK`]'s, beside [`FOLLOW_LAST`].
pub const LINK_REPLACE: u64 = 2;
pub const LINK_KNOWN: u64 = FOLLOW_LAST | LINK_REPLACE;
/// [`UNLINK`]'s.
pub const UNLINK_DIR: u64 = 1;

// Node kinds.
pub const KIND_FILE: u64 = 1;
pub const KIND_DIR: u64 = 2;
pub const KIND_SYMLINK: u64 = 3;

/// [`HELLO`]'s answer: the capability's rights in `value`.
pub const RIGHT_WRITE: u64 = 1;

/// Set by the client on every volume a server answers ([`Stat::volume`]): a
/// kernel mount's is below it, so no server can name one, whatever it says
/// (`toyos_abi::syscall::Stat::volume`).
pub const SERVED_VOLUME: u64 = 1 << 63;

/// Declares [`Error`]: the refusals a server sends, each with its wire code,
/// and the client end's own words, which have none.
macro_rules! errors {
    (
        wire { $($(#[$doc:meta])* $name:ident = $code:literal,)* }
        local { $($(#[$ldoc:meta])* $lname:ident = $lcode:literal,)* }
    ) => {
        /// Why a request was not answered with its result: the server's word
        /// on the wire, or the client end's own.
        ///
        /// A kernel refusal of a call either end makes for the protocol is
        /// translated once, by [`Error::kernel`]; this protocol carries no
        /// kernel word.
        // The discriminants are the wire codes, and past them, the client's
        // words', so the compiler holds every one to its own.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u8)]
        pub enum Error {
            $($(#[$doc])* $name = $code,)*
            $($(#[$ldoc])* $lname = $lcode,)*
        }

        impl Error {
            /// The word a reply's `status` carries: `None` for one of the
            /// client end's own, which no server sends.
            pub const fn code(self) -> Option<u64> {
                match self {
                    $(Self::$name => Some($code),)*
                    $(Self::$lname => None,)*
                }
            }

            /// The refusal a server's `code` names: `None` for a word no
            /// server sends.
            pub const fn from_code(code: u64) -> Option<Self> {
                match code {
                    $($code => Some(Self::$name),)*
                    _ => None,
                }
            }

            /// The word [`Self::byte`] answered, the client's own among them:
            /// how one crosses `crate::files`' C interface.
            pub const fn from_byte(byte: u8) -> Option<Self> {
                match byte {
                    $($code => Some(Self::$name),)*
                    $($lcode => Some(Self::$lname),)*
                    _ => None,
                }
            }

            /// This word's discriminant, the client's own words' too.
            pub const fn byte(self) -> u8 {
                self as u8
            }

            #[cfg(test)]
            const WIRE: &[Error] = &[$(Error::$name,)*];
            #[cfg(test)]
            const LOCAL: &[Error] = &[$(Error::$lname,)*];
        }
    };
}


errors! {
    wire {
        NotFound = 1,
        AlreadyExists = 2,
        /// A directory was asked for and this is a file or a link.
        NotADirectory = 3,
        /// A file was asked for and this is a directory.
        IsADirectory = 4,
        /// A directory with anything in it is not removed.
        NotEmpty = 5,
        /// A link's target was asked of something that is not one.
        NotALink = 6,
        /// A name, a path or a link's target longer than the volume or the wire
        /// holds.
        NameTooLong = 7,
        /// A name the volume's format cannot store.
        InvalidName = 8,
        /// The directory's grant, or its volume, lets nothing in it change.
        ReadOnly = 9,
        /// A write, a truncate or a stream on a file not opened to write.
        NotOpenForWriting = 10,
        /// A directory the server serves as a root is not removed or renamed.
        Root = 11,
        /// A link's target climbs out of the directory the connection was given.
        Escapes = 12,
        /// A rename or a link made through an absolute link would span two
        /// directories, which one connection does not.
        CrossesDirectories = 13,
        /// More links on the way than are followed, or a cycle of them.
        Loop = 14,
        /// A directory renamed beneath itself.
        IntoItself = 15,
        /// The volume has no room for it.
        NoSpace = 16,
        /// Past [`MAX_FILE_BYTES`], or past the largest file the volume's format
        /// holds.
        FileTooLarge = 17,
        /// A bound the server or its volume keeps, refused rather than answered
        /// short: a listing's first entry past its span, a directory with more
        /// entries than it reads, a file in more pieces than its entry records.
        LimitExceeded = 18,
        /// The connection holds as many files open as the server keeps for one.
        TooManyOpen = 19,
        /// The server, or the share the grant names, serves as many connections
        /// as it takes.
        TooManyConnections = 20,
        /// The server, or the share the grant names, serves as many streams as it
        /// takes.
        TooManyStreams = 21,
        /// The device did not do it, or the volume's own structures do not decode.
        Io = 22,
        /// The file was removed or renamed over while it was open, a
        /// [`LINK_REPLACE`] found its name no longer on the node it expected, or
        /// the id was answered on an earlier connection to a server that has
        /// since restarted.
        Stale = 23,
        /// A file id the connection does not hold.
        UnknownFile = 24,
        /// A path that is not UTF-8, not [`relative`], or not inside the span.
        InvalidPath = 25,
        /// A request whose words the protocol gives no meaning: an open flag it
        /// does not have, a length past the window, an empty name or target.
        InvalidRequest = 26,
        /// The volume's format has no such thing, as FAT32 has no symlink.
        Unsupported = 27,
        /// The kernel had no room for what the request needs: a handle, memory,
        /// a place in a queue.
        OutOfResources = 28,
        /// A path on a connection that was given descriptions and no directory.
        NoDirectory = 32,
    }
    local {
        /// The server ended, and the connection with it.
        Gone = 0x80,
        /// This process holds no directory capability by that name.
        NotHeld = 0x81,
        /// The server answered something this protocol does not say.
        Malformed = 0x82,
    }
}

impl Error {
    /// A kernel refusal of `call`, a call made for this protocol, as the
    /// protocol names it. The kernel answers these calls nothing else for a
    /// handle this end holds as it made it, so any other word is a broken
    /// invariant.
    pub fn kernel(call: &str, e: SyscallError) -> Self {
        match e {
            SyscallError::Gone => Self::Gone,
            SyscallError::ResourceExhausted => Self::OutOfResources,
            other => panic!("toyos::fs: the kernel refused {call} {other:?}, which it never answers it"),
        }
    }
}


ipc_payload! {
    /// Every request's words; what each means is its frame type's. `tag` is
    /// the client's and comes back on the reply; `at` and `span` are the
    /// request's bytes of the window.
    pub struct Request {
        pub tag: u64,
        pub fid: u64,
        pub fid2: u64,
        /// [`LINK_REPLACE`]'s description.
        pub expected: u64,
        pub offset: u64,
        pub at: u64,
        pub span: u64,
        pub len: u64,
        pub len2: u64,
        pub flags: u64,
        pub atime: u64,
        pub mtime: u64,
    }

    /// Every reply's words. `status` is 0 or an [`Error`]'s [`Error::code`];
    /// a reply that answers what a node is fills the words from `kind` on
    /// ([`Stat`]).
    pub struct Reply {
        pub tag: u64,
        pub status: u64,
        pub value: u64,
        pub value2: u64,
        pub kind: u64,
        pub size: u64,
        pub nlink: u64,
        pub atime: u64,
        pub mtime: u64,
        pub ctime: u64,
        pub volume: u64,
        pub node: u64,
    }
}

impl Default for Request {
    fn default() -> Self {
        Self::new()
    }
}

impl Request {
    pub const fn new() -> Self {
        Self { tag: 0, fid: ROOT, fid2: ROOT, expected: 0, offset: 0, at: 0, span: 0, len: 0, len2: 0, flags: 0, atime: 0, mtime: 0 }
    }
}

impl Reply {
    pub const fn ok() -> Self {
        Self { tag: 0, status: 0, value: 0, value2: 0, kind: 0, size: 0, nlink: 0, atime: 0, mtime: 0, ctime: 0, volume: 0, node: 0 }
    }

    /// A server's refusal; one of the client end's own words is never one.
    pub fn refused(e: Error) -> Self {
        let status = e.code().unwrap_or_else(|| panic!("toyos::fs: a server refused with {e:?}, which is the client's"));
        Self { status, ..Self::ok() }
    }

    /// An answer of what a node is, `volume` as the server numbers it.
    pub fn stat(stat: &Stat) -> Self {
        Self {
            kind: stat.kind,
            size: stat.size,
            nlink: stat.nlink,
            atime: stat.atime,
            mtime: stat.mtime,
            ctime: stat.ctime,
            volume: stat.volume,
            node: stat.node,
            ..Self::ok()
        }
    }

    /// The reply, or the refusal its status names; a status this protocol
    /// does not have is [`Error::Malformed`].
    fn result(self) -> Result<Self, Error> {
        match self.status {
            0 => Ok(self),
            code => Err(Error::from_code(code).unwrap_or(Error::Malformed)),
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
    /// Whether a request that changes what the directory holds is served.
    pub access: Access,
    /// The directory, as a path on the role's volume, every path on the
    /// connection is resolved beneath: `home`, or the empty path for a volume
    /// served whole. [`canonical`], and at most [`MAX_GRANT_ROOT`] bytes.
    pub root: &'a str,
}

/// What a [`Grant`] lets its holder do to the directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read, list and stat; every request that would change what the
    /// directory holds is refused [`Error::ReadOnly`].
    ReadOnly,
    ReadWrite,
}

/// The format [`Grant::encode`] writes. Carried because a swap replaces a file
/// server and not the supervisor, so one server reads grants another build
/// minted, and an older one is refused by name rather than read as this one.
const GRANT_VERSION: u8 = 3;

/// The longest root a grant carries: what one badge holds past the version,
/// the share and the access.
pub const MAX_GRANT_ROOT: usize = MAX_BADGE - 1 - 8 - 1;

impl<'a> Grant<'a> {
    /// The version, the share, the access, then the root: `None` for a root
    /// no grant can carry.
    pub fn encode<'b>(&self, out: &'b mut [u8; MAX_BADGE]) -> Option<&'b [u8]> {
        if self.root.len() > MAX_GRANT_ROOT || !canonical(self.root) {
            return None;
        }
        out[0] = GRANT_VERSION;
        out[1..9].copy_from_slice(&self.share.to_le_bytes());
        out[9] = match self.access {
            Access::ReadOnly => 0,
            Access::ReadWrite => 1,
        };
        let end = 10 + self.root.len();
        out[10..end].copy_from_slice(self.root.as_bytes());
        Some(&out[..end])
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        let (&version, rest) = bytes.split_first()?;
        if version != GRANT_VERSION || rest.len() < 9 || rest.len() - 9 > MAX_GRANT_ROOT {
            return None;
        }
        let share = u64::from_le_bytes(rest[..8].try_into().expect("eight bytes"));
        let access = match rest[8] {
            0 => Access::ReadOnly,
            1 => Access::ReadWrite,
            _ => return None,
        };
        let root = core::str::from_utf8(&rest[9..]).ok()?;
        canonical(root).then_some(Self { share, access, root })
    }
}

/// Whether `path` is a path the wire may carry from a directory: components of
/// at least one byte, none of them `.`, no leading or trailing `/`, and the
/// empty path for the directory itself. A `..` is the server's to resolve.
pub fn relative(path: &str) -> bool {
    path.len() <= MAX_PATH && (path.is_empty() || path.split('/').all(|c| !c.is_empty() && c != "."))
}

/// Whether `path` is [`relative`] and climbs nowhere: no `..` either. What a
/// grant's root is.
pub fn canonical(path: &str) -> bool {
    relative(path) && (path.is_empty() || path.split('/').all(|c| c != ".."))
}

/// `path` as the wire carries it from a directory: its empty and `.`
/// components dropped, its `..` kept. `None` for one longer than a path.
pub fn wire_path(path: &str) -> Option<alloc::string::String> {
    let parts: Vec<&str> = path.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
    let joined = parts.join("/");
    (joined.len() <= MAX_PATH).then_some(joined)
}

// **Who owns the window when.** The server owns a request's span from the
// moment it is sent until its reply is received; the client owns it from then
// until it sends another request over it. The send and the receive are
// syscalls, which order the two sides, so honest peers never touch one span
// at once. A dishonest one races only bytes it could have written anyway:
// each side copies what it reads out of the window once, into memory of its
// own, and acts on that copy alone.

/// What a file or directory is, and which. `volume` and `node` mean what
/// `toyos_abi::syscall::Stat`'s do, `volume` with [`SERVED_VOLUME`] set once
/// the client has it. A time is nanoseconds since the Unix epoch, and 0 one
/// the volume does not keep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    pub kind: u64,
    pub size: u64,
    /// The names it has.
    pub nlink: u64,
    pub atime: u64,
    pub mtime: u64,
    /// When what it is last changed: its contents, a name or an attribute.
    pub ctime: u64,
    pub volume: u64,
    pub node: u64,
}

/// How much more the volume behind a capability holds: `free` bytes, taken in
/// allocation units of `unit` bytes, so a file of `n` bytes takes `n` rounded
/// up to `unit`. A volume's own structures take room too, so it is a bound and
/// never a promise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Room {
    pub free: u64,
    pub unit: u64,
}

/// A write answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Written {
    /// How many bytes it took.
    pub len: usize,
    /// The offset after them: for a file opened to append, the file's end.
    pub offset: u64,
}

/// A file id, and the connection it was answered on: [`Dir::generation`]
/// then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fid {
    pub id: u64,
    pub generation: u64,
}

/// An open answered.
#[derive(Debug)]
pub struct Opened {
    pub fid: Fid,
    pub stat: Stat,
    /// A holder of the file's paged object, through which it is read
    /// without a request ([`crate::paged::Paged`]): a regular file opened to
    /// read on a server that keeps its files' pages there.
    pub holder: Option<crate::OwnedHandle>,
}

/// Where a path a request names is resolved from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    /// The capability's own directory, on whichever connection is current.
    Root,
    /// A directory this connection opened; with the empty path, the file or
    /// directory the id names itself.
    Fid(Fid),
}

/// A time [`Dir::set_attr`] sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Time {
    /// The server's clock as it sets it.
    Now,
    /// Nanoseconds since the Unix epoch.
    At(u64),
}

/// What [`Dir::set_attr`] changes; `None` leaves it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub size: Option<u64>,
    pub atime: Option<Time>,
    pub mtime: Option<Time>,
}

impl Change {
    /// [`SETATTR`]'s flags and words for this change.
    pub fn encode(&self, request: &mut Request) {
        if let Some(size) = self.size {
            request.flags |= SET_SIZE;
            request.offset = size;
        }
        for (time, set, now, word) in
            [(self.atime, SET_ATIME, SET_ATIME_NOW, &mut request.atime), (self.mtime, SET_MTIME, SET_MTIME_NOW, &mut request.mtime)]
        {
            match time {
                None => {}
                Some(Time::Now) => request.flags |= now,
                Some(Time::At(nanos)) => {
                    request.flags |= set;
                    *word = nanos;
                }
            }
        }
    }

    /// The change a [`SETATTR`]'s flags and words ask for: `None` for flags
    /// that set one time two ways.
    pub fn decode(request: &Request) -> Option<Self> {
        let time = |set: u64, now: u64, word: u64| match (request.flags & set != 0, request.flags & now != 0) {
            (false, false) => Some(None),
            (true, false) => Some(Some(Time::At(word))),
            (false, true) => Some(Some(Time::Now)),
            (true, true) => None,
        };
        Some(Self {
            size: (request.flags & SET_SIZE != 0).then_some(request.offset),
            atime: time(SET_ATIME, SET_ATIME_NOW, request.atime)?,
            mtime: time(SET_MTIME, SET_MTIME_NOW, request.mtime)?,
        })
    }
}

/// Why a request on a path was not answered with its result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Error(Error),
    /// The path met an absolute symlink; [`Dir::follow_target`] holds the
    /// absolute path, `.0` bytes long, to resolve in the client's own table.
    Follow(usize),
}

impl From<Error> for Refused {
    fn from(e: Error) -> Self {
        Refused::Error(e)
    }
}

/// Where a [`Dir`]'s connection comes from.
enum Source {
    /// A directory capability, connected again through the same connector.
    Named { names: &'static Namespace, name: [u8; MAX_SERVICE_NAME], len: usize },
    /// A connection another process exported: one connection, never another.
    Exported,
}

/// One directory capability, or one exported connection, connected. It keeps
/// one request outstanding at a time, its span the window's start.
pub struct Dir {
    source: Source,
    conn: Option<Connection>,
    window: SharedMemory,
    generation: u64,
    writable: bool,
    /// The tag the next request carries.
    tag: u64,
}

/// A path request's two paths: the second is a path, or a symlink's target,
/// which is stored as written and so held to no rule but its length.
struct Paths<'a> {
    first: &'a str,
    second: &'a [u8],
}

impl Dir {
    /// Connect to the directory `names` calls `name`: [`Error::NotHeld`] for a
    /// name this process was not given.
    pub fn connect(names: &'static Namespace, name: &str) -> Result<Self, Error> {
        if name.len() > MAX_SERVICE_NAME {
            return Err(Error::NotHeld);
        }
        let mut buf = [0u8; MAX_SERVICE_NAME];
        buf[..name.len()].copy_from_slice(name.as_bytes());
        let mut dir = Self::unconnected(Source::Named { names, name: buf, len: name.len() })?;
        dir.reconnect()?;
        Ok(dir)
    }

    /// Serve the connection an [`EXPORT`] answered, whose descriptions are ids
    /// 1.. in the order exported. It names no path, and is not connected again
    /// once it has ended.
    pub fn adopt(conn: Connection) -> Result<Self, Error> {
        let mut dir = Self::unconnected(Source::Exported)?;
        dir.attach(conn)?;
        Ok(dir)
    }

    fn unconnected(source: Source) -> Result<Self, Error> {
        let window = SharedMemory::create(WINDOW_BYTES).map_err(|e| Error::kernel("a window", e))?;
        Ok(Self { source, conn: None, window, generation: 0, writable: false, tag: 0 })
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

    fn window(&self) -> Bytes<'_> {
        self.window.bytes()
    }

    /// Open a fresh connection through the same connector, and lend it the
    /// window again. The old one's file ids name nothing from here on; an
    /// exported connection has no connector, and stays gone.
    fn reconnect(&mut self) -> Result<(), Error> {
        self.conn = None;
        let Source::Named { names, name, len } = &self.source else { return Err(Error::Gone) };
        let name = core::str::from_utf8(&name[..*len]).expect("copied from a str");
        let conn = match names.open(name) {
            Ok(conn) => conn,
            Err(SyscallError::NotFound) => return Err(Error::NotHeld),
            Err(e) => return Err(Error::kernel("a connection", e)),
        };
        self.attach(conn)
    }

    /// Lend `conn` the window and serve on it.
    fn attach(&mut self, conn: Connection) -> Result<(), Error> {
        let reply = hello(&conn, &self.window)?;
        self.writable = reply.value & RIGHT_WRITE != 0;
        self.conn = Some(conn);
        self.generation += 1;
        Ok(())
    }

    /// The one handle a reply that answers with one carries; an error carries
    /// none, and any other count ends the connection.
    fn one(&mut self, msg_type: u32, reply: Reply, carried: crate::ipc::Batch) -> Result<crate::OwnedHandle, Error> {
        let n = carried.len();
        match (answer(msg_type, reply), carried.exact::<1>()) {
            (Ok(_), Some([handle])) => Ok(handle),
            (Err(e), None) if n == 0 => Err(e),
            _ => {
                self.conn = None;
                Err(Error::Malformed)
            }
        }
    }

    /// One request and its reply on the current connection. A connection that
    /// has ended answers `Gone` and is dropped, so the next call reconnects.
    fn call(&mut self, op: u32, request: Request) -> Result<(u32, Reply), Error> {
        let (msg_type, reply, carried) = self.call_with(op, request)?;
        if !carried.is_empty() {
            // A reply that brings handles nobody asked for speaks another protocol.
            self.conn = None;
            return Err(Error::Malformed);
        }
        Ok((msg_type, reply))
    }

    /// [`Self::call`], and the handles the reply frame carries into `batch`.
    /// A reply to another tag than the request's is [`Error::Malformed`].
    fn call_with(
        &mut self,
        op: u32,
        mut request: Request,
    ) -> Result<(u32, Reply, crate::ipc::Batch), Error> {
        if self.conn.is_none() {
            self.reconnect()?;
        }
        self.tag = self.tag.wrapping_add(1);
        request.tag = self.tag;
        let conn = self.conn.as_ref().expect("connected above");
        let mut rx = FrameRx::<{ core::mem::size_of::<Reply>() }>::new();
        let answered = conn.send(op, &request).map_err(transport).and_then(|()| {
            let frame = rx.recv(conn).map_err(transport)?;
            let reply: Reply = crate::ipc::decode_payload(rx.payload(frame.payload_len)).map_err(transport)?;
            if reply.tag != request.tag {
                return Err(Error::Malformed);
            }
            Ok((frame.msg_type, reply, rx.take_batch()))
        });
        if answered.is_err() {
            self.conn = None;
        }
        answered
    }

    /// The file id `base` names on the current connection, and whether a
    /// request from it means the same on a fresh one: [`Error::Stale`] for an
    /// id of an earlier connection.
    fn base(&self, base: Base) -> Result<(u64, bool), Error> {
        match base {
            Base::Root => Ok((ROOT, true)),
            Base::Fid(fid) => self.held(fid).map(|id| (id, false)),
        }
    }

    /// `fid`'s id on the current connection, or [`Error::Stale`].
    fn held(&self, fid: Fid) -> Result<u64, Error> {
        match fid.generation == self.generation && self.conn.is_some() {
            true => Ok(fid.id),
            false => Err(Error::Stale),
        }
    }

    /// A request naming paths, from `bases`: put them in the span first, which
    /// is `room` bytes at least for what the answer puts there. A server that
    /// has gone is asked again once on a fresh connection when every base is
    /// the root, since a path from it means the same thing on both.
    fn path_call(&mut self, op: u32, request: Request, bases: [Base; 2], paths: Paths<'_>, room: usize) -> Result<Reply, Refused> {
        let (reply, carried) = self.path_call_with(op, request, bases, paths, room)?;
        if !carried.is_empty() {
            // A reply that brings handles nobody asked for speaks another protocol.
            self.conn = None;
            return Err(Refused::Error(Error::Malformed));
        }
        Ok(reply)
    }

    /// [`Self::path_call`], and the handles the reply carries.
    fn path_call_with(
        &mut self,
        op: u32,
        mut request: Request,
        bases: [Base; 2],
        paths: Paths<'_>,
        room: usize,
    ) -> Result<(Reply, crate::ipc::Batch), Refused> {
        if !relative(paths.first) {
            return Err(Refused::Error(Error::InvalidPath));
        }
        // Room for a link to follow, which the server answers in the span.
        let span = (paths.first.len() + paths.second.len()).max(room).clamp(MAX_PATH, WINDOW_BYTES);
        let mut attempt = 0;
        loop {
            let (fid, again) = self.base(bases[0])?;
            let (fid2, again2) = self.base(bases[1])?;
            self.window().write(0, paths.first.as_bytes());
            self.window().write(paths.first.len(), paths.second);
            request.fid = fid;
            request.fid2 = fid2;
            request.at = 0;
            request.span = span as u64;
            request.len = paths.first.len() as u64;
            request.len2 = paths.second.len() as u64;
            match self.call_with(op, request) {
                Ok((msg_type, reply, carried)) => return path_answer(msg_type, reply, span).map(|reply| (reply, carried)),
                Err(Error::Gone) if attempt == 0 && again && again2 => {
                    attempt += 1;
                    self.reconnect()?;
                }
                Err(e) => return Err(Refused::Error(e)),
            }
        }
    }

    /// The absolute path a [`Refused::Follow`] of `len` bytes left in the
    /// window.
    pub fn follow_target<'b>(&self, len: usize, buf: &'b mut [u8; MAX_PATH]) -> &'b [u8] {
        self.window().read(0, &mut buf[..len]);
        &buf[..len]
    }

    /// A call on a file id, with `span` bytes of the window from its start.
    fn fid_call(&mut self, op: u32, fid: Fid, mut request: Request, span: usize) -> Result<Reply, Error> {
        request.fid = self.held(fid)?;
        request.at = 0;
        request.span = span as u64;
        let (msg_type, reply) = self.call(op, request)?;
        answer(msg_type, reply)
    }

    pub fn open(&mut self, base: Base, path: &str, flags: u64) -> Result<Opened, Refused> {
        self.opened(OPEN, base, path, flags)
    }

    /// A file with no name in the directory `path` names, opened by `flags`
    /// as [`Self::open`] opens: [`Self::link`] gives it one.
    pub fn create_unnamed(&mut self, base: Base, path: &str, flags: u64) -> Result<Opened, Refused> {
        self.opened(CREATE_UNNAMED, base, path, flags)
    }

    fn opened(&mut self, op: u32, base: Base, path: &str, flags: u64) -> Result<Opened, Refused> {
        let paths = Paths { first: path, second: &[] };
        let (reply, mut carried) = self.path_call_with(op, Request { flags, ..Request::new() }, [base, Base::Root], paths, 0)?;
        let fid = Fid { id: reply.value, generation: self.generation };
        let holder = carried.next();
        if !carried.is_empty() {
            self.close(fid);
            self.conn = None;
            return Err(Refused::Error(Error::Malformed));
        }
        Ok(Opened { fid, stat: stat_of(&reply), holder })
    }

    /// Let `fid` go, without waiting for the server: [`CLOSE`] is answered
    /// with nothing.
    pub fn close(&mut self, fid: Fid) {
        let Ok(id) = self.held(fid) else { return };
        let conn = self.conn.as_ref().expect("an id is held on a connection");
        if conn.send(CLOSE, &Request { fid: id, ..Request::new() }).is_err() {
            self.conn = None;
        }
    }

    /// Read at most `out.len()` bytes, and at most the window, at the
    /// description's offset, which moves past them.
    pub fn read(&mut self, fid: Fid, out: &mut [u8]) -> Result<usize, Error> {
        self.read_with(READ, fid, 0, out)
    }

    /// Read at most `out.len()` bytes, and at most the window, at `offset`,
    /// leaving the description's offset where it is.
    pub fn pread(&mut self, fid: Fid, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        self.read_with(PREAD, fid, offset, out)
    }

    fn read_with(&mut self, op: u32, fid: Fid, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let len = out.len().min(WINDOW_BYTES);
        let reply = self.fid_call(op, fid, Request { offset, ..Request::new() }, len)?;
        let n = counted(reply.value, len)?;
        self.window().read(0, &mut out[..n]);
        Ok(n)
    }

    /// Write at most the window of `data` at the description's offset, or at
    /// the end for a file opened to append; the offset moves past it. Answers
    /// how much, and the offset after it, which is never past
    /// [`MAX_FILE_BYTES`].
    pub fn write(&mut self, fid: Fid, data: &[u8]) -> Result<Written, Error> {
        let len = data.len().min(WINDOW_BYTES);
        self.window().write(0, &data[..len]);
        let reply = self.fid_call(WRITE, fid, Request::new(), len)?;
        let offset = Some(reply.value2).filter(|&at| at <= MAX_FILE_BYTES).ok_or(Error::Malformed)?;
        Ok(Written { len: counted(reply.value, len)?, offset })
    }

    /// Write at most the window of `data` at `offset`, leaving the
    /// description's offset where it is: how much.
    pub fn pwrite(&mut self, fid: Fid, offset: u64, data: &[u8]) -> Result<usize, Error> {
        let len = data.len().min(WINDOW_BYTES);
        self.window().write(0, &data[..len]);
        let reply = self.fid_call(PWRITE, fid, Request { offset, ..Request::new() }, len)?;
        counted(reply.value, len)
    }

    /// Move the description's offset `delta` from `whence` ([`SEEK_START`],
    /// [`SEEK_CURRENT`] or [`SEEK_END`]): where it landed.
    pub fn seek(&mut self, fid: Fid, whence: u64, delta: i64) -> Result<u64, Error> {
        let request = Request { offset: delta as u64, flags: whence, ..Request::new() };
        self.fid_call(SEEK, fid, request, 0).map(|r| r.value)
    }

    /// A second id on `fid`'s description, sharing its offset.
    pub fn dup(&mut self, fid: Fid) -> Result<Fid, Error> {
        let reply = self.fid_call(DUP, fid, Request::new(), 0)?;
        Ok(Fid { id: reply.value, generation: fid.generation })
    }

    /// A connection for another process holding the descriptions `fids` name,
    /// as ids 1.. in this order ([`Dir::adopt`]). It spends a served slot of
    /// this connection's share for as long as anybody holds it.
    pub fn export(&mut self, fids: &[Fid]) -> Result<Connection, Error> {
        if fids.is_empty() || fids.len() * 8 > WINDOW_BYTES {
            return Err(Error::InvalidRequest);
        }
        for (i, fid) in fids.iter().enumerate() {
            self.window().write(i * 8, &self.held(*fid)?.to_le_bytes());
        }
        let request = Request { len: fids.len() as u64, span: (fids.len() * 8) as u64, ..Request::new() };
        let (msg_type, reply, carried) = self.call_with(EXPORT, request)?;
        self.one(msg_type, reply, carried).map(Connection::received)
    }

    /// What `path` names from `base`: the link itself where `follow` is
    /// false.
    pub fn stat(&mut self, base: Base, path: &str, follow: bool) -> Result<Stat, Refused> {
        let flags = if follow { FOLLOW_LAST } else { 0 };
        let reply = self.path_call(STAT, Request { flags, ..Request::new() }, [base, Base::Root], Paths { first: path, second: &[] }, 0)?;
        Ok(stat_of(&reply))
    }

    /// What the file or directory `fid` names is.
    pub fn fstat(&mut self, fid: Fid) -> Result<Stat, Error> {
        self.stat(Base::Fid(fid), "", false).map_err(|refused| match refused {
            Refused::Error(e) => e,
            // An id names one node, which nothing follows from.
            Refused::Follow(_) => Error::Malformed,
        })
    }

    /// Make `change` to what `path` names from `base`, following a link in
    /// its last component where `follow`. A size is changed only through an
    /// id opened to write, with the empty path.
    pub fn set_attr(&mut self, base: Base, path: &str, follow: bool, change: &Change) -> Result<(), Refused> {
        let mut request = Request { flags: if follow { FOLLOW_LAST } else { 0 }, ..Request::new() };
        change.encode(&mut request);
        self.path_call(SETATTR, request, [base, Base::Root], Paths { first: path, second: &[] }, 0).map(drop)
    }

    /// Set the size of the file `fid` opened to write.
    pub fn truncate(&mut self, fid: Fid, size: u64) -> Result<(), Error> {
        let change = Change { size: Some(size), ..Change::default() };
        self.set_attr(Base::Fid(fid), "", false, &change).map_err(|refused| match refused {
            Refused::Error(e) => e,
            Refused::Follow(_) => Error::Malformed,
        })
    }

    /// Make what this file was written durable on the volume.
    pub fn fsync(&mut self, fid: Fid) -> Result<(), Error> {
        self.fid_call(FSYNC, fid, Request::new(), 0).map(drop)
    }

    /// Make the whole volume durable. It names no path, and so is asked again
    /// once on a fresh connection.
    pub fn sync(&mut self) -> Result<(), Error> {
        self.unpathed(SYNC).map(drop)
    }

    /// The volume's [`Room`]. Like [`Self::sync`], it names no path.
    pub fn room(&mut self) -> Result<Room, Error> {
        let reply = self.unpathed(ROOM)?;
        match reply.value2 {
            0 => Err(Error::Malformed),
            unit => Ok(Room { free: reply.value, unit }),
        }
    }

    fn unpathed(&mut self, op: u32) -> Result<Reply, Error> {
        self.path_call(op, Request::new(), [Base::Root; 2], Paths { first: "", second: &[] }, 0).map_err(|refused| match refused {
            Refused::Error(e) => e,
            Refused::Follow(_) => Error::Malformed,
        })
    }

    /// A pipe whose bytes the server writes to this file at the description's
    /// offset, which they move, for a program's standard output: the write end.
    pub fn stream(&mut self, fid: Fid) -> Result<Pipe, Error> {
        let mut request = Request::new();
        request.fid = self.held(fid)?;
        let (msg_type, reply, carried) = self.call_with(STREAM, request)?;
        self.one(msg_type, reply, carried).map(Pipe)
    }

    /// The bytes of the file `fid` opened, as [`IMAGE`] answers them.
    pub fn image(&mut self, fid: Fid) -> Result<crate::OwnedHandle, Error> {
        let mut request = Request::new();
        request.fid = self.held(fid)?;
        let (msg_type, reply, carried) = self.call_with(IMAGE, request)?;
        self.one(msg_type, reply, carried)
    }

    /// The entries of the directory `dir` opened, from `cursor` (0 at its
    /// start), [`for_each_entry`]'s encoding, at most `room` bytes and at
    /// most the window of them, into `out`: the cursor to ask from next,
    /// `None` after the last. A first entry larger than `room` is refused
    /// [`Error::LimitExceeded`].
    pub fn read_dir(&mut self, dir: Fid, cursor: u64, room: usize, out: &mut Vec<u8>) -> Result<Option<u64>, Error> {
        let room = room.min(WINDOW_BYTES);
        let reply = self.fid_call(READDIR, dir, Request { offset: cursor, ..Request::new() }, room)?;
        let n = counted(reply.value, room)?;
        let next = next_batch(n, reply.value2)?;
        out.clear();
        out.resize(n, 0);
        self.window().read(0, out);
        Ok(next)
    }

    pub fn mkdir(&mut self, base: Base, path: &str) -> Result<(), Refused> {
        self.path_call(MKDIR, Request::new(), [base, Base::Root], Paths { first: path, second: &[] }, 0).map(drop)
    }

    /// Remove the name `path`: a directory's only where `dir`, which removes
    /// nothing else.
    pub fn unlink(&mut self, base: Base, path: &str, dir: bool) -> Result<(), Refused> {
        let flags = if dir { UNLINK_DIR } else { 0 };
        self.path_call(UNLINK, Request { flags, ..Request::new() }, [base, Base::Root], Paths { first: path, second: &[] }, 0).map(drop)
    }

    pub fn rename(&mut self, from: (Base, &str), to: (Base, &str)) -> Result<(), Refused> {
        if !relative(to.1) {
            return Err(Refused::Error(Error::InvalidPath));
        }
        self.path_call(RENAME, Request::new(), [from.0, to.0], Paths { first: from.1, second: to.1.as_bytes() }, 0).map(drop)
    }

    /// Make `link` a symlink holding `target`, which is stored as written and
    /// resolved by whoever follows it.
    pub fn symlink(&mut self, target: &str, link: (Base, &str)) -> Result<(), Refused> {
        if target.is_empty() {
            return Err(Refused::Error(Error::InvalidRequest));
        }
        if target.len() > MAX_PATH {
            return Err(Refused::Error(Error::NameTooLong));
        }
        self.path_call(SYMLINK, Request::new(), [link.0, Base::Root], Paths { first: link.1, second: target.as_bytes() }, 0).map(drop)
    }

    /// What the symlink at `path` holds, into `out`.
    pub fn read_link(&mut self, base: Base, path: &str, out: &mut [u8; MAX_PATH]) -> Result<usize, Refused> {
        let reply = self.path_call(READLINK, Request::new(), [base, Base::Root], Paths { first: path, second: &[] }, MAX_PATH)?;
        let n = counted(reply.value, MAX_PATH)?;
        self.window().read(0, &mut out[..n]);
        Ok(n)
    }

    /// Give what `from` names, following a link in its last component where
    /// `follow`, the name `to` too. With `replace`, `to` is taken from the
    /// node of that open file, and only if it still names it: [`Error::Stale`]
    /// when it no longer does.
    pub fn link(&mut self, from: (Base, &str), follow: bool, to: (Base, &str), replace: Option<Fid>) -> Result<(), Refused> {
        if !relative(to.1) {
            return Err(Refused::Error(Error::InvalidPath));
        }
        let mut request = Request { flags: if follow { FOLLOW_LAST } else { 0 }, ..Request::new() };
        if let Some(expected) = replace {
            request.flags |= LINK_REPLACE;
            request.expected = self.held(expected)?;
        }
        self.path_call(LINK, request, [from.0, to.0], Paths { first: from.1, second: to.1.as_bytes() }, 0).map(drop)
    }
}

/// A path request's reply frame: its reply, an absolute link to resolve again
/// from the span's first `span` bytes, or the refusal it names.
fn path_answer(msg_type: u32, reply: Reply, span: usize) -> Result<Reply, Refused> {
    match msg_type {
        FOLLOW => Err(Refused::Follow(counted(reply.value, MAX_PATH.min(span))?)),
        _ => answer(msg_type, reply).map_err(Refused::Error),
    }
}

/// A reply frame that carries no link: the reply, or the refusal it names.
fn answer(msg_type: u32, reply: Reply) -> Result<Reply, Error> {
    match msg_type {
        REPLY => reply.result(),
        _ => Err(Error::Malformed),
    }
}

/// Where a listing goes on after a batch of `bytes` that names `next`: `None`
/// at its end. An empty batch that is not the end is [`Error::Malformed`],
/// since asking from its cursor again would answer it again.
fn next_batch(bytes: usize, next: u64) -> Result<Option<u64>, Error> {
    match (bytes, next) {
        (_, READDIR_END) => Ok(None),
        (0, _) => Err(Error::Malformed),
        (_, next) => Ok(Some(next)),
    }
}

/// A count of bytes a reply says it put in the window, which is at most what
/// was asked, `most`.
fn counted(value: u64, most: usize) -> Result<usize, Error> {
    usize::try_from(value).ok().filter(|&n| n <= most).ok_or(Error::Malformed)
}

/// One entry of a listing [`Dir::read_dir`] answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub kind: u64,
    pub size: u64,
    /// What [`Dir::stat`] answers as the entry's `node`, without following it.
    pub node: u64,
    pub name: &'a str,
}

/// An entry's kind byte, size, node, and its name's length before the name.
const ENTRY_HEAD: usize = 19;

/// The bytes [`encode_entry`] takes for `name`'s entry.
pub const fn entry_bytes(name: &str) -> usize {
    ENTRY_HEAD + name.len()
}

/// Each entry of a listing [`Dir::read_dir`] answered.
pub fn for_each_entry(listing: &[u8], mut f: impl FnMut(Entry<'_>)) -> Result<(), Error> {
    let mut at = 0;
    while at < listing.len() {
        let head = listing.get(at..at + ENTRY_HEAD).ok_or(Error::Malformed)?;
        let word = |at: usize| u64::from_le_bytes(head[at..at + 8].try_into().expect("eight bytes"));
        let (kind, size, node) = (head[0] as u64, word(1), word(9));
        let len = u16::from_le_bytes([head[17], head[18]]) as usize;
        let name = listing.get(at + ENTRY_HEAD..at + ENTRY_HEAD + len).ok_or(Error::Malformed)?;
        let name = core::str::from_utf8(name).map_err(|_| Error::Malformed)?;
        f(Entry { kind, size, node, name });
        at += ENTRY_HEAD + len;
    }
    Ok(())
}

/// One listing entry's encoding, appended to `out` for the server: `None` for
/// a name longer than the encoding holds.
pub fn encode_entry(out: &mut Vec<u8>, entry: &Entry<'_>) -> Option<()> {
    let len = u16::try_from(entry.name.len()).ok()?;
    out.push(entry.kind as u8);
    out.extend_from_slice(&entry.size.to_le_bytes());
    out.extend_from_slice(&entry.node.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(entry.name.as_bytes());
    Some(())
}

/// What a server answered a node is, its volume marked a server's.
fn stat_of(reply: &Reply) -> Stat {
    Stat {
        kind: reply.kind,
        size: reply.size,
        nlink: reply.nlink,
        atime: reply.atime,
        mtime: reply.mtime,
        ctime: reply.ctime,
        volume: reply.volume | SERVED_VOLUME,
        node: reply.node,
    }
}

/// Lend `window` to the server at the other end of `conn` ([`HELLO`]), and
/// answer what it granted, or why it refused.
///
/// A server that refuses a connection as it takes it answers before it lets
/// go, so a hello that finds it gone still reads why.
fn hello(conn: &Connection, window: &SharedMemory) -> Result<Reply, Error> {
    // The window travels in the HELLO frame itself, as one message.
    let sent = window
        .share()
        .map_err(|e| Error::kernel("a window's loan", e))
        .and_then(|lent| conn.send_with(HELLO, &Request::new(), [lent]).map_err(transport));
    match sent {
        Ok(()) | Err(Error::Gone) => {}
        Err(e) => return Err(e),
    }
    let mut rx = FrameRx::<{ core::mem::size_of::<Reply>() }>::new();
    let frame = rx.recv(conn).map_err(transport)?;
    let reply = crate::ipc::decode_payload(rx.payload(frame.payload_len)).map_err(transport)?;
    answer(frame.msg_type, reply)
}

/// A connection's transport refused, as the protocol names it.
fn transport(e: IpcError) -> Error {
    match e {
        IpcError::Disconnected => Error::Gone,
        IpcError::Malformed => Error::Malformed,
        IpcError::Syscall(e) => Error::kernel("a frame", e),
        IpcError::TooLarge => unreachable!("every frame of this protocol is one fixed payload"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path from a directory may climb, which its server resolves; it never
    /// has an empty or `.` component, and a grant's root climbs nowhere.
    #[test]
    fn a_wire_path_may_climb_and_a_grant_root_never_does() {
        for good in ["", "a", "a/b", "toy/Documents/x.txt", "..a", "a..", ".x", "..", "../x", "a/../b"] {
            assert!(relative(good), "{good:?}");
        }
        for bad in ["/a", "a/", "a//b", ".", "a/./b", "./x"] {
            assert!(!relative(bad), "{bad:?}");
            assert!(!canonical(bad), "{bad:?}");
        }
        for climbs in ["..", "../x", "a/../b", "a/.."] {
            assert!(!canonical(climbs), "{climbs:?}");
        }
        for good in ["", "a", "a/b", "..a", "a.."] {
            assert!(canonical(good), "{good:?}");
        }
        assert!(!relative(&"a".repeat(MAX_PATH + 1)));
        assert!(relative(&"a".repeat(MAX_PATH)));
    }

    /// What a client puts on the wire for a path from a directory: every empty
    /// and `.` component gone, every `..` where it was, and nothing longer
    /// than a path.
    #[test]
    fn a_path_from_a_directory_keeps_its_climbs_and_drops_its_dots() {
        assert_eq!(wire_path("a//./b/").as_deref(), Some("a/b"));
        assert_eq!(wire_path("./../x/./..").as_deref(), Some("../x/.."));
        assert_eq!(wire_path("").as_deref(), Some(""));
        assert_eq!(wire_path("/././").as_deref(), Some(""));
        for path in ["a//b", "../x", "x/../y/.", "./a"] {
            let wire = wire_path(path).unwrap();
            assert!(relative(&wire), "{path:?} became {wire:?}");
        }
        assert_eq!(wire_path(&"a".repeat(MAX_PATH + 1)), None);
        assert_eq!(wire_path(&format!("./{}", "a".repeat(MAX_PATH))).map(|p| p.len()), Some(MAX_PATH));
    }

    #[test]
    fn a_grant_round_trips_at_every_bound() {
        let longest = "r".repeat(MAX_GRANT_ROOT);
        for (share, root) in [(0, ""), (1, "home"), (u64::MAX, "home/toy/Documents"), (7, longest.as_str())] {
            for access in [Access::ReadOnly, Access::ReadWrite] {
                let grant = Grant { share, access, root };
                let mut out = [0u8; MAX_BADGE];
                let bytes = grant.encode(&mut out).expect("a root a grant carries");
                assert_eq!(Grant::decode(bytes), Some(grant), "{root:?} {access:?}");
            }
        }
    }

    #[test]
    fn no_grant_carries_a_root_the_wire_refuses_or_one_past_the_badge() {
        let mut out = [0u8; MAX_BADGE];
        let past = "r".repeat(MAX_GRANT_ROOT + 1);
        for root in ["/home", "home/", "a//b", ".", "a/../b", "..", past.as_str()] {
            assert_eq!(Grant { share: 1, access: Access::ReadWrite, root }.encode(&mut out), None, "{root:?}");
        }
    }

    #[test]
    fn bytes_no_grant_was_encoded_as_are_refused() {
        let mut out = [0u8; MAX_BADGE];
        let good = Grant { share: 3, access: Access::ReadOnly, root: "home" }.encode(&mut out).unwrap().to_vec();
        // Shorter than a version, a share and an access.
        for n in 0..10 {
            assert_eq!(Grant::decode(&good[..n]), None, "{n} bytes");
        }
        // Another version, and the one before this.
        for version in [GRANT_VERSION - 1, GRANT_VERSION + 1] {
            let mut other = good.clone();
            other[0] = version;
            assert_eq!(Grant::decode(&other), None, "version {version}");
        }
        // An access byte that is neither, which is never read as either.
        for access in [2, 0x80, 0xff] {
            let mut other = good.clone();
            other[9] = access;
            assert_eq!(Grant::decode(&other), None, "access {access}");
        }
        // A root the wire refuses, or not UTF-8.
        for root in [&b"/home"[..], b"a/../b", b"..", b"a//b", b"\xff"] {
            let mut bad = good[..10].to_vec();
            bad.extend_from_slice(root);
            assert_eq!(Grant::decode(&bad), None, "{root:?}");
        }
        // One byte past the longest root.
        let mut long = good[..10].to_vec();
        long.extend(core::iter::repeat_n(b'r', MAX_GRANT_ROOT + 1));
        assert_eq!(Grant::decode(&long), None);
    }

    #[test]
    fn a_listing_entry_round_trips_and_a_short_one_is_refused() {
        let entries = [
            Entry { kind: KIND_DIR, size: 42, node: 7, name: "Documents" },
            Entry { kind: KIND_FILE, size: 3, node: u64::MAX, name: "a" },
        ];
        let mut buf = Vec::new();
        for entry in &entries {
            encode_entry(&mut buf, entry).unwrap();
        }
        assert_eq!(buf.len(), entries.iter().map(|e| entry_bytes(e.name)).sum::<usize>());
        let mut seen = Vec::new();
        for_each_entry(&buf, |e| seen.push((e.kind, e.size, e.node, e.name.to_string()))).unwrap();
        assert_eq!(seen, entries.map(|e| (e.kind, e.size, e.node, e.name.to_string())));
        assert_eq!(for_each_entry(&buf[..buf.len() - 1], |_| {}), Err(Error::Malformed));
        let long = "x".repeat(usize::from(u16::MAX) + 1);
        assert_eq!(encode_entry(&mut Vec::new(), &Entry { kind: KIND_FILE, size: 0, node: 1, name: &long }), None);
    }

    /// Every change a client asks of `SETATTR` reads back as itself on the
    /// server's side, and flags that set one time both to a value and to now
    /// are no change at all.
    #[test]
    fn every_change_round_trips_through_its_flags_and_a_time_set_two_ways_is_refused() {
        let times = [None, Some(Time::Now), Some(Time::At(0)), Some(Time::At(1_700_000_000_123_456_789))];
        for size in [None, Some(0), Some(MAX_FILE_BYTES)] {
            for atime in times {
                for mtime in times {
                    let change = Change { size, atime, mtime };
                    let mut request = Request::new();
                    change.encode(&mut request);
                    assert_eq!(request.flags & !SETATTR_KNOWN, 0, "{change:?}");
                    assert_eq!(Change::decode(&request), Some(change), "{change:?}");
                }
            }
        }
        for (set, now) in [(SET_ATIME, SET_ATIME_NOW), (SET_MTIME, SET_MTIME_NOW)] {
            assert_eq!(Change::decode(&Request { flags: set | now, ..Request::new() }), None);
        }
    }

    /// A stat reply carries every word of what the server said a node is,
    /// and the client marks its volume a server's.
    #[test]
    fn a_stat_crosses_the_wire_whole_and_its_volume_is_marked_served() {
        let stat = Stat { kind: KIND_FILE, size: 5, nlink: 2, atime: 6, mtime: 7, ctime: 8, volume: 9, node: 10 };
        assert_eq!(stat_of(&Reply::stat(&stat)), Stat { volume: 9 | SERVED_VOLUME, ..stat });
    }

    /// Every refusal a server sends reads back as itself, and 0 — a reply's
    /// success — is none of them.
    #[test]
    fn every_server_refusal_round_trips_through_its_code() {
        for &e in Error::WIRE {
            let code = e.code().expect("a server's refusal has a code");
            assert_ne!(code, 0, "{e:?}");
            assert_eq!(Error::from_code(code), Some(e));
            assert_eq!(Reply::refused(e).result().err(), Some(e));
        }
        assert!(Reply::ok().result().is_ok());
    }

    /// The client end's own words have no code, so no server's status reads
    /// as one, and a server that tried to send one is a bug that fails loudly.
    #[test]
    fn the_client_s_own_words_never_cross_the_wire() {
        for &e in Error::LOCAL {
            assert_eq!(e.code(), None, "{e:?}");
            assert!(std::panic::catch_unwind(|| Reply::refused(e)).is_err(), "{e:?}");
        }
    }

    /// A status, a frame type or a count no server of this protocol sends is
    /// refused by name, never read as another answer.
    #[test]
    fn a_reply_outside_the_protocol_is_malformed() {
        let past = Error::WIRE.iter().filter_map(|e| e.code()).max().expect("wire words") + 1;
        // 29 to 31 are what the client's own words once were, and 0x80 on what
        // their discriminants are.
        for status in [past, 29, 30, 31, 0x80, 0x81, 0x82, u64::MAX, SyscallError::NotFound.to_u64()] {
            assert_eq!(Error::from_code(status), None, "{status:#x}");
            assert_eq!(Reply { status, ..Reply::ok() }.result().err(), Some(Error::Malformed), "{status:#x}");
        }
        for msg_type in [0, HELLO, OPEN, FOLLOW + 1, u32::MAX] {
            assert_eq!(answer(msg_type, Reply::ok()).err(), Some(Error::Malformed), "frame {msg_type:#x}");
            assert_eq!(path_answer(msg_type, Reply::ok(), MAX_PATH).err(), Some(Refused::Error(Error::Malformed)), "frame {msg_type:#x}");
        }
        assert_eq!(answer(FOLLOW, Reply::ok()).err(), Some(Error::Malformed), "a link to follow answers only a path");
        let follow = |value| Reply { value, ..Reply::ok() };
        assert_eq!(path_answer(FOLLOW, follow(MAX_PATH as u64), MAX_PATH).err(), Some(Refused::Follow(MAX_PATH)));
        assert_eq!(path_answer(FOLLOW, follow(MAX_PATH as u64 + 1), 2 * MAX_PATH).err(), Some(Refused::Error(Error::Malformed)));
        assert_eq!(path_answer(FOLLOW, follow(9), 8).err(), Some(Refused::Error(Error::Malformed)), "a link past the span");
        assert_eq!(path_answer(REPLY, Reply::refused(Error::Loop), MAX_PATH).err(), Some(Refused::Error(Error::Loop)));
        assert_eq!(next_batch(0, READDIR_END), Ok(None));
        assert_eq!(next_batch(19, READDIR_END), Ok(None));
        assert_eq!(next_batch(19, 5), Ok(Some(5)));
        assert_eq!(next_batch(0, 5), Err(Error::Malformed), "an empty batch that is not the end");
        assert_eq!(counted(7, 7), Ok(7));
        assert_eq!(counted(8, 7), Err(Error::Malformed), "more bytes than were asked for");
    }

    /// Each word the kernel answers a call this protocol makes is one named
    /// refusal; the rest are words those calls never get, and fail loudly.
    #[test]
    fn every_kernel_refusal_the_calls_can_see_is_one_named_error() {
        assert_eq!(Error::kernel("a call", SyscallError::Gone), Error::Gone);
        assert_eq!(Error::kernel("a call", SyscallError::ResourceExhausted), Error::OutOfResources);
        assert_eq!(transport(IpcError::Disconnected), Error::Gone);
        assert_eq!(transport(IpcError::Syscall(SyscallError::Gone)), Error::Gone);
        assert_eq!(transport(IpcError::Syscall(SyscallError::ResourceExhausted)), Error::OutOfResources);
        assert_eq!(transport(IpcError::Malformed), Error::Malformed);
        for never in [
            SyscallError::Unknown,
            SyscallError::NotFound,
            SyscallError::PermissionDenied,
            SyscallError::AlreadyExists,
            SyscallError::InvalidArgument,
            SyscallError::BadAddress,
            SyscallError::WouldBlock,
            SyscallError::NotSupported,
            SyscallError::Io,
        ] {
            assert!(std::panic::catch_unwind(|| Error::kernel("a call", never)).is_err(), "{never:?}");
        }
    }
}
