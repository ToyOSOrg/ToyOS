//! The `sftp` subsystem, version 3, in the profile this machine needs: open,
//! read, write, close, stat and the directory listing, over `std::fs`.
//!
//! **The wire format is draft-ietf-secsh-filexfer-02** — the version every
//! deployed client speaks — and this file implements a subset of it. A request
//! outside that subset is answered `SSH_FX_OP_UNSUPPORTED` with its own name in
//! the message, never guessed at and never silently succeeded: a `SETSTAT` that
//! answered `OK` without setting anything would make a client that checks its
//! own work believe a mode it never got.
//!
//! **Nothing here is a policy boundary.** Paths are ambient on this system, and
//! a client that authenticated holds the machine — `/boot`'s mount guard is the
//! one restriction the path space carries and the kernel applies it. So this
//! server refuses nothing on the grounds of *where* a path points; every
//! refusal below is about a request it cannot carry out.
//!
//! Every bound in here is named where it is spent, because a client on the
//! other side of a network is the one choosing the numbers.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::UNIX_EPOCH;

/// The protocol version this server speaks, and the only one it will speak.
const VERSION: u32 = 3;

/// The largest request this server will assemble before acting on it. A
/// `WRITE` carries its payload inline, so this is the write-chunk ceiling too.
pub const MAX_PACKET: usize = 256 * 1024;

/// The largest `DATA` reply, whatever a `READ` asks for.
const MAX_READ: usize = 64 * 1024;

/// How much of a directory one `NAME` reply carries. The listing continues on
/// the next `READDIR`, which is what the protocol's cursor is for.
const NAMES_PER_REPLY: usize = 64;

/// How many entries a directory may have before this server refuses to list it.
/// It reads the whole listing at `OPENDIR` so the cursor cannot be invalidated
/// underneath it, and that read has to be bounded by something.
const MAX_DIR_ENTRIES: usize = 8192;

// Request types.
const FXP_INIT: u8 = 1;
const FXP_VERSION: u8 = 2;
const FXP_OPEN: u8 = 3;
const FXP_CLOSE: u8 = 4;
const FXP_READ: u8 = 5;
const FXP_WRITE: u8 = 6;
const FXP_LSTAT: u8 = 7;
const FXP_FSTAT: u8 = 8;
const FXP_OPENDIR: u8 = 11;
const FXP_READDIR: u8 = 12;
const FXP_REALPATH: u8 = 16;
const FXP_STAT: u8 = 17;

// Response types.
const FXP_STATUS: u8 = 101;
const FXP_HANDLE: u8 = 102;
const FXP_DATA: u8 = 103;
const FXP_NAME: u8 = 104;
const FXP_ATTRS: u8 = 105;

// Status codes.
const FX_OK: u32 = 0;
const FX_EOF: u32 = 1;
const FX_NO_SUCH_FILE: u32 = 2;
const FX_PERMISSION_DENIED: u32 = 3;
const FX_FAILURE: u32 = 4;
const FX_BAD_MESSAGE: u32 = 5;
const FX_OP_UNSUPPORTED: u32 = 8;

/// The exit status the channel carries when a session ends on a packet this
/// protocol has no reply for: the draft's own code for it. **Not 127** — that
/// one says this daemon would not run what was asked, and an SFTP session that
/// got this far ran.
pub const EXIT_BAD_MESSAGE: u32 = FX_BAD_MESSAGE;

// `OPEN` flags.
const OPEN_READ: u32 = 0x0000_0001;
const OPEN_WRITE: u32 = 0x0000_0002;
const OPEN_APPEND: u32 = 0x0000_0004;
const OPEN_CREAT: u32 = 0x0000_0008;
const OPEN_TRUNC: u32 = 0x0000_0010;
const OPEN_EXCL: u32 = 0x0000_0020;

// Attribute flags.
const ATTR_SIZE: u32 = 0x0000_0001;
const ATTR_PERMISSIONS: u32 = 0x0000_0004;
const ATTR_ACMODTIME: u32 = 0x0000_0008;

/// What a request names by number rather than by name, for a refusal that says
/// something. Only the subset this server does not implement is here — the ones
/// it does implement are matched on their constants.
fn request_name(kind: u8) -> &'static str {
    match kind {
        9 => "SETSTAT",
        10 => "FSETSTAT",
        13 => "REMOVE",
        14 => "MKDIR",
        15 => "RMDIR",
        18 => "RENAME",
        19 => "READLINK",
        20 => "SYMLINK",
        200 => "EXTENDED",
        _ => "an unknown request",
    }
}

/// A condition the session cannot continue past: a client that sent one of
/// these is not speaking this protocol, and answering it further would be
/// guessing.
#[derive(Debug)]
pub struct Fatal(pub String);

impl Fatal {
    /// Say it on the console and hand back the status the channel closes with.
    pub fn say(&self, peer: &str) -> u32 {
        println!("sshd: sftp for {peer}: {}", self.0);
        EXIT_BAD_MESSAGE
    }
}

/// A field this packet does not have, or has and cannot be read.
#[derive(Debug)]
struct Malformed(&'static str);

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, at: 0 }
    }

    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], Malformed> {
        let end = self.at.checked_add(n).ok_or(Malformed(what))?;
        let slice = self.bytes.get(self.at..end).ok_or(Malformed(what))?;
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self, what: &'static str) -> Result<u8, Malformed> {
        Ok(self.take(1, what)?[0])
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, Malformed> {
        let b = self.take(4, what)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self, what: &'static str) -> Result<u64, Malformed> {
        let b = self.take(8, what)?;
        Ok(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    fn bytes(&mut self, what: &'static str) -> Result<&'a [u8], Malformed> {
        let len = self.u32(what)? as usize;
        self.take(len, what)
    }

    /// A protocol string, which is a byte string; this system's paths are UTF-8
    /// and one that is not is refused here rather than lossily converted.
    fn string(&mut self, what: &'static str) -> Result<&'a str, Malformed> {
        std::str::from_utf8(self.bytes(what)?).map_err(|_| Malformed(what))
    }
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }

    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }

    fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v);
    }

    fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }

    /// The framed packet: the length prefix counts everything after itself.
    fn frame(kind: u8, body: impl FnOnce(&mut Writer)) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(0);
        w.u8(kind);
        body(&mut w);
        let len = (w.0.len() - 4) as u32;
        w.0[..4].copy_from_slice(&len.to_be_bytes());
        w.0
    }
}

/// What one entry of a listing carries, read once at `OPENDIR` so the cursor
/// cannot be invalidated by anything that happens to the directory after it.
struct Entry {
    name: String,
    attrs: Attrs,
}

#[derive(Clone, Copy)]
struct Attrs {
    size: u64,
    dir: bool,
    mtime: u32,
}

impl Attrs {
    /// What this system knows. There are no per-file permissions and no owner,
    /// so the mode is the file's *kind* and a fixed set of bits — said once,
    /// here, rather than invented at three call sites.
    fn of(meta: &std::fs::Metadata) -> Self {
        Attrs {
            size: meta.len(),
            dir: meta.is_dir(),
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs().min(u32::MAX as u64) as u32),
        }
    }

    fn mode(&self) -> u32 {
        if self.dir { 0o040755 } else { 0o100644 }
    }

    fn write(&self, w: &mut Writer) {
        w.u32(ATTR_SIZE | ATTR_PERMISSIONS | ATTR_ACMODTIME);
        w.u64(self.size);
        w.u32(self.mode());
        w.u32(self.mtime);
        w.u32(self.mtime);
    }

    /// The `ls -l` line version 3 makes every server produce beside the name.
    fn longname(&self, name: &str) -> String {
        let kind = if self.dir { 'd' } else { '-' };
        let perms = if self.dir { "rwxr-xr-x" } else { "rw-r--r--" };
        format!("{kind}{perms} 1 root root {:>10} {name}", self.size)
    }
}

enum Handle {
    File(File),
    Dir { entries: Vec<Entry>, at: usize },
}

pub struct Server {
    open: BTreeMap<String, Handle>,
    next: u64,
    /// The version the client asked for, and the proof `INIT` came first.
    negotiated: bool,
}

impl Default for Server {
    fn default() -> Self {
        Server::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Server { open: BTreeMap::new(), next: 0, negotiated: false }
    }

    /// One whole request in, one whole reply out.
    ///
    /// `Fatal` ends the session: the packet was not a request this protocol
    /// has a reply for, so there is nobody to answer.
    pub fn request(&mut self, packet: &[u8]) -> Result<Vec<u8>, Fatal> {
        let mut r = Reader::new(packet);
        let kind = r.u8("the request type").map_err(|m| Fatal(format!("a packet with no {}", m.0)))?;

        if kind == FXP_INIT {
            if self.negotiated {
                return Err(Fatal("a second SSH_FXP_INIT on one session".to_string()));
            }
            // The client's own version is read and deliberately not honoured
            // beyond the answer: this server speaks 3, and a client that asked
            // for more takes 3 or leaves. That is the protocol's own rule.
            let _ = r.u32("the client's version");
            self.negotiated = true;
            return Ok(Writer::frame(FXP_VERSION, |w| w.u32(VERSION)));
        }
        if !self.negotiated {
            return Err(Fatal(format!("request type {kind} before SSH_FXP_INIT")));
        }

        let id = match r.u32("the request id") {
            Ok(id) => id,
            Err(m) => return Err(Fatal(format!("a request with no {}", m.0))),
        };

        match self.dispatch(kind, id, &mut r) {
            Ok(reply) => Ok(reply),
            Err(Malformed(what)) => Ok(status(id, FX_BAD_MESSAGE, &format!("no {what} in the request"))),
        }
    }

    fn dispatch(&mut self, kind: u8, id: u32, r: &mut Reader) -> Result<Vec<u8>, Malformed> {
        Ok(match kind {
            FXP_REALPATH => {
                let path = r.string("a path")?;
                // No working directory and no symlinks, so the canonical form
                // of a relative path is what the filesystem's own root makes
                // of it. `canonicalize` answers only for a path that exists;
                // a client asking about one that does not gets the name back,
                // which is what it needs to create the file there.
                let resolved = std::fs::canonicalize(path)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| path.to_string());
                let attrs = Attrs { size: 0, dir: true, mtime: 0 };
                Writer::frame(FXP_NAME, |w| {
                    w.u32(id);
                    w.u32(1);
                    w.str(&resolved);
                    w.str(&attrs.longname(&resolved));
                    attrs.write(w);
                })
            }
            FXP_OPEN => {
                let path = r.string("a path")?.to_string();
                let flags = r.u32("the open flags")?;
                self.open_file(id, &path, flags)
            }
            FXP_OPENDIR => {
                let path = r.string("a path")?.to_string();
                self.open_dir(id, &path)
            }
            FXP_CLOSE => {
                let handle = r.string("a handle")?.to_string();
                match self.open.remove(&handle) {
                    Some(_) => status(id, FX_OK, "closed"),
                    None => status(id, FX_FAILURE, "no such open handle"),
                }
            }
            FXP_READ => {
                let handle = r.string("a handle")?.to_string();
                let offset = r.u64("an offset")?;
                let len = r.u32("a length")?;
                self.read(id, &handle, offset, len)
            }
            FXP_WRITE => {
                let handle = r.string("a handle")?.to_string();
                let offset = r.u64("an offset")?;
                let data = r.bytes("the data")?.to_vec();
                self.write(id, &handle, offset, &data)
            }
            FXP_READDIR => {
                let handle = r.string("a handle")?.to_string();
                self.readdir(id, &handle)
            }
            FXP_STAT | FXP_LSTAT => {
                let path = r.string("a path")?;
                // No symlinks on this system, so the two questions have one
                // answer; `symlink_metadata` is the one that is right either
                // way and is used for both rather than pretending otherwise.
                match std::fs::symlink_metadata(path) {
                    Ok(meta) => attrs_reply(id, Attrs::of(&meta)),
                    Err(e) => io_status(id, &e, "stat"),
                }
            }
            FXP_FSTAT => {
                let handle = r.string("a handle")?.to_string();
                match self.open.get(&handle) {
                    Some(Handle::File(f)) => match f.metadata() {
                        Ok(meta) => attrs_reply(id, Attrs::of(&meta)),
                        Err(e) => io_status(id, &e, "fstat"),
                    },
                    Some(Handle::Dir { .. }) => {
                        status(id, FX_FAILURE, "that handle is a directory")
                    }
                    None => status(id, FX_FAILURE, "no such open handle"),
                }
            }
            other => status(
                id,
                FX_OP_UNSUPPORTED,
                &format!(
                    "sshd's sftp serves open, read, write, close, stat and the directory \
                     listing; {} is not one of them",
                    request_name(other)
                ),
            ),
        })
    }

    fn handle_name(&mut self, tag: char) -> String {
        self.next += 1;
        format!("{tag}{}", self.next)
    }

    fn open_file(&mut self, id: u32, path: &str, flags: u32) -> Vec<u8> {
        let mut options = OpenOptions::new();
        options
            .read(flags & OPEN_READ != 0)
            .write(flags & OPEN_WRITE != 0)
            .append(flags & OPEN_APPEND != 0)
            .truncate(flags & OPEN_TRUNC != 0);
        if flags & OPEN_EXCL != 0 {
            options.create_new(true);
        } else if flags & OPEN_CREAT != 0 {
            options.create(true);
        }
        // A request that asked for neither read nor write is not a mode this
        // filesystem has; refusing it by name beats opening for read and
        // letting the first `WRITE` fail for a reason that names nothing.
        if flags & (OPEN_READ | OPEN_WRITE | OPEN_APPEND) == 0 {
            return status(id, FX_BAD_MESSAGE, "an open for neither reading nor writing");
        }
        match options.open(path) {
            Ok(file) => {
                let name = self.handle_name('f');
                self.open.insert(name.clone(), Handle::File(file));
                Writer::frame(FXP_HANDLE, |w| {
                    w.u32(id);
                    w.str(&name);
                })
            }
            Err(e) => io_status(id, &e, "open"),
        }
    }

    fn open_dir(&mut self, id: u32, path: &str) -> Vec<u8> {
        let listing = match std::fs::read_dir(path) {
            Ok(listing) => listing,
            Err(e) => return io_status(id, &e, "opendir"),
        };
        let mut entries = Vec::new();
        for entry in listing {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => return io_status(id, &e, "readdir"),
            };
            if entries.len() == MAX_DIR_ENTRIES {
                return status(
                    id,
                    FX_FAILURE,
                    &format!("this directory has more than {MAX_DIR_ENTRIES} entries"),
                );
            }
            let attrs = entry.metadata().map_or(
                Attrs { size: 0, dir: false, mtime: 0 },
                |meta| Attrs::of(&meta),
            );
            entries.push(Entry { name: entry.file_name().to_string_lossy().into_owned(), attrs });
        }
        let name = self.handle_name('d');
        self.open.insert(name.clone(), Handle::Dir { entries, at: 0 });
        Writer::frame(FXP_HANDLE, |w| {
            w.u32(id);
            w.str(&name);
        })
    }

    fn read(&mut self, id: u32, handle: &str, offset: u64, len: u32) -> Vec<u8> {
        let Some(Handle::File(file)) = self.open.get_mut(handle) else {
            return status(id, FX_FAILURE, "no such open file handle");
        };
        if let Err(e) = file.seek(SeekFrom::Start(offset)) {
            return io_status(id, &e, "seek");
        }
        let want = (len as usize).min(MAX_READ);
        let mut buf = vec![0u8; want];
        let mut got = 0;
        while got < want {
            match file.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return io_status(id, &e, "read"),
            }
        }
        if got == 0 {
            // Version 3 says end of file is a status and not an empty `DATA`.
            return status(id, FX_EOF, "end of file");
        }
        buf.truncate(got);
        Writer::frame(FXP_DATA, |w| {
            w.u32(id);
            w.bytes(&buf);
        })
    }

    fn write(&mut self, id: u32, handle: &str, offset: u64, data: &[u8]) -> Vec<u8> {
        let Some(Handle::File(file)) = self.open.get_mut(handle) else {
            return status(id, FX_FAILURE, "no such open file handle");
        };
        if let Err(e) = file.seek(SeekFrom::Start(offset)) {
            return io_status(id, &e, "seek");
        }
        match file.write_all(data) {
            Ok(()) => status(id, FX_OK, "written"),
            Err(e) => io_status(id, &e, "write"),
        }
    }

    fn readdir(&mut self, id: u32, handle: &str) -> Vec<u8> {
        let Some(Handle::Dir { entries, at }) = self.open.get_mut(handle) else {
            return status(id, FX_FAILURE, "no such open directory handle");
        };
        if *at >= entries.len() {
            return status(id, FX_EOF, "end of the listing");
        }
        let end = (*at + NAMES_PER_REPLY).min(entries.len());
        let batch = &entries[*at..end];
        *at = end;
        Writer::frame(FXP_NAME, |w| {
            w.u32(id);
            w.u32(batch.len() as u32);
            for entry in batch {
                w.str(&entry.name);
                w.str(&entry.attrs.longname(&entry.name));
                entry.attrs.write(w);
            }
        })
    }
}

fn status(id: u32, code: u32, message: &str) -> Vec<u8> {
    Writer::frame(FXP_STATUS, |w| {
        w.u32(id);
        w.u32(code);
        w.str(message);
        w.str("en");
    })
}

fn attrs_reply(id: u32, attrs: Attrs) -> Vec<u8> {
    Writer::frame(FXP_ATTRS, |w| {
        w.u32(id);
        attrs.write(w);
    })
}

/// The filesystem's refusal, in the protocol's vocabulary and in words. The
/// code is what a client branches on; the message is what a person reads, and
/// it names the operation because a bare "not found" says nothing about which
/// of a transfer's dozen requests failed.
fn io_status(id: u32, e: &std::io::Error, op: &str) -> Vec<u8> {
    let code = match e.kind() {
        std::io::ErrorKind::NotFound => FX_NO_SUCH_FILE,
        std::io::ErrorKind::PermissionDenied => FX_PERMISSION_DENIED,
        _ => FX_FAILURE,
    };
    status(id, code, &format!("{op}: {e}"))
}

/// The frame in front of every request: a length and then that many bytes.
///
/// `Ok(None)` is "not yet whole" — the caller keeps reading. Nothing acts on a
/// partial packet, which is the server doctrine's own rule and here also the
/// difference between a `WRITE`'s payload and a truncated one.
pub fn next_packet(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>, Fatal> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len == 0 {
        return Err(Fatal("a zero-length sftp packet".to_string()));
    }
    if len > MAX_PACKET {
        return Err(Fatal(format!(
            "an sftp packet of {len} bytes, over the {MAX_PACKET}-byte ceiling"
        )));
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let packet = buf[4..4 + len].to_vec();
    buf.drain(..4 + len);
    Ok(Some(packet))
}

/// The wire, against the draft this file implements.
#[cfg(test)]
mod tests {
    use super::*;

    /// A framed request, as a client sends it.
    fn packet(kind: u8, body: impl FnOnce(&mut Writer)) -> Vec<u8> {
        let framed = Writer::frame(kind, body);
        framed[4..].to_vec()
    }

    fn init(server: &mut Server) {
        let reply = server.request(&packet(FXP_INIT, |w| w.u32(3))).unwrap_or_else(|e| panic!("{}", e.0));
        assert_eq!(reply[4], FXP_VERSION);
        assert_eq!(&reply[5..9], &3u32.to_be_bytes());
    }

    /// The type and the request id of a reply, which is all most assertions
    /// below need.
    fn kind_of(reply: &[u8]) -> u8 {
        reply[4]
    }

    fn status_of(reply: &[u8]) -> u32 {
        assert_eq!(kind_of(reply), FXP_STATUS, "not a status reply");
        u32::from_be_bytes([reply[9], reply[10], reply[11], reply[12]])
    }

    fn handle_of(reply: &[u8]) -> String {
        assert_eq!(kind_of(reply), FXP_HANDLE, "not a handle reply");
        let len = u32::from_be_bytes([reply[9], reply[10], reply[11], reply[12]]) as usize;
        String::from_utf8(reply[13..13 + len].to_vec()).expect("utf-8")
    }

    /// The names one `NAME` reply carries, decoded the way a client does.
    fn names_of(reply: &[u8]) -> Vec<String> {
        assert_eq!(kind_of(reply), FXP_NAME, "not a name reply");
        let mut r = Reader::new(&reply[5..]);
        r.u32("the request id").expect("an id");
        let count = r.u32("the count").expect("a count");
        (0..count)
            .map(|_| {
                let name = r.string("a name").expect("a name").to_string();
                r.string("a longname").expect("a longname");
                r.u32("the attribute flags").expect("flags");
                r.u64("the size").expect("a size");
                r.u32("the mode").expect("a mode");
                r.u32("the atime").expect("an atime");
                r.u32("the mtime").expect("an mtime");
                name
            })
            .collect()
    }

    /// A directory of this test's own, emptied first so a previous run cannot
    /// decide what a listing here contains.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("toyos-sftp-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn opendir(server: &mut Server, id: u32, dir: &std::path::Path) -> Vec<u8> {
        server
            .request(&packet(FXP_OPENDIR, |w| {
                w.u32(id);
                w.str(dir.to_str().expect("utf-8"));
            }))
            .unwrap_or_else(|e| panic!("{}", e.0))
    }

    #[test]
    fn a_request_before_init_ends_the_session() {
        let mut server = Server::new();
        let err = server.request(&packet(FXP_STAT, |w| {
            w.u32(1);
            w.str("/");
        }));
        assert!(err.is_err(), "a stat before INIT was answered");
    }

    #[test]
    fn a_second_init_ends_the_session() {
        let mut server = Server::new();
        init(&mut server);
        assert!(server.request(&packet(FXP_INIT, |w| w.u32(3))).is_err());
    }

    /// The named refusal: every request outside the profile answers
    /// `OP_UNSUPPORTED` and says so, rather than failing somewhere later.
    #[test]
    fn a_request_outside_the_profile_is_refused_by_name() {
        let mut server = Server::new();
        init(&mut server);
        for (kind, name) in [(9u8, "SETSTAT"), (13, "REMOVE"), (14, "MKDIR"), (18, "RENAME")] {
            let reply = server
                .request(&packet(kind, |w| {
                    w.u32(7);
                    w.str("/tmp/x");
                }))
                .unwrap_or_else(|e| panic!("{}", e.0));
            assert_eq!(status_of(&reply), FX_OP_UNSUPPORTED, "{name} was not refused");
            let text = String::from_utf8_lossy(&reply).into_owned();
            assert!(text.contains(name), "the refusal of {name} does not name it: {text}");
        }
    }

    #[test]
    fn a_truncated_request_is_a_bad_message_and_not_a_panic() {
        let mut server = Server::new();
        init(&mut server);
        // A `READ` with a handle and nothing else: no offset, no length.
        let reply = server
            .request(&packet(FXP_READ, |w| {
                w.u32(1);
                w.str("f1");
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        assert_eq!(status_of(&reply), FX_BAD_MESSAGE);
    }

    #[test]
    fn a_handle_nobody_opened_is_refused() {
        let mut server = Server::new();
        init(&mut server);
        let reply = server
            .request(&packet(FXP_CLOSE, |w| {
                w.u32(1);
                w.str("f99");
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        assert_eq!(status_of(&reply), FX_FAILURE);
    }

    #[test]
    fn framing_waits_for_a_whole_packet() {
        let whole = Writer::frame(FXP_INIT, |w| w.u32(3));
        let mut buf: Vec<u8> = Vec::new();
        for byte in &whole[..whole.len() - 1] {
            buf.push(*byte);
            assert!(next_packet(&mut buf).expect("a legal length").is_none());
        }
        buf.push(whole[whole.len() - 1]);
        let got = next_packet(&mut buf).expect("a legal length").expect("the whole packet");
        assert_eq!(got, whole[4..]);
        assert!(buf.is_empty());
    }

    #[test]
    fn an_oversized_length_ends_the_session() {
        let mut buf = ((MAX_PACKET + 1) as u32).to_be_bytes().to_vec();
        assert!(next_packet(&mut buf).is_err());
        let mut zero = 0u32.to_be_bytes().to_vec();
        assert!(next_packet(&mut zero).is_err());
    }

    /// Open, write, read back, stat and list — the whole profile against a real
    /// directory, so the reply encodings are judged by what comes back out.
    #[test]
    fn the_profile_moves_a_file_and_lists_it() {
        let dir = scratch("profile");
        let path = dir.join("payload");
        let path = path.to_str().expect("utf-8");

        let mut server = Server::new();
        init(&mut server);

        let reply = server
            .request(&packet(FXP_OPEN, |w| {
                w.u32(1);
                w.str(path);
                w.u32(OPEN_WRITE | OPEN_CREAT | OPEN_TRUNC);
                w.u32(0);
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        let handle = handle_of(&reply);

        let body = b"the wire is the oracle";
        let reply = server
            .request(&packet(FXP_WRITE, |w| {
                w.u32(2);
                w.str(&handle);
                w.u64(0);
                w.bytes(body);
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        assert_eq!(status_of(&reply), FX_OK);
        server
            .request(&packet(FXP_CLOSE, |w| {
                w.u32(3);
                w.str(&handle);
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        assert_eq!(std::fs::read(path).expect("the file"), body);

        // Stat sees the size the write left.
        let reply = server
            .request(&packet(FXP_STAT, |w| {
                w.u32(4);
                w.str(path);
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        assert_eq!(kind_of(&reply), FXP_ATTRS);
        let size = u64::from_be_bytes([
            reply[13], reply[14], reply[15], reply[16], reply[17], reply[18], reply[19], reply[20],
        ]);
        assert_eq!(size, body.len() as u64);

        // Read it back, and then the end of it.
        let reply = server
            .request(&packet(FXP_OPEN, |w| {
                w.u32(5);
                w.str(path);
                w.u32(OPEN_READ);
                w.u32(0);
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        let handle = handle_of(&reply);
        let reply = server
            .request(&packet(FXP_READ, |w| {
                w.u32(6);
                w.str(&handle);
                w.u64(0);
                w.u32(4096);
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        assert_eq!(kind_of(&reply), FXP_DATA);
        let len = u32::from_be_bytes([reply[9], reply[10], reply[11], reply[12]]) as usize;
        assert_eq!(&reply[13..13 + len], body);
        let reply = server
            .request(&packet(FXP_READ, |w| {
                w.u32(7);
                w.str(&handle);
                w.u64(body.len() as u64);
                w.u32(4096);
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        assert_eq!(status_of(&reply), FX_EOF);

        // The listing names it, and runs out.
        let reply = opendir(&mut server, 8, &dir);
        let dir_handle = handle_of(&reply);
        let reply = server
            .request(&packet(FXP_READDIR, |w| {
                w.u32(9);
                w.str(&dir_handle);
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        assert_eq!(kind_of(&reply), FXP_NAME);
        assert!(String::from_utf8_lossy(&reply).contains("payload"));
        let reply = server
            .request(&packet(FXP_READDIR, |w| {
                w.u32(10);
                w.str(&dir_handle);
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        assert_eq!(status_of(&reply), FX_EOF);

        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    /// The cursor: a listing longer than one reply is handed over in batches
    /// that continue where the last left off, and every name arrives once.
    ///
    /// A cursor that ran to the end of the listing would lose the tail; one
    /// that stayed put would hand a client the first batch forever.
    #[test]
    fn a_listing_longer_than_one_reply_continues_where_it_left_off() {
        let dir = scratch("cursor");
        let want: Vec<String> =
            (0..NAMES_PER_REPLY + 17).map(|n| format!("entry-{n:04}")).collect();
        for name in &want {
            std::fs::write(dir.join(name), b"x").expect("an entry");
        }

        let mut server = Server::new();
        init(&mut server);
        let handle = handle_of(&opendir(&mut server, 1, &dir));

        let read = |server: &mut Server, id: u32| {
            server
                .request(&packet(FXP_READDIR, |w| {
                    w.u32(id);
                    w.str(&handle);
                }))
                .unwrap_or_else(|e| panic!("{}", e.0))
        };

        let first = names_of(&read(&mut server, 2));
        assert_eq!(first.len(), NAMES_PER_REPLY, "the first batch is not a whole reply");
        let second = names_of(&read(&mut server, 3));
        assert_eq!(second.len(), want.len() - NAMES_PER_REPLY, "the tail is the wrong length");
        assert_eq!(status_of(&read(&mut server, 4)), FX_EOF, "the listing did not run out");

        let mut got: Vec<String> = first.into_iter().chain(second).collect();
        got.sort();
        assert_eq!(got, want, "the two batches are not the directory");

        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    /// The ceiling on a listing, refused by name rather than read into memory
    /// on a client's say-so.
    #[test]
    fn a_directory_past_the_ceiling_is_refused_by_name() {
        let dir = scratch("ceiling");
        for n in 0..=MAX_DIR_ENTRIES {
            std::fs::write(dir.join(format!("{n}")), b"").expect("an entry");
        }

        let mut server = Server::new();
        init(&mut server);
        let reply = opendir(&mut server, 1, &dir);
        assert_eq!(status_of(&reply), FX_FAILURE, "an oversized directory opened");
        let text = String::from_utf8_lossy(&reply).into_owned();
        assert!(
            text.contains(&MAX_DIR_ENTRIES.to_string()),
            "the refusal does not name the ceiling: {text}"
        );

        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_file_that_is_not_there_is_no_such_file() {
        let mut server = Server::new();
        init(&mut server);
        let reply = server
            .request(&packet(FXP_STAT, |w| {
                w.u32(1);
                w.str("/nonexistent/nothing-here");
            }))
            .unwrap_or_else(|e| panic!("{}", e.0));
        assert_eq!(status_of(&reply), FX_NO_SUCH_FILE);
    }
}
