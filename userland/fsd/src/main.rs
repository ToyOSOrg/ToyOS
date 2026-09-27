//! fsd: one role's file server — DATA, LOG or BOOT — serving the directories
//! of that role as capabilities.
//!
//! **What it holds**: the acceptor of each directory its role serves, endowed
//! by init under `serve:fs:<dir>`; for its volume, a claim on a partition of a
//! disk the kernel drives, or blockd's `block` connector in its namespace; and
//! nothing else of the machine. Argv is the role, and for LOG and BOOT the
//! unique GUID of the partition the loader named for it when no claim on it
//! was minted; then the row's own arguments, which are tests' actuators
//! ([`END_ON`], [`END_AT_READ`], [`END_AT_MOUNT`]).
//!
//! **A connection is bound to the directory whose port it came in on**, and
//! every path on it is resolved there (`fsd::resolve`); a write on a read-only
//! volume is refused before the volume sees it.
//!
//! **A server never blocks on a client.** Accept and the first frame are two
//! events; a request is buffered until whole; every reply is one `try_send`,
//! and a client whose pipe will not take one is dropped by name. The window a
//! client lends is read once per request into this process's memory, and
//! what is acted on is that copy.
//!
//! **What is written reaches the disk at a sync**: an fsync, a client's
//! `SYNC`, or [`fsd::writeback::WRITEBACK`] after the first unsynced write when nothing asked
//! sooner. A kill loses what no sync covered, which is POSIX's promise and no
//! more.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use fsd::absent::Absent;
use fsd::data::{DataVolume, Probed};
use fsd::disk::{Claimed, Disk, Ram, Served};
use fsd::fat::FatVolume;
use fsd::resolve::{self, Found, Refusal as Escape, Resolved};
use fsd::volume::{Kind, Meta, Node, OpenHow, Out, Volume};
use fsd::writeback::WriteBack;
use toyos::endow::{self, Endowments};
use toyos::fs::*;
use toyos::ipc::{self, Connection, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::port::Acceptor;
use toyos::shm::SharedMemory;
use toyos::volatile::Window;
use toyos::Pipe;
use toyos_abi::syscall::{SyscallError, DEV_PREFIX, SERVE_PREFIX};

/// Clients served at once, machine-wide: one connection per directory per
/// process. The next is answered `ResourceExhausted` at its hello and let go,
/// by name.
const MAX_SERVED: usize = 128;

/// Connections taken and not yet answered at their hello. While this many
/// wait, the next waits in its port's queue — for at most
/// [`HANDSHAKE_TIMEOUT`], by which each of these is answered or let go.
const MAX_HANDSHAKES: usize = 32;

/// Files one client holds open at once.
const MAX_FIDS: usize = 1024;

/// Streams served at once, machine-wide.
const MAX_STREAMS: usize = 64;

/// What one turn of a stream appends at most.
const STREAM_READ: usize = 64 * 1024;

/// Directories one server serves: DATA's four, with room.
const MAX_DIRS: usize = 32;

// One wait watches every acceptor, connection and stream at once.
const _: () = assert!(MAX_SERVED + MAX_HANDSHAKES + MAX_STREAMS + MAX_DIRS <= Poller::MAX_HANDLES as usize);

/// How long an accepted connection may take to lend its window.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// The volume memory stands in for when DATA has no partition: 1 GiB of
/// blocks, of which only what is written costs anything.
const RAM_BLOCKS: u64 = 1 << 18;

/// `--end-on <path>`: a write through a file opened to append at `path` on the
/// volume ends this process once the volume has taken it and before it is
/// answered — a server killed with a write done and unanswered, as a test
/// stages one. Armed by nothing but a boot config's `args`.
const END_ON: &str = "--end-on";

/// `--end-at-read <path>`, repeatable: the first open of `path` on the volume
/// to read only, this boot, ends this process before it is answered — a
/// server killed under a client that holds a connection, whose retry connects
/// again and waits in the port's queue, as a test stages one. Once a boot per
/// path, across restarts: the kernel's `/tmp` keeps the mark. Armed by nothing
/// but a boot config's `args`.
const END_AT_READ: &str = "--end-at-read";

/// `--end-at-mount <role>`: that role's first server this boot, its volume
/// mounted, waits for a connection and ends without accepting it — a server
/// gone before its first answer, under a client already waiting on it, as a
/// test stages one. Once a boot, as [`END_AT_READ`] is. Armed by nothing but a
/// boot config's `args`.
const END_AT_MOUNT: &str = "--end-at-mount";

/// How long [`END_AT_MOUNT`] waits for the connection it ends under.
const END_AT_MOUNT_WAIT: Duration = Duration::from_secs(60);

const TOKEN_CLIENT: u64 = 1 << 32;
const TOKEN_STREAM: u64 = 2 << 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Data,
    Log,
    Boot,
}

impl Role {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "data" => Some(Self::Data),
            "log" => Some(Self::Log),
            "boot" => Some(Self::Boot),
            _ => None,
        }
    }
}

/// One directory this server serves.
struct Capability {
    /// Its absolute name, `/home`.
    dir: String,
    /// Where it is on the volume.
    root: String,
    acceptor: Acceptor,
}

/// One file a client holds open.
struct Fid {
    node: Node,
    write: bool,
    append: bool,
    /// Opened to append at [`END_ON`]'s path.
    ends: bool,
}

struct Client {
    conn: Connection,
    rx: ipc::FrameRx<{ core::mem::size_of::<Request>() }>,
    cap: usize,
    window: Option<SharedMemory>,
    fids: BTreeMap<u64, Fid>,
    next_fid: u64,
    since: Instant,
}

/// A pipe a program writes and this server appends to a file.
struct Stream {
    pipe: Pipe,
    node: Node,
    offset: u64,
}

/// Local seconds since the epoch, the zone FAT stamps in: bracketed so both
/// readings are of one second, as `userland/logd/src/wall.rs` does.
fn local_secs() -> u64 {
    for _ in 0..4 {
        let (Some(before), Some(local), Some(after)) = (
            toyos_abi::syscall::clock_epoch(),
            toyos_abi::syscall::clock_realtime(),
            toyos_abi::syscall::clock_epoch(),
        ) else {
            return 0;
        };
        if before != after {
            continue;
        }
        let lsod = local.hours as u64 * 3600 + local.minutes as u64 * 60 + local.seconds as u64;
        return match toyos_wallclock::resolve(before, lsod) {
            toyos_wallclock::Recovery::Offset(off) => before.saturating_add_signed(off),
            // Two real zones a day apart: UTC, which is a time and not a guess.
            toyos_wallclock::Recovery::Ambiguous { .. } => before,
        };
    }
    toyos_abi::syscall::clock_epoch().unwrap_or(0)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let role = args.get(1).and_then(|r| Role::parse(r)).unwrap_or_else(|| {
        panic!("fsd: started with {args:?}; the first argument is a role: data, log or boot")
    });
    let (mut guid, mut end_on, mut end_at_read, mut end_at_mount) = (None, None, Vec::new(), None);
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            END_ON => end_on = Some(rest.next().unwrap_or_else(|| panic!("fsd: {END_ON} takes a path")).clone()),
            END_AT_READ => {
                end_at_read.push(rest.next().unwrap_or_else(|| panic!("fsd: {END_AT_READ} takes a path")).clone())
            }
            END_AT_MOUNT => {
                end_at_mount = Some(
                    rest.next().and_then(|r| Role::parse(r)).unwrap_or_else(|| panic!("fsd: {END_AT_MOUNT} takes a role")),
                )
            }
            flag if flag.starts_with("--") => panic!("fsd: {flag} is no argument of this server"),
            named if guid.is_none() => guid = Some(named),
            extra => panic!("fsd: a second partition, {extra}, after {guid:?}"),
        }
    }
    let caps = capabilities(role);
    let roots: Vec<String> = caps.iter().map(|c| c.root.clone()).collect();
    let roots: Vec<&str> = roots.iter().map(String::as_str).collect();
    let volume = open_volume(role, guid, &roots);
    println!(
        "fsd: {role:?} serving {} — {}",
        caps.iter().map(|c| c.dir.as_str()).collect::<Vec<_>>().join(", "),
        volume.describe()
    );
    if end_at_mount == Some(role) && first_this_boot(&format!("end-at-mount-{role:?}")) {
        end_under_a_waiting_connection(&caps);
    }
    let caps_len = caps.len() as u32;
    Server {
        volume,
        caps,
        clients: BTreeMap::new(),
        next_client: 0,
        streams: BTreeMap::new(),
        next_stream: 0,
        writeback: WriteBack::default(),
        end_on,
        end_at_read,
        probe: Poller::new(caps_len),
        scratch: Vec::new(),
    }
    .serve()
}

/// Every directory init endowed this process an acceptor for.
fn capabilities(role: Role) -> Vec<Capability> {
    let prefix = format!("{SERVE_PREFIX}{CAPABILITY_PREFIX}");
    let labels: Vec<String> =
        Endowments::get().labels().filter(|l| l.starts_with(&prefix)).map(str::to_string).collect();
    let mut caps = Vec::new();
    for label in labels {
        let dir = label[prefix.len()..].to_string();
        let acceptor: Acceptor = Endowments::get().take(&label).expect("fsd: an acceptor its label names");
        let root = match role {
            Role::Data => dir.trim_start_matches('/').to_string(),
            Role::Log | Role::Boot => String::new(),
        };
        caps.push(Capability { dir, root, acceptor });
    }
    assert!(!caps.is_empty(), "fsd: started serving no directory");
    assert!(caps.len() <= MAX_DIRS, "fsd: started serving {} directories, past {MAX_DIRS}", caps.len());
    caps
}

/// The partition claim init minted, when the role's partition is on a disk
/// the kernel drives.
fn claimed() -> Option<Claimed> {
    let prefix = format!("{DEV_PREFIX}part:");
    let label = Endowments::get().labels().find(|l| l.starts_with(&prefix))?.to_string();
    let claim: toyos::PartitionDev = Endowments::get().take(&label)?;
    match Claimed::new(claim) {
        Ok(disk) => Some(disk),
        Err(why) => {
            println!("fsd: the partition claim would not describe itself: {why:?}");
            None
        }
    }
}

/// A session on the partition blockd serves under `guid`.
fn served(guid: [u8; 16]) -> Option<Served> {
    let names = endow::namespace()?;
    let own = toyos::namespace::build().keep(names, &[toyos_blockring::PORT]).finish().ok()?;
    match blockd::Session::open(own, toyos_blockring::PORT, guid) {
        Ok(session) => Some(Served::new(session)),
        Err(why) => {
            println!("fsd: blockd would not open the partition: {why:?}");
            None
        }
    }
}

/// The TOYOS-DATA partitions blockd serves, or why it would not say: a disk
/// that failed is not a machine without one, and is never answered with memory.
fn data_partitions() -> Result<Vec<[u8; 16]>, String> {
    let Some(names) = endow::namespace() else { return Ok(Vec::new()) };
    match blockd::list(names, toyos_blockring::PORT) {
        Ok(listed) => {
            Ok(listed.into_iter().filter(|l| l.kind == toyos_gpt::Guid::TOYOS_DATA.0).map(|l| l.unique).collect())
        }
        // No block service in this image: the namespace has no `block`.
        Err(blockd::Error::Kernel(SyscallError::NotFound)) => Ok(Vec::new()),
        Err(why) => Err(format!("the block service would not list its partitions ({why:?})")),
    }
}

fn open_volume(role: Role, guid: Option<&str>, roots: &[&str]) -> Box<dyn Volume> {
    match role {
        Role::Data => {
            if let Some(disk) = claimed() {
                return data_on(disk, roots);
            }
            match data_partitions().as_deref() {
                Ok([one]) => match served(*one) {
                    Some(disk) => data_on(disk, roots),
                    None => Box::new(Absent::new(roots, "the DATA partition would not open".into())),
                },
                Ok([]) => ram(roots, "this machine has no DATA partition"),
                Ok(many) => ram(roots, &format!("this machine has {} DATA partitions, and a volume is one", many.len())),
                Err(why) => {
                    println!("fsd: {why}; DATA is absent this boot");
                    Box::new(Absent::new(roots, why.clone()))
                }
            }
        }
        Role::Log | Role::Boot => {
            let writable = role == Role::Log;
            let mounted = if let Some(disk) = claimed() {
                Some(fat_on(disk, writable))
            } else {
                guid.and_then(toyos_abi::part::PartGuid::parse).and_then(|g| served(g.0)).map(|disk| fat_on(disk, writable))
            };
            match mounted {
                Some(Ok(volume)) => volume,
                Some(Err(why)) => {
                    println!("fsd: the {role:?} partition does not mount: {why}");
                    Box::new(Absent::new(roots, why))
                }
                None => Box::new(Absent::new(
                    roots,
                    match guid {
                        Some(guid) => format!("the {role:?} partition {guid} is on no disk this server reaches"),
                        None => format!("the loader named no {role:?} partition"),
                    },
                )),
            }
        }
    }
}

fn data_on<D: Disk + 'static>(disk: D, roots: &[&str]) -> Box<dyn Volume> {
    match DataVolume::probe(disk, roots, fsd::volume::now_nanos) {
        Probed::Mounted(volume) => Box::new(volume),
        Probed::Unmountable(why) => {
            println!("fsd: the DATA volume is ours and does not mount ({why}); it is absent this boot");
            Box::new(Absent::new(roots, why))
        }
        Probed::Foreign => ram(roots, "the DATA partition is not ours"),
    }
}

fn ram(roots: &[&str], why: &str) -> Box<dyn Volume> {
    println!("fsd: {why}; /apps, /config, /home and /state are in memory and will not survive a reboot");
    match DataVolume::format(Ram::new(RAM_BLOCKS), roots, fsd::volume::now_nanos) {
        Ok(volume) => Box::new(volume),
        Err(why) => panic!("fsd: a volume in memory would not format: {why}"),
    }
}

fn fat_on<D: Disk + 'static>(disk: D, writable: bool) -> Result<Box<dyn Volume>, String> {
    FatVolume::mount(disk, writable, local_secs).map(|v| Box::new(v) as Box<dyn Volume>)
}

struct Server {
    volume: Box<dyn Volume>,
    caps: Vec<Capability>,
    clients: BTreeMap<u64, Client>,
    next_client: u64,
    streams: BTreeMap<u64, Stream>,
    next_stream: u64,
    /// When the sync nobody asked for is due.
    writeback: WriteBack,
    /// [`END_ON`]'s path on the volume.
    end_on: Option<String>,
    /// [`END_AT_READ`]'s paths on the volume.
    end_at_read: Vec<String>,
    /// Asks an acceptor whether a connection waits, before [`Server::accept`]
    /// takes it.
    probe: Poller,
    /// A write's bytes, copied out of the client's window or a stream's pipe
    /// into this process's own memory before the volume sees them. Kept, so a
    /// write allocates nothing.
    scratch: Vec<u8>,
}

/// What one request is answered.
enum Answer {
    Reply(Reply),
    /// A path the client resolves again, `len` bytes of the window.
    Link(usize),
    /// A reply that carries a handle.
    WithHandle(Reply, toyos::RawHandle),
    /// The client broke the protocol and is let go.
    Drop(&'static str),
    /// The client is answered this refusal and let go, for this reason.
    Refuse(SyscallError, String),
}

fn stat_reply(meta: Meta) -> Reply {
    Reply { kind: meta.kind.wire(), value2: meta.size, mtime: meta.mtime, ..Reply::ok() }
}

/// Whether this is an actuator's first firing this boot for `what`: its mark
/// is made once in the kernel's `/tmp`, which outlives this process and not
/// the boot. Looked for and then made, not made exclusively — the std fork's
/// `create_new` on a kernel path is not exclusive — which one server of a role
/// at a time makes safe. A mark that can be neither found nor made is an
/// actuator that cannot do its one job.
fn first_this_boot(what: &str) -> bool {
    let mark = format!("/tmp/fsd-{}", what.replace('/', "-"));
    match std::fs::metadata(&mark) {
        Ok(_) => false,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&mark, b"").unwrap_or_else(|e| panic!("fsd: an actuator's mark {mark}: {e}"));
            true
        }
        Err(e) => panic!("fsd: an actuator's mark {mark} would not be looked up: {e}"),
    }
}

/// [`END_AT_MOUNT`]'s end: once a connection waits on one of `caps`' ports,
/// and before it is taken.
fn end_under_a_waiting_connection(caps: &[Capability]) -> ! {
    let poller = Poller::new(caps.len() as u32);
    for (i, cap) in caps.iter().enumerate() {
        poller.watch(&cap.acceptor, READABLE, i as u64);
    }
    let mut waiting = false;
    poller.wait(1, END_AT_MOUNT_WAIT.as_nanos() as u64, |_| waiting = true);
    assert!(waiting, "fsd: {END_AT_MOUNT}: no connection came in {} s", END_AT_MOUNT_WAIT.as_secs());
    println!("fsd: {END_AT_MOUNT}: ending with a connection waiting and unaccepted");
    std::process::exit(1);
}

fn window_of(w: &SharedMemory) -> Window {
    // SAFETY: the region is `WINDOW_BYTES` long (it came through `adopt` at
    // that size, and shared memory is never less) and lives as long as `w`.
    unsafe { Window::new(w.as_ptr(), WINDOW_BYTES) }
}

/// A read's destination: the first bytes of a client's window, which the
/// volume copies its cache's blocks into once and never holds a reference to.
struct WindowOut(Window);

impl Out for WindowOut {
    fn len(&self) -> usize {
        self.0.bytes()
    }

    fn put(&mut self, at: usize, bytes: &[u8]) {
        window_put(self.0, at, bytes);
    }

    fn zero(&mut self, at: usize, len: usize) {
        self.0.sub(at, len).zero();
    }
}

impl Server {
    fn serve(mut self) -> ! {
        let poller = Poller::new(Poller::MAX_HANDLES);
        let mut ready = Vec::new();
        loop {
            if self.clients.values().filter(|c| c.window.is_none()).count() < MAX_HANDSHAKES {
                for (i, cap) in self.caps.iter().enumerate() {
                    poller.watch(&cap.acceptor, READABLE, i as u64);
                }
            }
            for (id, c) in &self.clients {
                poller.watch(&c.conn, READABLE, TOKEN_CLIENT + id);
            }
            for (id, s) in &self.streams {
                poller.watch(&s.pipe, READABLE, TOKEN_STREAM + id);
            }
            let now = Instant::now();
            let timeout = self
                .clients
                .values()
                .filter(|c| c.window.is_none())
                .map(|c| HANDSHAKE_TIMEOUT.saturating_sub(now.duration_since(c.since)))
                .chain(self.writeback.left(now))
                .min()
                .map_or(u64::MAX, |left| left.as_nanos() as u64);
            ready.clear();
            poller.wait(1, timeout, |t| ready.push(t));

            for &token in &ready {
                if token < TOKEN_CLIENT {
                    self.accept(token as usize);
                } else if token < TOKEN_STREAM {
                    self.pump(token - TOKEN_CLIENT);
                } else {
                    self.drain(token - TOKEN_STREAM);
                }
            }
            let now = Instant::now();
            let late: Vec<u64> = self
                .clients
                .iter()
                .filter(|(_, c)| c.window.is_none() && now.duration_since(c.since) >= HANDSHAKE_TIMEOUT)
                .map(|(id, _)| *id)
                .collect();
            for id in late {
                self.drop_client(id, "it never lent its window");
            }
            if self.writeback.take_due(now) {
                if let Err(e) = self.sync(None) {
                    println!("fsd: the write-back sync failed: {e:?}");
                }
            }
        }
    }

    /// Take a connection that waits on `cap`'s port, if one does.
    ///
    /// **Asked first, because `accept` parks and a completion is a hint**: a
    /// watch replaced while a connection arrives can answer beside the watch
    /// that replaced it, so two completions name one connection, and the
    /// second `accept` would park this server for good. This process is the
    /// port's one acceptor, so a connection the probe sees is still there to
    /// take. The probe's ring is drained whole each time and a completion is
    /// read by its port's token, so what counts is an arrival on this port
    /// since its last probe, none of them taken.
    fn accept(&mut self, cap: usize) {
        self.probe.watch(&self.caps[cap].acceptor, READABLE, cap as u64);
        let mut waiting = false;
        self.probe.wait(0, 0, |token| waiting |= token == cap as u64);
        if !waiting {
            return;
        }
        let conn = match self.caps[cap].acceptor.accept() {
            Ok(conn) => conn,
            Err(why) => panic!("fsd: its own acceptor refused an accept: {why:?}"),
        };
        let id = self.next_client;
        self.next_client += 1;
        let client = Client {
            conn,
            rx: ipc::FrameRx::new(),
            cap,
            window: None,
            fids: BTreeMap::new(),
            next_fid: 1,
            since: Instant::now(),
        };
        self.clients.insert(id, client);
    }

    fn drop_client(&mut self, id: u64, why: &str) {
        let Some(client) = self.clients.remove(&id) else { return };
        if !why.is_empty() {
            println!("fsd: dropping client {id} of {}: {why}", self.caps[client.cap].dir);
        }
        for fid in client.fids.values() {
            // Nobody is left to answer: a refused close is said, and its node
            // kept for the next sync.
            let _ = self.volume.close(fid.node);
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
            RxStep::Frame { msg_type, payload_len } => {
                let Ok(request) = ipc::decode_payload::<Request>(client.rx.payload(payload_len)) else {
                    return self.drop_client(id, "its request was short");
                };
                let answer = self.answer(id, msg_type, request);
                self.reply(id, answer);
            }
        }
    }

    /// Send `answer`, or let the client go when it will not take it.
    fn reply(&mut self, id: u64, answer: Answer) {
        let Some(client) = self.clients.get(&id) else { return };
        let sent = match answer {
            Answer::Reply(reply) => client.conn.try_send(REPLY, &reply),
            Answer::Link(len) => client.conn.try_send(LINK, &Reply { value: len as u64, ..Reply::ok() }),
            Answer::WithHandle(reply, handle) => client.conn.try_send_with_handles(&[handle], REPLY, &reply),
            Answer::Drop(why) => return self.drop_client(id, why),
            Answer::Refuse(e, why) => {
                // Let go whether or not the refusal went: either way it is said.
                let _ = client.conn.try_send(REPLY, &Reply::refused(e));
                return self.drop_client(id, &why);
            }
        };
        if let Err(why) = sent {
            self.drop_client(id, &format!("its connection would not take a reply ({why:?})"));
        }
    }

    /// A path from the window: `len` bytes at `at`, UTF-8.
    fn path(&self, id: u64, at: u64, len: u64) -> Result<String, SyscallError> {
        let window = self.clients[&id].window.as_ref().ok_or(SyscallError::InvalidArgument)?;
        let (at, len) = (at as usize, len as usize);
        if len > MAX_PATH || at.checked_add(len).is_none_or(|end| end > WINDOW_BYTES) {
            return Err(SyscallError::InvalidArgument);
        }
        let mut bytes = vec![0u8; len];
        window_take(window_of(window), at, &mut bytes);
        String::from_utf8(bytes).map_err(|_| SyscallError::InvalidArgument)
    }

    /// `rel`, from the client's directory, as a volume path.
    fn resolve(&mut self, id: u64, rel: &str, follow_last: bool) -> Result<Resolved, SyscallError> {
        if !canonical(rel) {
            return Err(SyscallError::InvalidArgument);
        }
        let root = self.caps[self.clients[&id].cap].root.clone();
        let volume = &mut self.volume;
        let mut lookup = |p: &str| match volume.lstat(p) {
            Ok(meta) if meta.kind == Kind::Symlink => volume.read_link(p).map(Found::Link).map_err(drop),
            Ok(_) | Err(SyscallError::NotFound) => Ok(Found::Other),
            Err(_) => Err(()),
        };
        resolve::resolve(&root, rel, follow_last, &mut lookup).map_err(|e| match e {
            Escape::Escape => SyscallError::PermissionDenied,
            Escape::Loop => SyscallError::InvalidArgument,
            Escape::NotFound => SyscallError::NotFound,
            Escape::Io => SyscallError::Io,
        })
    }

    /// Put `bytes` in the client's window at 0.
    fn put(&self, id: u64, bytes: &[u8]) {
        let window = self.clients[&id].window.as_ref().expect("a request comes after the window");
        window_put(window_of(window), 0, bytes);
    }

    fn answer(&mut self, id: u64, op: u32, r: Request) -> Answer {
        if op == HELLO {
            let served = self.clients.values().filter(|c| c.window.is_some()).count();
            let client = self.clients.get_mut(&id).expect("pumped");
            if client.window.is_some() {
                return Answer::Drop("it lent a second window");
            }
            if served >= MAX_SERVED {
                let why = format!("it is refused, since {MAX_SERVED} clients are served already");
                return Answer::Refuse(SyscallError::ResourceExhausted, why);
            }
            let Some([lent]) = client.conn.recv_handles_exact::<1>() else {
                return Answer::Drop("its hello carried no window");
            };
            match SharedMemory::adopt(lent, WINDOW_BYTES) {
                Ok(window) => client.window = Some(window),
                Err(_) => return Answer::Drop("its window would not map"),
            }
            let rights = if self.volume.writable() { RIGHT_WRITE } else { 0 };
            return Answer::Reply(Reply { value: rights, ..Reply::ok() });
        }
        if self.clients[&id].window.is_none() {
            return Answer::Drop("it asked before it lent a window");
        }
        match self.serve_one(id, op, r) {
            Ok(answer) => answer,
            Err(e) => Answer::Reply(Reply::refused(e)),
        }
    }

    fn fid(&self, id: u64, fid: u64) -> Result<&Fid, SyscallError> {
        self.clients[&id].fids.get(&fid).ok_or(SyscallError::InvalidArgument)
    }

    /// Mark the volume written: the write-back sync is due from here.
    fn dirtied(&mut self) {
        self.writeback.dirtied(Instant::now());
    }

    /// `rel` resolved without following its last component, for an operation
    /// on the name itself; an absolute link on the way is the client's.
    fn name(&mut self, id: u64, rel: &str) -> Result<Result<String, Answer>, SyscallError> {
        Ok(match self.resolve(id, rel, false)? {
            Resolved::Path(p) => Ok(p),
            Resolved::Absolute(p) => Err(self.link(id, &p)),
        })
    }

    fn serve_one(&mut self, id: u64, op: u32, r: Request) -> Result<Answer, SyscallError> {
        let changes = matches!(op, WRITE | TRUNCATE | MKDIR | RMDIR | UNLINK | RENAME | SYMLINK | STREAM)
            || (op == OPEN && r.flags & (O_WRITE | O_APPEND | O_CREATE | O_TRUNCATE | O_CREATE_NEW) != 0);
        if changes && !self.volume.writable() {
            return Err(SyscallError::PermissionDenied);
        }
        match op {
            OPEN => {
                if r.flags & !O_KNOWN != 0 {
                    return Err(SyscallError::InvalidArgument);
                }
                let rel = self.path(id, 0, r.len)?;
                let path = match self.resolve(id, &rel, true)? {
                    Resolved::Absolute(p) => return Ok(self.link(id, &p)),
                    Resolved::Path(p) => p,
                };
                let reads_only = r.flags & (O_WRITE | O_APPEND | O_CREATE | O_TRUNCATE | O_CREATE_NEW) == 0;
                if reads_only && self.end_at_read.contains(&path) && first_this_boot(&format!("end-at-read-{path}")) {
                    println!("fsd: {END_AT_READ}: ending before the first read of {path} is answered");
                    std::process::exit(1);
                }
                if self.clients[&id].fids.len() >= MAX_FIDS {
                    return Err(SyscallError::ResourceExhausted);
                }
                let how = OpenHow {
                    create: r.flags & O_CREATE != 0,
                    create_new: r.flags & O_CREATE_NEW != 0,
                    truncate: r.flags & O_TRUNCATE != 0,
                };
                let node = self.volume.open(&path, how)?;
                let meta = match self.volume.node_meta(node) {
                    Ok(meta) => meta,
                    Err(e) => {
                        let _ = self.volume.close(node);
                        return Err(e);
                    }
                };
                if how.create || how.truncate || how.create_new {
                    self.dirtied();
                }
                let client = self.clients.get_mut(&id).expect("pumped");
                let fid = client.next_fid;
                client.next_fid += 1;
                let write = r.flags & (O_WRITE | O_APPEND) != 0;
                let append = r.flags & O_APPEND != 0;
                let ends = append && self.end_on.as_deref() == Some(path.as_str());
                let client = self.clients.get_mut(&id).expect("pumped");
                client.fids.insert(fid, Fid { node, write, append, ends });
                Ok(Answer::Reply(Reply { value: fid, ..stat_reply(meta) }))
            }
            CLOSE => {
                let client = self.clients.get_mut(&id).expect("pumped");
                let fid = client.fids.remove(&r.fid).ok_or(SyscallError::InvalidArgument)?;
                self.volume.close(fid.node)?;
                Ok(Answer::Reply(Reply::ok()))
            }
            READ => {
                let node = self.fid(id, r.fid)?.node;
                let len = (r.len as usize).min(WINDOW_BYTES);
                let window = window_of(self.clients[&id].window.as_ref().expect("lent")).sub(0, len);
                let n = self.volume.read(node, r.offset, &mut WindowOut(window))?;
                Ok(Answer::Reply(Reply { value: n as u64, ..Reply::ok() }))
            }
            WRITE => {
                let (node, write, append, ends) = {
                    let f = self.fid(id, r.fid)?;
                    (f.node, f.write, f.append, f.ends)
                };
                if !write {
                    return Err(SyscallError::PermissionDenied);
                }
                let len = r.len as usize;
                if len > WINDOW_BYTES {
                    return Err(SyscallError::InvalidArgument);
                }
                if self.scratch.len() < len {
                    self.scratch.resize(len, 0);
                }
                let window = window_of(self.clients[&id].window.as_ref().expect("lent"));
                window_take(window, 0, &mut self.scratch[..len]);
                let at = if append { self.volume.node_meta(node)?.size } else { r.offset };
                if at.checked_add(len as u64).is_none_or(|end| end > MAX_FILE_BYTES) {
                    return Err(SyscallError::InvalidArgument);
                }
                self.volume.write(node, at, &self.scratch[..len])?;
                if ends {
                    println!("fsd: {END_ON}: ending with a write done and unanswered");
                    std::process::exit(1);
                }
                self.dirtied();
                Ok(Answer::Reply(Reply { value: len as u64, value2: at + len as u64, ..Reply::ok() }))
            }
            FSTAT => {
                let node = self.fid(id, r.fid)?.node;
                let meta = self.volume.node_meta(node)?;
                Ok(Answer::Reply(stat_reply(meta)))
            }
            TRUNCATE => {
                let f = self.fid(id, r.fid)?;
                if !f.write {
                    return Err(SyscallError::PermissionDenied);
                }
                let node = f.node;
                if r.offset > MAX_FILE_BYTES {
                    return Err(SyscallError::InvalidArgument);
                }
                self.volume.truncate(node, r.offset)?;
                self.dirtied();
                Ok(Answer::Reply(Reply::ok()))
            }
            FSYNC => {
                let node = self.fid(id, r.fid)?.node;
                self.sync(Some(node))
            }
            SYNC => self.sync(None),
            STREAM => {
                let f = self.fid(id, r.fid)?;
                if !f.write {
                    return Err(SyscallError::PermissionDenied);
                }
                // Every client number this server keeps is bounded where it is
                // kept: a stream's offset grows by what it drains.
                if r.offset > MAX_FILE_BYTES {
                    return Err(SyscallError::InvalidArgument);
                }
                let node = f.node;
                if self.streams.len() >= MAX_STREAMS {
                    return Err(SyscallError::ResourceExhausted);
                }
                let (read, write) = toyos::pipe_pair()?;
                self.volume.hold(node);
                let sid = self.next_stream;
                self.next_stream += 1;
                self.streams.insert(sid, Stream { pipe: read, node, offset: r.offset });
                Ok(Answer::WithHandle(Reply::ok(), write.into_raw()))
            }
            STAT | LSTAT => {
                let rel = self.path(id, 0, r.len)?;
                match self.resolve(id, &rel, op == STAT)? {
                    Resolved::Absolute(p) => Ok(self.link(id, &p)),
                    Resolved::Path(p) => Ok(Answer::Reply(stat_reply(self.volume.lstat(&p)?))),
                }
            }
            READDIR => {
                let rel = self.path(id, 0, r.len)?;
                let dir = match self.resolve(id, &rel, true)? {
                    Resolved::Absolute(p) => return Ok(self.link(id, &p)),
                    Resolved::Path(p) => p,
                };
                let entries = self.volume.list(&dir)?;
                let mut out = Vec::new();
                let mut entry = vec![0u8; 11 + MAX_PATH];
                for (name, meta) in &entries {
                    let n = encode_entry(&mut entry, meta.kind.wire(), meta.size, name)
                        .ok_or(SyscallError::InvalidArgument)?;
                    out.extend_from_slice(&entry[..n]);
                }
                // Refused rather than cut short: a listing that stops early is a
                // confidently wrong answer to a caller deleting a tree.
                if out.len() > WINDOW_BYTES {
                    return Err(SyscallError::ResourceExhausted);
                }
                self.put(id, &out);
                Ok(Answer::Reply(Reply { value: out.len() as u64, ..Reply::ok() }))
            }
            READLINK => {
                let rel = self.path(id, 0, r.len)?;
                let path = match self.name(id, &rel)? {
                    Ok(p) => p,
                    Err(link) => return Ok(link),
                };
                let target = self.volume.read_link(&path)?;
                self.put(id, target.as_bytes());
                Ok(Answer::Reply(Reply { value: target.len() as u64, ..Reply::ok() }))
            }
            MKDIR | RMDIR | UNLINK => {
                let rel = self.path(id, 0, r.len)?;
                if rel.is_empty() {
                    return Err(if op == MKDIR { SyscallError::AlreadyExists } else { SyscallError::PermissionDenied });
                }
                let path = match self.name(id, &rel)? {
                    Ok(p) => p,
                    Err(link) => return Ok(link),
                };
                match op {
                    MKDIR => self.volume.mkdir(&path)?,
                    RMDIR => self.volume.rmdir(&path)?,
                    _ => self.volume.unlink(&path)?,
                }
                self.dirtied();
                Ok(Answer::Reply(Reply::ok()))
            }
            RENAME => {
                let from_rel = self.path(id, 0, r.len)?;
                let to_rel = self.path(id, r.len, r.len2)?;
                if from_rel.is_empty() || to_rel.is_empty() {
                    return Err(SyscallError::PermissionDenied);
                }
                let (Resolved::Path(from), Resolved::Path(to)) =
                    (self.resolve(id, &from_rel, false)?, self.resolve(id, &to_rel, false)?)
                else {
                    // A rename through a link that leaves this directory is a
                    // rename across two, which no one directory can do.
                    return Err(SyscallError::NotSupported);
                };
                self.volume.rename(&from, &to)?;
                self.dirtied();
                Ok(Answer::Reply(Reply::ok()))
            }
            SYMLINK => {
                let link_rel = self.path(id, 0, r.len)?;
                let target = self.path(id, r.len, r.len2)?;
                if target.is_empty() || link_rel.is_empty() {
                    return Err(SyscallError::InvalidArgument);
                }
                let Resolved::Path(link) = self.resolve(id, &link_rel, false)? else {
                    return Err(SyscallError::NotSupported);
                };
                self.volume.symlink(&link, &target)?;
                self.dirtied();
                Ok(Answer::Reply(Reply::ok()))
            }
            _ => Ok(Answer::Drop("it asked for an operation this protocol does not have")),
        }
    }

    /// The volume synced, answered as `node`'s file fared, or with none as
    /// every file did. A file whose entry was refused keeps the write-back
    /// due, so the next one tries it again.
    fn sync(&mut self, node: Option<Node>) -> Result<Answer, SyscallError> {
        let unwritten = self.volume.sync()?;
        self.writeback.synced(!unwritten.is_empty(), Instant::now());
        match unwritten.into_iter().find(|&(n, _)| node.is_none_or(|node| n == node)) {
            Some((_, e)) => Err(e),
            None => Ok(Answer::Reply(Reply::ok())),
        }
    }

    fn link(&self, id: u64, path: &str) -> Answer {
        if path.len() > MAX_PATH {
            return Answer::Reply(Reply::refused(SyscallError::InvalidArgument));
        }
        self.put(id, path.as_bytes());
        Answer::Link(path.len())
    }

    /// One read of what a stream's writer has put in the pipe, appended to
    /// its file, and no more: a pipe with more waiting fires at the next
    /// wait, after every other client that was ready — [`Self::pump`]'s rule.
    fn drain(&mut self, sid: u64) {
        if self.scratch.len() < STREAM_READ {
            self.scratch.resize(STREAM_READ, 0);
        }
        let Some(stream) = self.streams.get_mut(&sid) else { return };
        let ended = match stream.pipe.read_nonblock(&mut self.scratch[..STREAM_READ]) {
            Err(SyscallError::WouldBlock) => return,
            Ok(0) | Err(SyscallError::Gone) => None,
            Err(e) => Some(format!("its pipe would not read ({e:?})")),
            Ok(n) => {
                let (node, at) = (stream.node, stream.offset);
                match at.checked_add(n as u64).filter(|&end| end <= MAX_FILE_BYTES) {
                    None => Some("it reached the largest offset a file has".to_string()),
                    Some(end) => {
                        stream.offset = end;
                        match self.volume.write(node, at, &self.scratch[..n]) {
                            Ok(()) => {
                                self.dirtied();
                                return;
                            }
                            Err(e) => Some(format!("its write failed ({e:?})")),
                        }
                    }
                }
            }
        };
        if let Some(why) = ended {
            println!("fsd: a stream is ended: {why}");
        }
        if let Some(stream) = self.streams.remove(&sid) {
            // Nobody is left to answer: a refused close is said, and its node
            // kept for the next sync.
            let _ = self.volume.close(stream.node);
        }
    }
}
