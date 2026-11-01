//! fileserver: one role's file server — SYSTEM, DATA, LOG or BOOT — serving the
//! directories of that role as capabilities.
//!
//! **What it holds**: the acceptor of its role's one port, endowed by the
//! supervisor under `serve:fs:<role>`; for its volume, SYSTEM's ROOT image
//! (`ROOT_IMAGE_LABEL`), which needs no disk, or `block` in its namespace, a
//! connector to the block service minted for its role's partition alone —
//! every DATA partition, or the one the loader named, read-only for the boot
//! volume; and nothing else of the machine. Two DATA partitions are refused by
//! name, never guessed between (`fileserver::data::find`). Argv is the role,
//! and for LOG and BOOT the unique GUID of the partition the loader named for
//! it.
//!
//! **SYSTEM is the first program the supervisor starts**, from its boot image
//! and before any file can be read, so it needs no file and no library to
//! serve `/system`.
//!
//! **A connection is what its grant says** (`toyos::fs::Grant`): the badge
//! the supervisor minted its connector with, which the kernel stamped on it and
//! answers this port's acceptor alone. Every path on it is resolved beneath
//! the grant's root (`fileserver::resolve`); a request that would change what
//! it holds, on a read-only grant or a read-only volume, is refused before the
//! volume sees it (`fileserver::rights`).
//!
//! **One share cannot take the server** (`fileserver::bounds`). A share is one
//! service the supervisor started itself or one login session, with every
//! child spawned in it and every launch made from it that opens no session
//! (`toyos_manifest::launch`). Four at their parts take the server
//! (`issues/four-shares-take-a-file-server.md`).
//!
//! **An export is a connection this server makes to itself** through a private
//! port only it holds a connector to, for its life: connect, then accept, which
//! answers exactly that connection since this loop is single-threaded. The end
//! handed out cannot be duplicated, so it has one holder. A restart takes the
//! port with it, so nothing pending reaches a new instance.
//!
//! **A server never blocks on a client.** Accept and the first frame are two
//! events; a request is buffered until whole; every reply is one `try_send`,
//! and a client whose pipe will not take one is dropped by name. The window a
//! client lends is read once per request into this process's memory, and
//! what is acted on is that copy.
//!
//! **What is written reaches the disk at a sync**: a client's `SYNC`, or
//! [`fileserver::writeback::WRITEBACK`] after the first unsynced write when
//! nothing asked sooner; an fsync makes its own file durable, with what the
//! volume's commit carries beside it ([`fileserver::pages::Files::fsync`]). A
//! kill loses what neither covered, which is POSIX's promise and no more.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use fileserver::absent::Absent;
use fileserver::bounds::{self, Hello, Slot, MAX_HANDSHAKES, MAX_SERVED, MAX_STREAMS};
use fileserver::data::{DataVolume, Located, Probed};
use fileserver::descriptions::{self, Description, Held, Ids, Shared};
use fileserver::listing;
use diskserver::disk::Served;
use fileserver::cache::Disk;
use fileserver::ram::Ram;
use fileserver::fat::FatVolume;
use fileserver::resolve::{self, Found, Refusal as Escape, Resolved};
use fileserver::rights;
use fileserver::root::RootVolume;
use fileserver::pages::Files;
use fileserver::volume::{Kind, Meta, Node, OpenHow, Volume};
use fileserver::writeback::WriteBack;
use toyos::endow::{self, Endowments};
use toyos::fs::*;
use toyos::ipc::{self, Connection, RxStep};
use toyos::namespace::Namespace;
use toyos::poller::{Poller, READABLE};
use toyos::port::{self, Acceptor};
use toyos::shm::{Bytes, SharedMemory};
use toyos::Pipe;
use toyos_abi::handle::Rights;
use toyos_abi::syscall::{SyscallError, MAX_BADGE, PIPE_CAPACITY, SERVE_PREFIX};

/// What one turn of a stream appends at most.
const STREAM_READ: usize = 64 * 1024;

/// The turns that drain what one stream holds before a request on its
/// description: the most a pipe holds, so a writer that keeps writing cannot
/// hold the loop.
const STREAM_PRESENT: usize = PIPE_CAPACITY / STREAM_READ;

/// The name the private export port has in this server's own namespace.
const EXPORT_PORT: &str = "export";

/// What the end an export hands out carries: a connection's rights without
/// `DUP`, so whoever holds it is its one holder, and may pass it on.
const EXPORTED: Rights =
    Rights::TRANSFER.union(Rights::WAIT).union(Rights::READ).union(Rights::WRITE).union(Rights::MAP);

// One wait watches the acceptor, every connection and every stream at once.
const _: () = assert!(1 + MAX_SERVED + MAX_HANDSHAKES + MAX_STREAMS <= Poller::MAX_HANDLES as usize);

/// How long an accepted connection may take to lend its window: while
/// [`MAX_HANDSHAKES`] wait, the next waits in the port's queue for at most this.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// Why a client asking for a request number the wire does not have is let go,
/// whatever its grant.
const NO_SUCH_OPERATION: &str = "it asked for an operation this protocol does not have";

/// The volume memory stands in for when DATA has no partition: 1 GiB of
/// blocks, of which only what is written costs anything.
const RAM_BLOCKS: u64 = 1 << 18;

/// What a volume's cache keeps clean: its tree, or its FAT and directories.
/// A file's pages are its paged object's ([`pager`]).
const CLEAN_BLOCKS: usize = (16 << 20) / toyos_blockdev::BLOCK;

const TOKEN_ACCEPTOR: u64 = 0;
const TOKEN_CLIENT: u64 = 1 << 32;
const TOKEN_STREAM: u64 = 2 << 32;
const TOKEN_PAGER: u64 = 3 << 32;

/// Notices read per call.
const NOTICES: usize = 64;

/// How long a reader waits on a page before the kernel reports it overdue;
/// it goes on waiting.
const REPORT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    System,
    Data,
    Log,
    Boot,
}

impl Role {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "system" => Some(Self::System),
            "data" => Some(Self::Data),
            "log" => Some(Self::Log),
            "boot" => Some(Self::Boot),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Data => "data",
            Self::Log => "log",
            Self::Boot => "boot",
        }
    }
}

struct Client {
    conn: Connection,
    rx: ipc::FrameRx<{ core::mem::size_of::<Request>() }>,
    /// Its grant's root, beneath which every path it names is resolved:
    /// `None` for an exported connection, which names no path.
    root: Option<String>,
    /// Its grant's share, which it spends; an exported connection's is the
    /// share of the connection that exported it.
    share: u64,
    /// Its grant is read-write and the volume is: what it may change.
    writes: bool,
    window: Option<SharedMemory>,
    fids: Ids,
    since: Instant,
    /// A watch on its connection waits in the poller.
    armed: bool,
}

impl Client {
    /// Whether it was exported: its served slot was taken then, so it waits on
    /// no handshake.
    fn exported(&self) -> bool {
        self.root.is_none()
    }

    /// Whether it holds a served slot.
    fn served(&self) -> bool {
        self.window.is_some() || self.exported()
    }

    /// Whether it is a handshake still waiting on its hello.
    fn waiting(&self) -> bool {
        !self.served()
    }
}

/// A pipe a program writes and this server writes to a file at its
/// description's offset.
struct Stream {
    pipe: Pipe,
    file: Shared,
    /// The share of the client that asked for it, which it spends.
    share: u64,
    /// A watch on its pipe waits in the poller.
    armed: bool,
}

/// The private port exports are made on: its acceptor, and the namespace
/// holding this server's one connector to it.
struct Exports {
    acceptor: Acceptor,
    names: Namespace,
}

impl Exports {
    fn new() -> Self {
        let (acceptor, connector) = port::create().unwrap_or_else(|e| panic!("fileserver: no export port ({e:?})"));
        let names = toyos::namespace::build()
            .add(EXPORT_PORT, &connector)
            .finish()
            .unwrap_or_else(|e| panic!("fileserver: no namespace for the export port ({e:?})"));
        Self { acceptor, names }
    }

    /// A connection to this server: the end to hand away, narrowed to
    /// [`EXPORTED`], and this server's.
    fn connect(&self) -> Result<(Connection, Connection), Error> {
        let theirs = self.names.open(EXPORT_PORT).map_err(|e| Error::kernel("an export's connection", e))?;
        let ours = self.acceptor.accept().unwrap_or_else(|e| panic!("fileserver: the export port refused an accept ({e:?})"));
        let narrowed = toyos::AsHandle::as_handle(&theirs)
            .try_clone_narrowed(EXPORTED)
            .map_err(|e| Error::kernel("an export's narrowed end", e))?;
        Ok((Connection::received(narrowed), ours))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let role = args.get(1).and_then(|r| Role::parse(r)).unwrap_or_else(|| {
        panic!("fileserver: started with {args:?}; the first argument is a role: system, data, log or boot")
    });
    let mut guid = None;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            flag if flag.starts_with("--") => panic!("fileserver: {flag} is no argument of this server"),
            named if guid.is_none() => guid = Some(named),
            extra => panic!("fileserver: a second partition, {extra}, after {guid:?}"),
        }
    }
    fileserver::volume::take_anchor();
    let dirs = toyos_manifest::role_dirs(role.name()).expect("every role this server parses is a manifest role");
    let label = format!("{SERVE_PREFIX}{CAPABILITY_PREFIX}{}", role.name());
    let acceptor: Acceptor = Endowments::get()
        .take(&label)
        .unwrap_or_else(|| panic!("fileserver: started without its role's acceptor, `{label}`"));
    let roots: Vec<&str> = dirs.iter().map(|d| d.root).collect();
    let (volume, memory) = open_volume(role, guid, &roots);
    println!(
        "fileserver: {role:?} serving {} — {}",
        dirs.iter().map(|d| d.dir).collect::<Vec<_>>().join(", "),
        volume.describe()
    );
    Server {
        volume,
        files: Files::new(pager(role)),
        pager_armed: false,
        memory,
        acceptor,
        clients: BTreeMap::new(),
        next_client: 0,
        streams: BTreeMap::new(),
        next_stream: 0,
        exports: Exports::new(),
        writeback: WriteBack::default(),
        scratch: Vec::new(),
        volume_id: u64::from(toyos_abi::syscall::getpid().0),
        acceptor_armed: false,
    }
    .serve()
}

/// The share of physical memory the file servers' pagers together hold in
/// pages: LOG and BOOT [`SMALL_PAGES`] each, DATA the rest. An interim
/// bound, since nothing reclaims a pager's clean pages for another job
/// (`issues/a-pager-holds-a-fixed-capacity-until-the-memory-service-reclaims-its-clean-pages.md`).
const PAGES_SHARE: u64 = 8;

/// What LOG and BOOT hold: 16 MiB of pages each.
const SMALL_PAGES: u64 = (16 << 20) / toyos::paged::PAGE;

/// The most pages a DATA pager holds dirty: 32 MiB.
const DATA_DIRTY: u64 = 8 * 1024;

/// The role's pager: how many pages its files hold, how many of them may be
/// dirty and how many files it keeps. None for SYSTEM, whose files are
/// immutable and are served from ROOT's memory.
fn pager(role: Role) -> Option<toyos::paged::Pager> {
    let (capacity, dirty, objects) = match role {
        Role::System => return None,
        Role::Data => {
            let share = toyos::system::memory_total() / PAGES_SHARE / toyos::paged::PAGE;
            let capacity = share.saturating_sub(2 * SMALL_PAGES).max(SMALL_PAGES);
            // A file held for every 16 pages, the table's own cost a small
            // part of the pages': at most 65,536.
            (capacity, DATA_DIRTY.min(capacity / 2), (capacity / 16).clamp(1024, 64 * 1024))
        }
        Role::Log => (SMALL_PAGES, 512, 1024),
        Role::Boot => (SMALL_PAGES, 0, 1024),
    };
    let pager = toyos::paged::Pager::create(capacity, dirty, objects, REPORT)
        .unwrap_or_else(|e| panic!("fileserver: the {role:?} server makes no pager ({e:?}); its row is no pager row"));
    Some(pager)
}

/// A session on the partition diskserver serves under `guid`.
fn served(guid: [u8; 16]) -> Option<Served> {
    let names = endow::namespace()?;
    let own = toyos::namespace::build().keep(names, &[toyos_blockring::PORT]).finish().ok()?;
    match diskserver::Session::open(own, toyos_blockring::PORT, guid) {
        Ok(session) => Some(Served::new(session)),
        Err(why) => {
            println!("fileserver: diskserver would not open the partition: {why:?}");
            None
        }
    }
}

/// The TOYOS-DATA partitions diskserver serves, or why it would not say: a disk
/// that failed is not a machine without one, and is never answered with memory.
fn data_partitions() -> Result<Vec<[u8; 16]>, String> {
    let Some(names) = endow::namespace() else { return Ok(Vec::new()) };
    match diskserver::list(names, toyos_blockring::PORT) {
        Ok(listed) => {
            Ok(listed.into_iter().filter(|l| l.kind == toyos_gpt::Guid::TOYOS_DATA.0).map(|l| l.unique).collect())
        }
        // No block service in this image: the namespace has no `block`.
        Err(diskserver::Error::NotHeld) => Ok(Vec::new()),
        Err(why) => Err(format!("the block service would not list its partitions ({why:?})")),
    }
}

/// The role's volume, and the memory nobody writes that it is, if it is one.
fn open_volume(role: Role, guid: Option<&str>, roots: &[&str]) -> (Box<dyn Volume>, Option<&'static toyos::shm::Immutable>) {
    let volume = match role {
        Role::System => {
            let (volume, memory) = root();
            return (Box::new(volume), Some(memory));
        }
        Role::Data => match fileserver::data::find(data_partitions().as_deref().map_err(String::clone)) {
            Located::Nowhere => ram(roots, "this machine has no DATA partition"),
            Located::Served(guid) => match served(guid) {
                Some(disk) => data_on(disk, roots),
                None => absent(roots, "the DATA partition would not open".into()),
            },
            Located::Refused(why) => absent(roots, why),
        },
        Role::Log | Role::Boot => {
            // The supervisor starts this role with its partition's GUID.
            let guid = guid.unwrap_or_else(|| panic!("fileserver: the {role:?} server was started without its partition's GUID"));
            let parsed = toyos_gpt::Guid::parse(guid).unwrap_or_else(|| panic!("fileserver: {guid} is no partition GUID"));
            match served(parsed.0).map(|disk| fat_on(disk, role == Role::Log)) {
                Some(Ok(volume)) => volume,
                Some(Err(why)) => {
                    println!("fileserver: the {role:?} partition does not mount: {why}");
                    Box::new(Absent::new(roots, why))
                }
                None => Box::new(Absent::new(
                    roots,
                    format!("the {role:?} partition {guid} is on no disk this server reaches"),
                )),
            }
        }
    };
    (volume, None)
}

/// ROOT, out of the image the supervisor endowed, and that image: for the
/// life of this process, which is the volume's. Without it there is no
/// `/system`, and the supervisor that started this server is told by its end.
fn root() -> (RootVolume, &'static toyos::shm::Immutable) {
    let label = toyos_abi::syscall::ROOT_IMAGE_LABEL;
    let handle = Endowments::get()
        .take::<toyos::OwnedHandle>(label)
        .unwrap_or_else(|| panic!("fileserver: the system role was started without `{label}`"));
    let image = toyos::shm::Immutable::adopt(handle)
        .unwrap_or_else(|why| panic!("fileserver: `{label}` is no image nobody writes: {why}"));
    let image: &'static toyos::shm::Immutable = Box::leak(Box::new(image));
    (RootVolume::mount(image.as_slice()).unwrap_or_else(|why| panic!("fileserver: {why}")), image)
}

fn absent(roots: &[&str], why: String) -> Box<dyn Volume> {
    println!("fileserver: {why}; DATA is absent this boot");
    Box::new(Absent::new(roots, why))
}

fn data_on<D: Disk + 'static>(disk: D, roots: &[&str]) -> Box<dyn Volume> {
    match DataVolume::probe(disk, CLEAN_BLOCKS, roots, fileserver::volume::now_nanos) {
        Probed::Mounted(volume) => Box::new(volume),
        Probed::Unmountable(why) => {
            println!("fileserver: the DATA volume is ours and does not mount ({why}); it is absent this boot");
            Box::new(Absent::new(roots, why))
        }
        // A disk that is there and holds somebody's bytes is no machine without
        // one: nothing stands in for it, and a designation is how it becomes
        // ToyOS's.
        Probed::Foreign => absent(roots, "the DATA partition holds no volume of ours and no designation stamp".into()),
    }
}

fn ram(roots: &[&str], why: &str) -> Box<dyn Volume> {
    println!("fileserver: {why}; /apps, /config, /home and /state are in memory and will not survive a reboot");
    match DataVolume::format(Ram::new(RAM_BLOCKS), CLEAN_BLOCKS, roots, fileserver::volume::now_nanos) {
        Ok(volume) => Box::new(volume),
        Err(why) => panic!("fileserver: a volume in memory would not format: {why}"),
    }
}

fn fat_on<D: Disk + 'static>(disk: D, writable: bool) -> Result<Box<dyn Volume>, String> {
    FatVolume::mount(disk, CLEAN_BLOCKS, writable, fileserver::volume::now_nanos)
        .map(|v| Box::new(v) as Box<dyn Volume>)
}

struct Server {
    volume: Box<dyn Volume>,
    /// Its files' pages, in the kernel's paged objects.
    files: Files<toyos::paged::Pager>,
    /// A watch on the pager waits in the poller.
    pager_armed: bool,
    /// The memory nobody writes that the volume is, which
    /// [`Volume::in_place`] names bytes of: ROOT's, and no other volume's.
    memory: Option<&'static toyos::shm::Immutable>,
    /// The role's port, which every grant is minted on.
    acceptor: Acceptor,
    clients: BTreeMap<u64, Client>,
    next_client: u64,
    streams: BTreeMap<u64, Stream>,
    next_stream: u64,
    exports: Exports,
    /// When the sync nobody asked for is due.
    writeback: WriteBack,
    /// A write's bytes, copied out of the client's window or a stream's pipe
    /// into this process's own memory before the volume sees them. Kept, so a
    /// write allocates nothing.
    scratch: Vec<u8>,
    /// The volume every node is on: this start's pid, which the kernel never
    /// issues again, since a restart numbers its files afresh.
    volume_id: u64,
    /// A watch on the acceptor waits in the poller.
    acceptor_armed: bool,
}

/// What one request is answered.
enum Answer {
    Reply(Reply),
    /// A path the client resolves again, `len` bytes of the request's span.
    Follow(usize),
    /// A reply that carries a handle.
    WithHandle(Reply, toyos::OwnedHandle),
    /// No frame: what a `CLOSE` is answered.
    Nothing,
    /// The client broke the protocol and is let go.
    Drop(&'static str),
    /// The client is answered this refusal and let go, for this reason.
    Refuse(Error, String),
}

/// What `meta` and `identity` say a node is, on `volume`. These volumes keep
/// one name per file and no access or change time.
fn stat_reply(meta: Meta, identity: u64, volume: u64) -> Reply {
    let stat = Stat { kind: meta.kind.wire(), size: meta.size, nlink: 1, atime: 0, mtime: meta.mtime, ctime: 0, volume, node: identity };
    Reply::stat(&stat)
}

/// Client `id`'s window, borrowed from the clients alone so a request can
/// reach the volume while it holds it.
fn window_of(clients: &BTreeMap<u64, Client>, id: u64) -> Bytes<'_> {
    clients[&id].window.as_ref().expect("a request comes after the window").bytes()
}

impl Server {
    /// Whether another accepted connection may wait on its hello.
    fn takes_handshakes(&self) -> bool {
        self.clients.values().filter(|c| c.waiting()).count() < MAX_HANDSHAKES
    }

    /// A watch stays in the poller until it fires, so only what fired since
    /// the last wait, and what is new, is watched again: each watch costs the
    /// kernel a pass over every poll the ring holds.
    fn serve(mut self) -> ! {
        let poller = Poller::new(Poller::MAX_HANDLES);
        let mut ready = Vec::new();
        loop {
            if !self.acceptor_armed && self.takes_handshakes() {
                poller.watch(&self.acceptor, READABLE, TOKEN_ACCEPTOR);
                self.acceptor_armed = true;
            }
            for (id, c) in self.clients.iter_mut().filter(|(_, c)| !c.armed) {
                poller.watch(&c.conn, READABLE, TOKEN_CLIENT + id);
                c.armed = true;
            }
            if let (false, Some(pager)) = (self.pager_armed, self.files.pager()) {
                poller.watch(pager, READABLE, TOKEN_PAGER);
                self.pager_armed = true;
            }
            for (id, s) in self.streams.iter_mut().filter(|(_, s)| !s.armed) {
                poller.watch(&s.pipe, READABLE, TOKEN_STREAM + id);
                s.armed = true;
            }
            let now = Instant::now();
            let timeout = self
                .clients
                .values()
                .filter(|c| c.waiting())
                .map(|c| HANDSHAKE_TIMEOUT.saturating_sub(now.duration_since(c.since)))
                .chain(self.writeback.left(now))
                .min()
                .map_or(u64::MAX, |left| left.as_nanos() as u64);
            ready.clear();
            poller.wait(1, timeout, |t| ready.push(t));

            for &token in &ready {
                if token == TOKEN_ACCEPTOR {
                    self.acceptor_armed = false;
                    self.accept();
                } else if token == TOKEN_PAGER {
                    self.pager_armed = false;
                    self.notices();
                } else if token < TOKEN_STREAM {
                    let id = token - TOKEN_CLIENT;
                    if let Some(c) = self.clients.get_mut(&id) {
                        c.armed = false;
                    }
                    self.pump(id);
                } else {
                    let sid = token - TOKEN_STREAM;
                    if let Some(s) = self.streams.get_mut(&sid) {
                        s.armed = false;
                    }
                    self.drain(sid);
                }
            }
            let now = Instant::now();
            let late: Vec<u64> = self
                .clients
                .iter()
                .filter(|(_, c)| c.waiting() && now.duration_since(c.since) >= HANDSHAKE_TIMEOUT)
                .map(|(id, _)| *id)
                .collect();
            for id in late {
                self.drop_client(id, "it never lent its window");
            }
            if self.writeback.take_due(now) {
                if let Err(e) = self.sync() {
                    println!("fileserver: the write-back sync failed: {e:?}");
                }
            }
        }
    }

    /// Take the next connection and read its grant. Only the supervisor mints
    /// on this port, so a connection without one this server reads is let go
    /// by name; one whose share already holds its part of the handshakes is
    /// answered `TooManyConnections` and let go.
    fn accept(&mut self) {
        let conn = match self.acceptor.accept() {
            Ok(conn) => conn,
            Err(why) => panic!("fileserver: its own acceptor refused an accept: {why:?}"),
        };
        let mut badge = [0u8; MAX_BADGE];
        let grant = match self.acceptor.badge(&conn, &mut badge) {
            Ok(bytes) => match Grant::decode(bytes) {
                Some(grant) => grant,
                None => return println!("fileserver: letting a connection go: its badge is no grant: {bytes:?}"),
            },
            Err(why) => return println!("fileserver: letting a connection go: it carries no grant ({why:?})"),
        };
        let mine = self.clients.values().filter(|c| c.share == grant.share && c.waiting()).count();
        if let Err(why) = bounds::handshake(grant.share, mine) {
            // Let go whether or not the refusal went: either way it is said.
            let _ = conn.try_send(REPLY, &Reply::refused(Error::TooManyConnections));
            return println!("fileserver: letting a connection go: {why}");
        }
        let id = self.next_client;
        self.next_client += 1;
        let client = Client {
            conn,
            rx: ipc::FrameRx::new(),
            root: Some(grant.root.to_string()),
            share: grant.share,
            writes: grant.access == Access::ReadWrite && self.volume.writable(),
            window: None,
            fids: Ids::default(),
            since: Instant::now(),
            armed: false,
        };
        self.clients.insert(id, client);
    }

    fn drop_client(&mut self, id: u64, why: &str) {
        let Some(mut client) = self.clients.remove(&id) else { return };
        if !why.is_empty() {
            println!("fileserver: dropping client {id} of {:?}, share {}: {why}", client.root, client.share);
        }
        for (node, identity) in client.fids.drain() {
            // Nobody is left to answer: a refused close is said, and its node
            // kept for the next sync.
            let _ = self.files.closed(&mut *self.volume, identity, node);
        }
    }

    /// What the pager's objects ask: each reader's pages read off the volume.
    fn notices(&mut self) {
        let mut buf = [toyos::paged::PagedNotice::default(); NOTICES];
        loop {
            let Some(pager) = self.files.pager() else { return };
            let notices: Vec<_> = match pager.notices(&mut buf) {
                Ok(notices) => notices.collect(),
                Err(e) => panic!("fileserver: its own pager refused its notices ({e:?})"),
            };
            let more = notices.len() == NOTICES;
            for notice in notices {
                self.files.notice(&mut *self.volume, notice);
            }
            if !more {
                return;
            }
        }
    }

    /// One request of one client, and no more: a client whose next request
    /// is already queued is answered at the next wait, after every other
    /// client that was ready, since a watch on a connection with a frame
    /// queued fires at once, and `FrameRx` reads no byte past the frame. A
    /// loop until the client went quiet would let one that asks as fast as it
    /// is answered hold every other off for as long as it kept asking.
    fn pump(&mut self, id: u64) {
        let Some(client) = self.clients.get_mut(&id) else { return };
        match client.rx.pump(&client.conn) {
            RxStep::Idle => {}
            RxStep::Eof => self.drop_client(id, ""),
            RxStep::Malformed => self.drop_client(id, "it sent a frame this protocol cannot describe"),
            RxStep::Frame { msg_type, payload_len, .. } => {
                let Ok(request) = ipc::decode_payload::<Request>(client.rx.payload(payload_len)) else {
                    return self.drop_client(id, "its request was not one `Request` wide");
                };
                let answer = self.answer(id, msg_type, request);
                self.reply(id, request.tag, answer);
            }
        }
    }

    /// Send `answer` under the request's `tag`, or let the client go when it
    /// will not take it.
    fn reply(&mut self, id: u64, tag: u64, answer: Answer) {
        let Some(client) = self.clients.get(&id) else { return };
        let sent = match answer {
            Answer::Reply(reply) => client.conn.try_send(REPLY, &Reply { tag, ..reply }),
            Answer::Follow(len) => client.conn.try_send(FOLLOW, &Reply { tag, value: len as u64, ..Reply::ok() }),
            Answer::WithHandle(reply, handle) => client.conn.try_send_with(REPLY, &Reply { tag, ..reply }, [handle]),
            Answer::Nothing => return,
            Answer::Drop(why) => return self.drop_client(id, why),
            Answer::Refuse(e, why) => {
                // Let go whether or not the refusal went: either way it is said.
                let _ = client.conn.try_send(REPLY, &Reply { tag, ..Reply::refused(e) });
                return self.drop_client(id, &why);
            }
        };
        if let Err(why) = sent {
            self.drop_client(id, &format!("its connection would not take a reply ({why:?})"));
        }
    }

    /// The client's window.
    fn window(&self, id: u64) -> Bytes<'_> {
        window_of(&self.clients, id)
    }

    /// The request's span: where it starts in the window and how long it is.
    fn span(r: &Request) -> Result<(usize, usize), Error> {
        bounds::span(r.at, r.span)
    }

    /// The request's two paths out of its span, UTF-8: the second is a path
    /// or a symlink's target.
    fn paths(&self, id: u64, r: &Request) -> Result<(String, String), Error> {
        let (at, room) = Self::span(r)?;
        let (len, len2) = bounds::paths(room, r.len, r.len2)?;
        let mut bytes = vec![0u8; len + len2];
        self.window(id).read(at, &mut bytes);
        let second = bytes.split_off(len);
        Ok((bounds::path(bytes)?, bounds::path(second)?))
    }

    /// The request's first path.
    fn path(&self, id: u64, r: &Request) -> Result<String, Error> {
        self.paths(id, r).map(|(first, _)| first)
    }

    /// The directory a path from `fid` starts in, on the volume, and the
    /// grant's root beneath which it must stay: a directory moved out from
    /// under the root is refused `Escapes`, one removed names nothing, and a
    /// connection given no directory names no path.
    fn start(&self, id: u64, fid: u64) -> Result<(String, String), Error> {
        let client = &self.clients[&id];
        let root = client.root.clone().ok_or(Error::NoDirectory)?;
        if fid == ROOT {
            return Ok((root.clone(), root));
        }
        let shared = client.fids.get(fid)?;
        let start = match &shared.borrow().held {
            Held::Dir { removed: true, .. } => return Err(Error::NotFound),
            Held::Dir { path, .. } => path.clone(),
            Held::File(_) => return Err(Error::NotADirectory),
        };
        let beneath = root.is_empty() || start == root || start.strip_prefix(&root).is_some_and(|rest| rest.starts_with('/'));
        match beneath {
            true => Ok((root, start)),
            false => Err(Error::Escapes),
        }
    }

    /// `rel` from `fid`, as a volume path.
    fn resolve(&mut self, id: u64, fid: u64, rel: &str, follow_last: bool) -> Result<Resolved, Error> {
        bounds::relative(rel)?;
        let (root, start) = self.start(id, fid)?;
        let volume = &mut self.volume;
        let mut lookup = |p: &str| match volume.lstat(p) {
            Ok(meta) if meta.kind == Kind::Symlink => volume.read_link(p).map(Found::Link).map_err(drop),
            Ok(_) | Err(Error::NotFound) => Ok(Found::Other),
            Err(_) => Err(()),
        };
        resolve::resolve(&root, &start, rel, follow_last, &mut lookup).map_err(|e| match e {
            Escape::Escape => Error::Escapes,
            Escape::Loop => Error::Loop,
            Escape::NotFound => Error::NotFound,
            Escape::Io => Error::Io,
        })
    }

    /// `rel` from `fid` resolved without following its last component, for an
    /// operation on the name itself; an absolute link on the way is the
    /// client's, answered in the request's span.
    fn name(&mut self, id: u64, r: &Request, fid: u64, rel: &str) -> Result<Result<String, Answer>, Error> {
        Ok(match self.resolve(id, fid, rel, false)? {
            Resolved::Path(p) => Ok(p),
            Resolved::Absolute(p) => Err(self.follow(id, r, &p)?),
        })
    }

    /// Put `bytes` in the request's span, which they must fit.
    fn put(&self, id: u64, r: &Request, bytes: &[u8]) -> Result<(), Error> {
        let (at, room) = Self::span(r)?;
        if bytes.len() > room {
            return Err(Error::NameTooLong);
        }
        self.window(id).write(at, bytes);
        Ok(())
    }

    fn answer(&mut self, id: u64, op: u32, r: Request) -> Answer {
        if op == HELLO {
            let (all, mine) = self.served(self.clients[&id].share);
            let client = self.clients.get_mut(&id).expect("pumped");
            if client.window.is_some() {
                return Answer::Drop("it lent a second window");
            }
            let slot = if client.exported() { Slot::Taken } else { Slot::AtHello };
            // The window travels in the HELLO frame: its reader took it with the frame.
            let lent = || client.rx.take_handles::<1>().map(|[lent]| SharedMemory::lent(lent, WINDOW_BYTES));
            match bounds::hello(slot, all, mine, lent) {
                Hello::Served(window) => client.window = Some(window),
                Hello::Refused(why) => return Answer::Refuse(Error::TooManyConnections, why),
                Hello::Dropped(why) => return Answer::Drop(why),
            }
            let granted = if client.writes { RIGHT_WRITE } else { 0 };
            return Answer::Reply(Reply { value: granted, ..Reply::ok() });
        }
        if self.clients[&id].window.is_none() {
            return Answer::Drop("it asked before it lent a window");
        }
        match self.serve_one(id, op, r) {
            Ok(answer) => answer,
            Err(e) => Answer::Reply(Reply::refused(e)),
        }
    }

    /// How many clients hold a served slot, and how many of them are `share`'s.
    fn served(&self, share: u64) -> (usize, usize) {
        let served = self.clients.values().filter(|c| c.served());
        served.fold((0, 0), |(all, mine), c| (all + 1, mine + usize::from(c.share == share)))
    }

    /// The description `fid` names on client `id`.
    fn fid(&self, id: u64, fid: u64) -> Result<Shared, Error> {
        self.clients[&id].fids.get(fid).cloned()
    }

    /// The description `fid` names, with what its streams hold already
    /// written at its offset, so a request that reads or moves the offset
    /// comes after every byte a stream was given before it.
    fn settled(&mut self, id: u64, fid: u64) -> Result<Shared, Error> {
        let file = self.fid(id, fid)?;
        let streams: Vec<u64> =
            self.streams.iter().filter(|(_, s)| Rc::ptr_eq(&s.file, &file)).map(|(sid, _)| *sid).collect();
        for sid in streams {
            for _ in 0..STREAM_PRESENT {
                if !self.drain(sid) {
                    break;
                }
            }
        }
        Ok(file)
    }

    /// Mark the volume written: the write-back sync is due from here.
    fn dirtied(&mut self) {
        self.writeback.dirtied(Instant::now());
    }

    /// What the volume path `path` is.
    fn stat_path(&mut self, path: &str) -> Result<Answer, Error> {
        let meta = self.volume.lstat(path)?;
        let identity = self.volume.identity(path)?;
        Ok(Answer::Reply(stat_reply(meta, identity, self.volume_id)))
    }

    /// What a description holds open is.
    fn stat_held(&mut self, file: &Shared) -> Result<Answer, Error> {
        let (held, identity) = {
            let f = file.borrow();
            let held = match &f.held {
                Held::File(node) => Ok(*node),
                Held::Dir { path, removed } => Err((path.clone(), *removed)),
            };
            (held, f.identity)
        };
        let meta = match held {
            Ok(node) => self.volume.node_meta(node)?,
            // A removed directory is what it was, with nothing in it.
            Err((_, true)) => Meta { kind: Kind::Dir, size: 0, mtime: 0 },
            Err((path, false)) => self.volume.lstat(&path)?,
        };
        Ok(Answer::Reply(stat_reply(meta, identity, self.volume_id)))
    }

    /// The open file `fid` names, a directory refused `IsADirectory`, and
    /// where in the window its data travels and how much.
    fn file_and_span(&mut self, id: u64, r: &Request) -> Result<(Shared, Node, (usize, usize)), Error> {
        let file = self.settled(id, r.fid)?;
        let node = file.borrow().node()?;
        Ok((file, node, Self::span(r)?))
    }

    fn serve_one(&mut self, id: u64, op: u32, r: Request) -> Result<Answer, Error> {
        match rights::changes(op, r.flags) {
            None => return Ok(Answer::Drop(NO_SUCH_OPERATION)),
            Some(true) if !self.clients[&id].writes => return Err(Error::ReadOnly),
            Some(_) => {}
        }
        match op {
            OPEN => self.open(id, &r),
            // These volumes keep no file without a name.
            CREATE_UNNAMED => Err(Error::Unsupported),
            CLOSE => {
                // Nobody waits on the answer: an id the client does not hold
                // closes nothing, and a refused close is its volume's to say.
                let client = self.clients.get_mut(&id).expect("pumped");
                if let Ok(Some((node, identity))) = client.fids.close(r.fid) {
                    let _ = self.files.closed(&mut *self.volume, identity, node);
                }
                Ok(Answer::Nothing)
            }
            DUP => {
                let client = self.clients.get_mut(&id).expect("pumped");
                let fid = client.fids.dup(r.fid)?;
                Ok(Answer::Reply(Reply { value: fid, ..Reply::ok() }))
            }
            READ | PREAD => {
                let (file, node, (span_at, span)) = self.file_and_span(id, &r)?;
                let at = if op == READ { file.borrow().offset } else { r.offset };
                let window = window_of(&self.clients, id).sub(span_at, span);
                let identity = file.borrow().identity;
                let n = self.files.read(&mut *self.volume, identity, node, at, window)?;
                if op == READ {
                    file.borrow_mut().offset = at + n as u64;
                }
                Ok(Answer::Reply(Reply { value: n as u64, ..Reply::ok() }))
            }
            WRITE | PWRITE => {
                let (file, _, (span_at, span)) = self.file_and_span(id, &r)?;
                let window = window_of(&self.clients, id).sub(span_at, span);
                if !file.borrow().write {
                    return Err(Error::NotOpenForWriting);
                }
                let len = window.len();
                if self.scratch.len() < len {
                    self.scratch.resize(len, 0);
                }
                window.read(0, &mut self.scratch[..len]);
                let end = match op {
                    WRITE => self.write_at(&file, len)?,
                    _ => self.write_to(&file, r.offset, len)?,
                };
                Ok(Answer::Reply(Reply { value: len as u64, value2: end, ..Reply::ok() }))
            }
            SEEK => {
                let file = self.settled(id, r.fid)?;
                let node = file.borrow().node()?;
                let base = match r.flags {
                    SEEK_START => 0,
                    SEEK_CURRENT => file.borrow().offset,
                    SEEK_END => self.volume.node_meta(node)?.size,
                    _ => return Err(Error::InvalidRequest),
                };
                let at = bounds::seek(base, r.offset as i64)?;
                file.borrow_mut().offset = at;
                Ok(Answer::Reply(Reply { value: at, ..Reply::ok() }))
            }
            EXPORT => self.export(id, &r),
            STAT => {
                if r.flags & !FOLLOW_LAST != 0 {
                    return Err(Error::InvalidRequest);
                }
                let rel = self.path(id, &r)?;
                if rel.is_empty() && r.fid != ROOT {
                    let file = self.settled(id, r.fid)?;
                    return self.stat_held(&file);
                }
                match self.resolve(id, r.fid, &rel, r.flags & FOLLOW_LAST != 0)? {
                    Resolved::Absolute(p) => self.follow(id, &r, &p),
                    Resolved::Path(p) => self.stat_path(&p),
                }
            }
            SETATTR => self.set_attr(id, &r),
            FSYNC => {
                let file = self.settled(id, r.fid)?;
                let named = match file.borrow().held {
                    Held::File(node) => Some((file.borrow().identity, node)),
                    Held::Dir { .. } => None,
                };
                self.files.fsync(&mut *self.volume, named)?;
                Ok(Answer::Reply(Reply::ok()))
            }
            SYNC => self.sync(),
            ROOM => {
                let room = self.volume.room()?;
                Ok(Answer::Reply(Reply { value: room.free, value2: room.unit, ..Reply::ok() }))
            }
            STREAM => {
                let file = self.fid(id, r.fid)?;
                file.borrow().node()?;
                if !file.borrow().write {
                    return Err(Error::NotOpenForWriting);
                }
                let share = self.clients[&id].share;
                let mine = self.streams.values().filter(|s| s.share == share).count();
                bounds::stream(self.streams.len(), mine)?;
                let (read, write) = toyos::pipe_pair().map_err(|e| Error::kernel("a stream's pipe", e))?;
                let sid = self.next_stream;
                self.next_stream += 1;
                self.streams.insert(sid, Stream { pipe: read, file, share, armed: false });
                Ok(Answer::WithHandle(Reply::ok(), write.into()))
            }
            IMAGE => {
                let file = self.fid(id, r.fid)?;
                let node = file.borrow().node()?;
                let (Some(memory), Some((at, len))) = (self.memory, self.volume.in_place(node)?) else {
                    return Err(Error::Unsupported);
                };
                let image = memory.sub(at, len).map_err(|e| Error::kernel("a file's image", e))?;
                Ok(Answer::WithHandle(Reply::ok(), image))
            }
            READDIR => {
                let file = self.fid(id, r.fid)?;
                let dir = match &file.borrow().held {
                    Held::Dir { removed: true, .. } => None,
                    Held::Dir { path, .. } => Some(path.clone()),
                    Held::File(_) => return Err(Error::NotADirectory),
                };
                let (_, room) = Self::span(&r)?;
                let (out, next) = match dir {
                    Some(dir) => listing::batch(&self.volume.list(&dir)?, r.offset, room)?,
                    None => (Vec::new(), READDIR_END),
                };
                self.put(id, &r, &out)?;
                Ok(Answer::Reply(Reply { value: out.len() as u64, value2: next, ..Reply::ok() }))
            }
            READLINK => {
                let rel = self.path(id, &r)?;
                let path = match self.name(id, &r, r.fid, &rel)? {
                    Ok(p) => p,
                    Err(follow) => return Ok(follow),
                };
                let target = self.volume.read_link(&path)?;
                self.put(id, &r, target.as_bytes())?;
                Ok(Answer::Reply(Reply { value: target.len() as u64, ..Reply::ok() }))
            }
            MKDIR => {
                let rel = self.path(id, &r)?;
                if rel.is_empty() {
                    return Err(Error::AlreadyExists);
                }
                let path = match self.name(id, &r, r.fid, &rel)? {
                    Ok(p) => p,
                    Err(follow) => return Ok(follow),
                };
                self.volume.mkdir(&path)?;
                self.dirtied();
                Ok(Answer::Reply(Reply::ok()))
            }
            UNLINK => {
                if r.flags & !UNLINK_DIR != 0 {
                    return Err(Error::InvalidRequest);
                }
                let rel = self.path(id, &r)?;
                let path = match self.name(id, &r, r.fid, &rel)? {
                    Ok(p) => p,
                    Err(follow) => return Ok(follow),
                };
                if path.is_empty() || self.clients[&id].root.as_deref() == Some(path.as_str()) {
                    return Err(Error::Root);
                }
                match r.flags & UNLINK_DIR != 0 {
                    true => {
                        self.volume.rmdir(&path)?;
                        self.each_dir(|held, removed| {
                            if *held == path {
                                *removed = true;
                            }
                        });
                    }
                    false => {
                        let gone = self.volume.identity(&path).ok();
                        self.volume.unlink(&path)?;
                        if let Some(identity) = gone {
                            self.files.forget(&mut *self.volume, identity);
                        }
                    }
                }
                self.dirtied();
                Ok(Answer::Reply(Reply::ok()))
            }
            RENAME => {
                let (from_rel, to_rel) = self.paths(id, &r)?;
                let (Resolved::Path(from), Resolved::Path(to)) =
                    (self.resolve(id, r.fid, &from_rel, false)?, self.resolve(id, r.fid2, &to_rel, false)?)
                else {
                    // A rename through a link that leaves this directory is a
                    // rename across two, which no one directory can do.
                    return Err(Error::CrossesDirectories);
                };
                let root = self.clients[&id].root.clone();
                if from.is_empty() || to.is_empty() || root.as_deref() == Some(from.as_str()) {
                    return Err(Error::Root);
                }
                let over = match self.volume.lstat(&to) {
                    Ok(meta) if meta.kind == Kind::File => self.volume.identity(&to).ok(),
                    _ => None,
                };
                self.volume.rename(&from, &to)?;
                if let Some(identity) = over {
                    self.files.forget(&mut *self.volume, identity);
                }
                self.each_dir(|held, _| descriptions::moved(held, &from, &to));
                self.dirtied();
                Ok(Answer::Reply(Reply::ok()))
            }
            SYMLINK => {
                let (link_rel, target) = self.paths(id, &r)?;
                if target.is_empty() {
                    return Err(Error::InvalidRequest);
                }
                if link_rel.is_empty() {
                    return Err(Error::AlreadyExists);
                }
                let Resolved::Path(link) = self.resolve(id, r.fid, &link_rel, false)? else {
                    return Err(Error::CrossesDirectories);
                };
                self.volume.symlink(&link, &target)?;
                self.dirtied();
                Ok(Answer::Reply(Reply::ok()))
            }
            LINK => {
                if r.flags & !LINK_KNOWN != 0 {
                    return Err(Error::InvalidRequest);
                }
                // These volumes keep one name per file.
                Err(Error::Unsupported)
            }
            _ => Ok(Answer::Drop(NO_SUCH_OPERATION)),
        }
    }

    /// `f` on every held directory's path and removal, on every connection.
    fn each_dir(&mut self, mut f: impl FnMut(&mut String, &mut bool)) {
        for client in self.clients.values() {
            for shared in client.fids.dirs() {
                if let Held::Dir { path, removed } = &mut shared.borrow_mut().held {
                    f(path, removed);
                }
            }
        }
    }

    fn open(&mut self, id: u64, r: &Request) -> Result<Answer, Error> {
        if r.flags & !O_KNOWN != 0 {
            return Err(Error::InvalidRequest);
        }
        let rel = self.path(id, r)?;
        let path = match self.resolve(id, r.fid, &rel, true)? {
            Resolved::Absolute(p) => return self.follow(id, r, &p),
            Resolved::Path(p) => p,
        };
        if self.clients[&id].fids.len() >= descriptions::MAX_IDS {
            return Err(Error::TooManyOpen);
        }
        let writes = r.flags & (O_WRITE | O_APPEND | O_TRUNCATE) != 0;
        let found = match self.volume.lstat(&path) {
            Ok(meta) => Some(meta.kind),
            Err(Error::NotFound) => None,
            Err(e) => return Err(e),
        };
        let (held, meta) = match found {
            Some(Kind::Dir) if r.flags & O_CREATE_NEW != 0 => return Err(Error::AlreadyExists),
            Some(Kind::Dir) if writes => return Err(Error::IsADirectory),
            Some(Kind::Dir) => (Held::Dir { path: path.clone(), removed: false }, self.volume.lstat(&path)?),
            _ if r.flags & O_DIRECTORY != 0 => return Err(if found.is_some() { Error::NotADirectory } else { Error::NotFound }),
            _ => {
                let how = OpenHow {
                    create: r.flags & O_CREATE != 0,
                    create_new: r.flags & O_CREATE_NEW != 0,
                    truncate: r.flags & O_TRUNCATE != 0,
                };
                let node = self.volume.open(&path, how)?;
                // The volume truncated first, so a refused open changed
                // nothing; the object follows before anything writes back.
                if how.truncate && found.is_some() {
                    let truncated = self.volume.identity(&path).and_then(|identity| self.files.truncated(&mut *self.volume, identity, node));
                    if let Err(e) = truncated {
                        let _ = self.volume.close(node);
                        return Err(e);
                    }
                }
                match self.volume.node_meta(node) {
                    Ok(meta) => {
                        if how.create || how.truncate || how.create_new {
                            self.dirtied();
                        }
                        (Held::File(node), meta)
                    }
                    Err(e) => {
                        let _ = self.volume.close(node);
                        return Err(e);
                    }
                }
            }
        };
        let identity = match self.volume.identity(&path) {
            Ok(identity) => identity,
            Err(e) => {
                if let Held::File(node) = held {
                    let _ = self.volume.close(node);
                }
                return Err(e);
            }
        };
        let mut holder = None;
        if let Held::File(node) = held {
            if let Err(e) = self.files.opened(&mut *self.volume, identity, node) {
                let _ = self.volume.close(node);
                return Err(e);
            }
            // An open to read is answered with a holder of the file's
            // object, which reads without asking this server.
            if r.flags & O_READ != 0 {
                let minted = self.files.pager().zip(self.files.object(identity)).map(|(pager, object)| pager.holder(object));
                holder = match minted {
                    Some(Ok(handle)) => Some(handle),
                    Some(Err(e)) => {
                        println!("fileserver: file {identity}'s object made no holder ({e:?}); it is read through requests");
                        None
                    }
                    None => None,
                };
            }
        }
        let write = r.flags & (O_WRITE | O_APPEND) != 0;
        let append = r.flags & O_APPEND != 0;
        let node = match held {
            Held::File(node) => Some(node),
            Held::Dir { .. } => None,
        };
        let file = Description { held, identity, write, append, offset: 0 };
        let client = self.clients.get_mut(&id).expect("pumped");
        let fid = match client.fids.insert(Rc::new(RefCell::new(file))) {
            Ok(fid) => fid,
            Err(e) => {
                // The open counted is undone, as the close it will never have.
                if let Some(node) = node {
                    let _ = self.files.closed(&mut *self.volume, identity, node);
                }
                return Err(e);
            }
        };
        let reply = Reply { value: fid, ..stat_reply(meta, identity, self.volume_id) };
        Ok(match holder {
            Some(handle) => {
                self.files.held(identity);
                Answer::WithHandle(reply, handle)
            }
            None => Answer::Reply(reply),
        })
    }

    /// `SETATTR`: a size only through an id opened to write and no path; an
    /// access time is kept by none of these volumes, and a directory's time
    /// by none either.
    fn set_attr(&mut self, id: u64, r: &Request) -> Result<Answer, Error> {
        let change = Change::decode(r).filter(|_| r.flags & !SETATTR_KNOWN == 0).ok_or(Error::InvalidRequest)?;
        let rel = self.path(id, r)?;
        let held = rel.is_empty() && r.fid != ROOT;
        if change.atime.is_some() {
            return Err(Error::Unsupported);
        }
        if let Some(size) = change.size {
            if !held {
                return Err(Error::InvalidRequest);
            }
            let file = self.fid(id, r.fid)?;
            let (node, write, identity) = {
                let f = file.borrow();
                (f.node()?, f.write, f.identity)
            };
            if !write {
                return Err(Error::NotOpenForWriting);
            }
            bounds::in_a_file(size, 0)?;
            self.files.truncate(&mut *self.volume, identity, node, size)?;
            self.dirtied();
        }
        let Some(mtime) = change.mtime else { return Ok(Answer::Reply(Reply::ok())) };
        let mtime = match mtime {
            Time::Now => fileserver::volume::now_nanos(),
            Time::At(nanos) => nanos,
        };
        if held {
            let file = self.fid(id, r.fid)?;
            let (node, identity) = {
                let f = file.borrow();
                (f.node().map_err(|_| Error::Unsupported)?, f.identity)
            };
            self.volume.set_mtime(node, mtime)?;
            self.files.stamped(&mut *self.volume, identity, node)?;
        } else {
            let path = match self.resolve(id, r.fid, &rel, r.flags & FOLLOW_LAST != 0)? {
                Resolved::Absolute(p) => return self.follow(id, r, &p),
                Resolved::Path(p) => p,
            };
            if self.volume.lstat(&path)?.kind != Kind::File {
                return Err(Error::Unsupported);
            }
            let identity = self.volume.identity(&path)?;
            let node = self.volume.open(&path, OpenHow { create: false, create_new: false, truncate: false })?;
            let set = self.volume.set_mtime(node, mtime).and_then(|()| self.files.stamped(&mut *self.volume, identity, node));
            let closed = self.volume.close(node);
            set.and(closed)?;
        }
        self.dirtied();
        Ok(Answer::Reply(Reply::ok()))
    }

    /// The volume synced, answered as every file fared. A file whose entry
    /// was refused keeps the write-back due, so the next one tries it again.
    fn sync(&mut self) -> Result<Answer, Error> {
        let unwritten = self.files.sync(&mut *self.volume)?;
        self.writeback.synced(!unwritten.is_empty(), Instant::now());
        match unwritten.into_iter().next() {
            Some((_, e)) => Err(e),
            None => Ok(Answer::Reply(Reply::ok())),
        }
    }

    /// The absolute `path` an absolute link led to, for the client to
    /// resolve again, in the request's span.
    fn follow(&self, id: u64, r: &Request, path: &str) -> Result<Answer, Error> {
        if path.len() > MAX_PATH {
            return Err(Error::NameTooLong);
        }
        self.put(id, r, path.as_bytes())?;
        Ok(Answer::Follow(path.len()))
    }

    /// Write the scratch's first `len` bytes to `file` at its offset, or at
    /// its end when it appends, and move the offset past them: the offset.
    fn write_at(&mut self, file: &Shared, len: usize) -> Result<u64, Error> {
        let (node, append, offset) = {
            let f = file.borrow();
            (f.node()?, f.append, f.offset)
        };
        let at = if append { self.volume.node_meta(node)?.size } else { offset };
        let end = self.write_to(file, at, len)?;
        file.borrow_mut().offset = end;
        Ok(end)
    }

    /// Write the scratch's first `len` bytes to `file` at `at`: the offset
    /// after them.
    fn write_to(&mut self, file: &Shared, at: u64, len: usize) -> Result<u64, Error> {
        let (node, identity) = {
            let f = file.borrow();
            (f.node()?, f.identity)
        };
        let end = bounds::in_a_file(at, len as u64)?;
        self.files.write(&mut *self.volume, identity, node, at, &self.scratch[..len])?;
        self.dirtied();
        Ok(end)
    }

    /// A connection to this server for another process, holding the
    /// descriptions of the ids in client `id`'s span. Its served slot is
    /// taken here, of `id`'s share, or the export is refused.
    fn export(&mut self, id: u64, r: &Request) -> Result<Answer, Error> {
        let count = bounds::export_ids(r.len)?;
        let (at, room) = Self::span(r)?;
        if count * 8 > room {
            return Err(Error::InvalidRequest);
        }
        let mut words = vec![0u8; count * 8];
        self.window(id).read(at, &mut words);
        let fids: Vec<u64> =
            words.chunks_exact(8).map(|w| u64::from_le_bytes(w.try_into().expect("eight bytes"))).collect();
        let parent = &self.clients[&id];
        let (share, writes) = (parent.share, parent.writes);
        let fids = parent.fids.export(&fids)?;
        let (all, mine) = self.served(share);
        bounds::export(all, mine)?;
        let (theirs, ours) = self.exports.connect()?;
        let client = Client {
            conn: ours,
            rx: ipc::FrameRx::new(),
            root: None,
            share,
            writes,
            window: None,
            fids,
            since: Instant::now(),
            armed: false,
        };
        let child = self.next_client;
        self.next_client += 1;
        self.clients.insert(child, client);
        Ok(Answer::WithHandle(Reply { value: count as u64, ..Reply::ok() }, theirs.into_owned()))
    }

    /// One read of what a stream's writer has put in the pipe, written to its
    /// file at its description's offset, and no more: a pipe with more waiting
    /// fires at the next wait, after every other client that was ready —
    /// [`Self::pump`]'s rule. Whether it wrote anything.
    fn drain(&mut self, sid: u64) -> bool {
        if self.scratch.len() < STREAM_READ {
            self.scratch.resize(STREAM_READ, 0);
        }
        let Some(stream) = self.streams.get(&sid) else { return false };
        let ended = match stream.pipe.read_nonblock(&mut self.scratch[..STREAM_READ]) {
            Err(SyscallError::WouldBlock) => return false,
            Ok(0) | Err(SyscallError::Gone) => None,
            Err(e) => Some(format!("its pipe would not read ({e:?})")),
            Ok(n) => {
                let file = Rc::clone(&stream.file);
                match self.write_at(&file, n) {
                    Ok(_) => return true,
                    Err(Error::FileTooLarge) => Some("it reached the largest offset a file has".to_string()),
                    Err(e) => Some(format!("its write failed ({e:?})")),
                }
            }
        };
        if let Some(why) = ended {
            println!("fileserver: a stream is ended: {why}");
        }
        if let Some(stream) = self.streams.remove(&sid) {
            if let Some((node, identity)) = descriptions::release(stream.file) {
                // Nobody is left to answer: a refused close is said, and its
                // node kept for the next sync.
                let _ = self.files.closed(&mut *self.volume, identity, node);
            }
        }
        false
    }
}
