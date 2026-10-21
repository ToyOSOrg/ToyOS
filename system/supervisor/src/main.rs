//! The one program the kernel starts, and the only holder of the machine's
//! system capability.
//!
//! Everything else is started from here, holding exactly what
//! `/system/etc/system.manifest` says it holds. **Every port exists before any server
//! runs**: the supervisor creates one per `serves` name in the whole manifest, then
//! builds each program's namespace out of the connectors and spawns it with the
//! acceptor moved in. So a client's connection works from its first
//! instruction whether or not the server has reached `accept` or has even been
//! spawned, there is no instant at which a name is not bound yet, and there is
//! nothing anywhere to retry.
//!
//! **Every program it starts writes its stdout and stderr into a log ring of its
//! own, which the supervisor hands `logkeeper` under the manifest's name for it and its pid**
//! ([`Log`]). That is what makes a line's origin structure rather than a claim:
//! the name travels beside the ring, and no program holds the connection it
//! travels on. A program launched through `launcher` gets a ring of its own in
//! place of any ring its caller passed as its stdout or stderr, and keeps
//! anything else the caller passed — a terminal's pipes stay the terminal's.
//!
//! **The supervisor sequences the machine's stop** ([`toyos::power`]): a holder of the
//! `power` connector asks, the supervisor has `logkeeper` flush and answer, and only then
//! asks the kernel, which stops every process at once and waits for none.
//!
//! **A service the supervisor starts at boot outlives any one process serving it.** The supervisor
//! keeps each of its acceptors and endows a duplicate, so a restart or a swap
//! ([`toyos_swap`]) can stop the process, start another binary holding the same
//! manifest row, and leave every client's connector naming the same port — a
//! connect made in the gap waits in that port's queue for the new process. A
//! service that ends with no swap owning it and no restart owed has its kept
//! acceptors closed at once, so a client's next connect is `ServerGone` exactly
//! as it is for a server nothing keeps. A program started through `launcher`
//! still gets its acceptor by move and is started once per boot.
//!
//! **A `drives` row has one process per PCI function it names**, each holding
//! that function's claim and started when the inventory shows the function
//! free ([`toyos_supervisor::drives`]). **Every end is readiness on the
//! process's handle**, watched on the loop's own poller: the supervisor reads
//! the inventory again after each, and what an end owes is
//! [`toyos_supervisor`]'s to decide.
//!
//! **The supervisor never waits on a file server it supervises from its loop alone**:
//! the loop is also where a server that ended is started again. Its calls into
//! the file servers run on one worker thread ([`Worker`]) — a launch's files
//! included, read there before its spawn (`CommandExt::prepare`) — and the supervisor
//! waits on the call and on a service's end together ([`Supervisor::files`]), so a
//! call a server's end left in the port's queue goes on to the server started
//! in its place, and one alive and silent costs a bounded wait
//! ([`FILES_BOUND`]).
//!
//! **Every program it starts gets `HOME` from its row** (`Program::home`), over
//! anything a launching caller carried: a service its own `/state/<name>` and
//! an installed package its own `Apps/<name>` folder of the session user's
//! home, each made before it runs, and everything else the session user's
//! home, made at boot.
//!
//! **Every program it starts holds the directories its row's view names**
//! (`Program::view`), each a grant minted for that start ([`Grants`]): an
//! installed package its own directory read-only and its own folder
//! read-write, and every other row the whole tree.
//! A launch of a program no row names is answered with the session's, which
//! the caller's direct spawn carries in place of its own.
//!
//! **Every program whose row receives `netstack` holds one minted for it**,
//! with its row and session as the badge ([`Grants::netstack`]), which netstack
//! keeps each program's DNS answers apart by: nothing holds the port's own
//! connector, and a program started once its server has ended for good is
//! minted none.
//!
//! **An installed package's launch also holds the folder of the home it was
//! granted** ([`toyos_manifest::grants`]): the answer stored for the exact
//! bytes it starts, which the supervisor alone writes, at the request of the
//! one row holding the `grants` port from a login session or as the person at
//! the screen's answer, and starts in it. A launch with no stored answer in a
//! session that holds the screen waits for that answer ([`Parked`]), asked on
//! [`toyos_manifest::consent::PORT`], which only the supervisor connects to.
//!
//! **A launch starts only what its caller's row lists** ([`toyos_manifest::launch`]).
//! Every row whose `starts` lists anything is endowed a `launcher` the supervisor
//! minted with its row and session as the badge, under a label of its own so
//! no direct spawn inherits it; the badge on a launch's connection is who asks,
//! and nothing the caller says is.
//!
//! **A launched program is spawned under the place its request names** — a
//! copy of the caller's `self` — so the caller's end takes it down; a request
//! that asks for the supervisor is the one way to outlive the caller, and one naming
//! neither is refused.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::os::toyos::process::{ChildExt, CommandExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use toyos_swap::{Refusal, Request as SwapRequest, Word};

use toyos_manifest::consent;
use toyos_manifest::grants::{self, Answer, Decision, Store};
use toyos_manifest::launch::{self as authority, Authority, Session, Sessions, Target};
use toyos_manifest::package::{self, Package};
use toyos_manifest::device::{Entry, PciMatch};
use toyos_manifest::{Manifest, Program, View};
use toyos_supervisor::claims::{self, Missing, Unresolved};
use toyos_supervisor::drives::{self, Act, Drives};
use toyos_supervisor::restart::{self, AfterEnd, Budget};
use toyos::endow::Endowments;
use toyos::fs::{Access, Grant, CAPABILITY_PREFIX};
use toyos::ipc::{self, Connection, RxStep};
use toyos::launch::{self, Parent, Request, LAUNCHER};
use toyos::namespace::{self, Namespace};
use toyos::poller::{Poller, READABLE};
use toyos::port::{self, Acceptor, Connector};
use toyos::log::region::{Ring, RING_BYTES};
use toyos::power::{self, Stop};
use toyos::say;
use toyos::shm::SharedMemory;
use toyos::syscap::SysCap;
use toyos::{AsHandle, Pipe};
use toyos_abi::Rights;
use toyos_blockring::wire::{Grant as BlockGrant, Scope};
use toyos_logstream::{
    Registration, Tag, ALIVE, CONSOLE, FLUSH, FLUSHED, LOGKEEPER, MAX_TAG, ORIGINS, REGISTER, RESUME, STOPPING,
};
use toyos_abi::inventory::{Arrival, Pci, PciAddr, Record};
use toyos_abi::syscall::{
    DeviceRequest, FileType, SyscallError, DEV_PREFIX, MAX_BADGE, PROVIDE_PREFIX, SERVE_PREFIX,
    SVC_LABEL, SYSCAP_LABEL,
};

/// netstack's port, whose every connector the supervisor mints for one row and
/// session (`Grants::netstack`).
const NETSTACK: &str = "netstack";

/// What the supervisor makes in the session user's home before anything runs. English on
/// disk in every locale: a translation is a label, never a rename.
const HOME_FOLDERS: [&str; 8] =
    ["Apps", "Desktop", "Documents", "Downloads", "Fonts", "Music", "Pictures", "Videos"];

/// Connections accepted and not yet carrying a whole launch.
///
/// **The bound is the supervisor's handle table, not memory.** A connection nobody has
/// spoken on costs a `PendingConnection` and no ring page, so what this stops a
/// client from doing is filling the one table the machine cannot do without.
/// Thirty-two is the kernel's own per-port queue depth
/// (`MAX_PENDING_CONNECTIONS`), which is the allowance one step earlier on the
/// same path — past it the supervisor refuses by name rather than growing.
const MAX_PENDING_LAUNCHES: usize = 32;

/// How long an accepted connection may go without completing its launch.
///
/// Policy, and generous: every caller sends its frame in the statement after
/// `connect` (`toyos::launch::launch`). What this bounds is the one that never
/// sends it, and it is what guarantees the table above drains.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// One caller's inbound framing.
///
/// **The supervisor never reads a client with a blocking read.** `recv_header` and
/// `recv_bytes` park the caller until the peer sends the bytes it promised. The
/// whole request blob is kept because a launch carries the child's argv,
/// environment and working directory, and a truncated one is a launch refused
/// for a reason the caller did not cause.
type LaunchRx = ipc::FrameRx<{ ipc::MAX_FRAME_LEN as usize }>;

/// A connection that has been accepted and has not yet said what to start.
///
/// It exists because accept and the request frame are two events.
struct Pending {
    conn: Connection,
    rx: LaunchRx,
    since: Instant,
    /// Which of the supervisor's ports it came in on, which decides what its frame
    /// may ask for.
    port: Port,
}

/// A launch's caller: the row and session the badge on its connection names.
struct Caller {
    row: &'static Program,
    session: Session,
}

enum Port {
    /// Who asks, off the connection's badge.
    Launcher(Caller),
    /// Who asks to read or change the grants, off the connection's badge.
    Grants(Caller),
    Swap,
    Power,
}

/// The ports the supervisor mints a row's badge on, its row and session: the
/// launcher every row whose `starts` lists anything holds, and
/// [`grants::PORT`], which its one holder asks on.
struct Minted {
    launcher: Acceptor,
    grants: Acceptor,
}

/// The poll tokens for the `launcher`, `swap`, `power` and `grants` acceptors, for the
/// one connection whose swap has been answered and whose hang-up is awaited,
/// for every process the supervisor watches the end of, and
/// for the two of the launch waiting on an answer. A
/// pending connection's token is [`TOKEN_PENDING_BASE`] plus its handle, which
/// is unique among the connections the supervisor holds at once.
const TOKEN_ACCEPTOR: u64 = 0;
const TOKEN_SWAP_ACCEPTOR: u64 = 1;
const TOKEN_HANGUP: u64 = 2;
const TOKEN_POWER_ACCEPTOR: u64 = 3;
const TOKEN_ENDED: u64 = 4;
const TOKEN_GRANTS_ACCEPTOR: u64 = 5;
/// The parked launch's caller, and its question's connection ([`Parked`]).
const TOKEN_PARKED_CALLER: u64 = 6;
const TOKEN_CONSENT: u64 = 7;
const TOKEN_PENDING_BASE: u64 = 8;

/// The processes whose end the supervisor watches at once: its services and
/// its `drives` processes, in what a poller holds beside the loop's own
/// watches. A `drives` process past it is not started, and says so.
const MAX_CHILDREN: usize = Poller::MAX_HANDLES as usize - TOKEN_PENDING_BASE as usize - MAX_PENDING_LAUNCHES;

/// How long a stop waits for `logkeeper` to say the log is whole.
///
/// **A policy number**: the flush is a read of every ring, a write and a
/// device flush, and a stick that takes longer than this is one whose
/// last lines this boot gives up on rather than the stop it was asked for.
/// Past it the stop goes ahead, and the lines are on the console.
const FLUSH_BOUND: Duration = Duration::from_millis(toyos_quiesce::FLUSH_MS);

/// The connection the supervisor hands `logkeeper` every program's ring on, and the rest of
/// what the supervisor owns of the log.
///
/// **Blocking sends, bounded by construction**: each is one small frame and
/// two handles, sent once per program the supervisor starts and once for the supervisor itself,
/// so the connection's queues hold them all before `logkeeper` has read one. A
/// refusal is therefore a broken invariant and ends the supervisor loudly.
struct Log {
    conn: Connection,
    /// The acceptor end, until `logkeeper` is started holding it.
    acceptor: Option<Acceptor>,
    /// The supervisor's console as every program but `logkeeper` gets it for its stdin: one
    /// it may read and not write, so no program puts a line on the console but
    /// through `logkeeper` and under its name.
    stdin: toyos::RawHandle,
    /// What `logkeeper` answers on the connection, kept across flushes.
    rx: RefCell<ipc::FrameRx<8>>,
    /// Flushes asked for and not answered yet: one waited out past
    /// [`FLUSH_BOUND`] is answered late, before the next one is.
    unanswered: Cell<u32>,
}

/// The rights a program's stdin console keeps: it reads it and polls it, and a
/// spawn duplicates it; it does not write it.
const STDIN_RIGHTS: Rights =
    Rights::READ.union(Rights::WAIT).union(Rights::DUP).union(Rights::TRANSFER);

/// A fresh log ring, laid out before any other process can map it, which a
/// spawn duplicates into the program and [`Log::register`] names it the owner
/// of. **Owned by no process until then**: the program's pid exists only once
/// it runs, and a child it spawns may write before the supervisor has it.
fn new_ring() -> SharedMemory {
    let region = SharedMemory::create(RING_BYTES).expect("supervisor: no memory for a log ring");
    view(&region).lay_out_with_placeholder_owner();
    region
}

fn view(region: &SharedMemory) -> Ring {
    let base = core::ptr::NonNull::new(region.as_ptr()).expect("a mapped region is not null");
    // SAFETY: `region` maps `RING_BYTES` for as long as it is held, and the
    // view is used only while it is.
    unsafe { Ring::at(base) }
}

impl Log {
    fn open() -> Self {
        let (acceptor, connector) = port::create().expect("supervisor: no port for the log's origins");
        let names = namespace::build()
            .add(ORIGINS, &connector)
            .finish()
            .expect("supervisor: no namespace for the log's origins");
        let conn = names.open(ORIGINS).expect("supervisor: the log's origins port refused the supervisor");
        let stdin = toyos_abi::syscall::dup_narrowed(toyos::RawHandle(0), STDIN_RIGHTS)
            .expect("supervisor: its console would not narrow");
        let log = Self {
            conn,
            acceptor: Some(acceptor),
            stdin,
            rx: RefCell::new(ipc::FrameRx::new()),
            unanswered: Cell::new(0),
        };
        // The supervisor's own lines: a ring like every program's, in the two slots the
        // kernel filled with its console.
        let ring = new_ring();
        for slot in [1, 2] {
            toyos_abi::syscall::dup2(ring.as_handle(), slot).expect("supervisor: its ring would not take its own slot");
        }
        let (alive, keep) = toyos::pipe_pair().expect("supervisor: no pipe to say it lives");
        // Held for the machine's life: the supervisor's end is the machine's.
        core::mem::forget(keep);
        log.register("supervisor", toyos_abi::syscall::getpid().0, ring, alive);
        log
    }

    /// Name program `pid` the owner of `ring`, its log, and move the ring to
    /// `logkeeper` under `name`, with the read end of the pipe whose only writer is
    /// that program.
    fn register(&self, name: &str, pid: u32, ring: SharedMemory, alive: Pipe) {
        let Some(tag) = Tag::new(name) else {
            panic!("supervisor: `{name}` is not a name a line of the log can carry");
        };
        view(&ring).own(pid);
        let ring = ring.share().expect("supervisor: a log ring would not duplicate");
        let mut payload = [0u8; 4 + MAX_TAG];
        let len = Registration { pid, tag }.encode(&mut payload);
        let sent = self.conn.send_handles([ring, alive.into()]).map_err(ipc::IpcError::Syscall);
        if let Err(e) = sent.and_then(|()| self.conn.send_bytes(REGISTER, &payload[..len])) {
            panic!("supervisor: logkeeper's origins connection refused {name}'s ring: {e:?}");
        }
    }

    /// The stop a flush was for was refused: `logkeeper` writes the file again.
    fn resume(&self) {
        if let Err(e) = self.conn.signal(RESUME) {
            toyos::warn!("supervisor: logkeeper could not be told the machine runs on ({e:?}); it is gone");
        }
    }

    /// Have `logkeeper` make the log whole, and wait for it to say so, bounded.
    fn flush(&self) {
        if let Err(e) = self.conn.signal(FLUSH) {
            toyos::warn!("supervisor: logkeeper could not be asked to flush ({e:?}); stopping without it");
            return;
        }
        self.unanswered.set(self.unanswered.get() + 1);
        let poller = Poller::new(1);
        let began = Instant::now();
        loop {
            let step = self.rx.borrow_mut().pump(&self.conn);
            match step {
                RxStep::Frame { msg_type: FLUSHED, .. } => {
                    self.unanswered.set(self.unanswered.get() - 1);
                    if self.unanswered.get() == 0 {
                        return;
                    }
                    continue;
                }
                RxStep::Frame { msg_type, .. } => {
                    panic!("supervisor: logkeeper answered a flush with frame type {msg_type}")
                }
                RxStep::Eof => {
                    toyos::warn!("supervisor: logkeeper is gone, so this stop has no log to flush");
                    return;
                }
                RxStep::Malformed => panic!("supervisor: logkeeper answered a flush with a malformed frame"),
                RxStep::Idle => {}
            }
            let left = FLUSH_BOUND.saturating_sub(began.elapsed());
            if left.is_zero() {
                toyos::warn!(
                    "supervisor: logkeeper did not answer the flush in {} ms; this stop's last lines are on \
                     the console only",
                    FLUSH_BOUND.as_millis()
                );
                return;
            }
            poller.watch(&self.conn, READABLE, 0);
            poller.wait(1, left.as_nanos() as u64, |_| {});
        }
    }
}

/// The session user's home and [`HOME_FOLDERS`]. A boot whose DATA volume did
/// not mount has no `/home`, which its file server has already said; this says
/// what it cost and starts the machine without it.
fn make_session_home() {
    let home = toyos_manifest::session_home();
    if let Err(e) = make_dir(&home) {
        say!("supervisor: {home} could not be made, so this boot has no session home: {e}");
        return;
    }
    for folder in HOME_FOLDERS {
        if let Err(e) = make_dir(&format!("{home}/{folder}")) {
            say!("supervisor: {home}/{folder} could not be made, so the session home has no {folder}: {e}");
        }
    }
}

/// One directory, whose parent is there already.
fn make_dir(path: &str) -> std::io::Result<()> {
    match std::fs::create_dir(path) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => Err(e),
        _ => Ok(()),
    }
}

fn main() {
    // Before anything is started, and before the supervisor says anything: from here
    // the supervisor's own lines are records in its ring.
    let log = Log::open();
    say_release();

    let syscap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("supervisor: the kernel spawns this program holding the system capability");

    let bytes = read_root(toyos_manifest::GUEST_PATH)
        .unwrap_or_else(|e| panic!("supervisor: cannot read {}: {e:?}", toyos_manifest::GUEST_PATH));
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|_| panic!("supervisor: {} is not text", toyos_manifest::GUEST_PATH));
    // For the machine's life, and `'static` so a launch's path can be resolved
    // against it on the supervisor's file worker.
    let system: &'static Manifest = Box::leak(Box::new(toyos_manifest::parse(&text)));

    // Before anything is spawned, and for every `serves` name in the manifest
    // rather than only the ones `[boot] start` names: the filepicker is
    // launched by the compositor, and an editor holding its connector must be
    // able to ask for a file before the picker has run an instruction.
    let mut acceptors = BTreeMap::new();
    let mut connectors: BTreeMap<&str, Connector> = BTreeMap::new();
    let names = system
        .served_names()
        .into_iter()
        .chain(system.supervisor_serves.iter().map(String::as_str));
    for name in names {
        let (acceptor, connector) =
            port::create().unwrap_or_else(|e| panic!("supervisor: no port for `{name}`: {e:?}"));
        acceptors.insert(name, acceptor);
        connectors.insert(name, connector);
    }
    // Its connector goes at once: every `netstack` a program holds is one
    // minted with that program's row and session (`Grants::netstack`).
    drop(connectors.remove(NETSTACK));

    // One port per file-server role, made here for the same reason: a
    // program's first open works whether or not its server runs yet. Its
    // connector goes at once: every connector a program holds to it is a
    // grant minted for that program's share (`Grants`).
    let mut role_acceptors: BTreeMap<&str, Acceptor> = BTreeMap::new();
    for role in system.programs.iter().flat_map(|p| p.roles.iter()) {
        let (acceptor, _) = port::create().unwrap_or_else(|e| panic!("supervisor: no port for the `{role}` role: {e:?}"));
        role_acceptors.insert(role, acceptor);
    }
    // The supervisor's own files are resolved through grants like every
    // program's, under a share no session is given, and it is the one
    // process nobody endows a namespace: std resolves through this one, and
    // the stop's syncs through the second.
    let files: &'static Namespace = {
        let whole = toyos_manifest::whole_tree();
        let own: Vec<(String, Connector)> = whole
            .iter()
            .filter_map(|view| Some((role_acceptors.get(view.role)?, view)))
            .map(|(acceptor, view)| mint(acceptor, authority::SUPERVISOR_SHARE, view))
            .collect();
        let build = || {
            let mut builder = namespace::build();
            for (name, connector) in &own {
                builder = builder.add(name, connector);
            }
            builder.finish().expect("supervisor: no namespace for its own files")
        };
        // SAFETY: a namespace handle built here and used nowhere else.
        if let Err(e) = unsafe { std::os::toyos::fs::adopt_namespace(build().into_raw().0) } {
            panic!("supervisor: std would not take its namespace ({e}), and the supervisor is endowed none");
        }
        Box::leak(Box::new(build()))
    };

    // Its connector goes at once: every launcher a program holds is one minted
    // with that program's row.
    let (launcher, _) = port::create().expect("supervisor: no port for `launcher`");
    let grants_port = acceptors
        .remove(grants::PORT)
        .expect("supervisor: the manifest declares the supervisor serves `grants`");
    // The supervisor alone holds a connector to it, so a connection on it is
    // the supervisor's question and nobody else's.
    let consent = connectors.remove(consent::PORT).map(|connector| {
        namespace::build()
            .add(consent::PORT, &connector)
            .finish()
            .expect("supervisor: no namespace for the consent port")
    });
    let mut supervisor = Supervisor {
        system,
        minted: Minted { launcher, grants: grants_port },
        store: Store::default(),
        consent,
        syscap: &syscap,
        acceptors,
        connectors,
        grants: Grants { roles: Vec::new(), block: None, netstack: None },
        sessions: Sessions::default(),
        files,
        services: Vec::new(),
        drivers: drives::matchers(system),
        drives: Drives::default(),
        instances: Vec::new(),
        undriven: Vec::new(),
        log,
        worker: Worker::start(),
        restarting: false,
    };
    supervisor.boot(&mut role_acceptors);

    // Nothing else holds a `serves` acceptor that has not been launched yet, so
    // the supervisor outliving its children is what keeps those ports open. It parks
    // here.
    let swap = supervisor
        .acceptors
        .remove(toyos_swap::PORT)
        .expect("supervisor: the manifest declares the supervisor serves `swap`");
    let power = supervisor
        .acceptors
        .remove(power::PORT)
        .expect("supervisor: the manifest declares the supervisor serves `power`");
    supervisor.serve_forever(&swap, &power);
}

/// How long the supervisor waits on one call into a file server that is alive and has
/// not answered, before it answers without it.
///
/// **A policy number, and not the bound on a server that ended**: one that
/// ends is started again while the supervisor waits (`Supervisor::files`), and the call then
/// goes on to the new process. What this bounds is a server alive and silent,
/// which would otherwise hold the machine's only way to start a process. A
/// first boot's DATA server formats and mounts before its first answer, so the
/// bound is generous.
const FILES_BOUND: Duration = Duration::from_millis(toyos_quiesce::FILES_MS);

/// A job on the supervisor's file worker.
type Job = Box<dyn FnOnce() + Send>;

/// The one thread the supervisor makes its calls into the file servers on.
///
/// **A call into a file server is a wait on a process the supervisor supervises**, and
/// the supervisor's loop is also where that process is started again when it ends: a
/// call made from the loop, to a server that ended under it, waits in the
/// port's queue for a server only the loop can start. So the call runs here
/// and the loop waits on it and on the end together ([`Supervisor::files`]).
struct Worker {
    jobs: std::sync::mpsc::Sender<Job>,
    /// Readable once per job finished.
    done: Pipe,
    /// Jobs handed over and not seen finished: one that outlived its bound is
    /// still here, and the worker takes no other until it finishes.
    owed: u32,
    /// What the job still owed is, for the refusal a second one gets.
    owed_what: String,
    /// Waits on `done` and on the ends of the supervisor's processes together.
    poller: Poller,
}

impl Worker {
    fn start() -> Self {
        let (jobs, queue) = std::sync::mpsc::channel::<Job>();
        let (done, finished) = toyos::pipe_pair().expect("supervisor: no pipe for its file worker");
        std::thread::Builder::new()
            .name("files".into())
            .spawn(move || {
                for job in queue {
                    job();
                    finished.write(&[1]).expect("supervisor: its file worker could not say a job finished");
                }
            })
            .expect("supervisor: its file worker could not be started");
        Self { jobs, done, owed: 0, owed_what: String::new(), poller: Poller::new(1 + MAX_CHILDREN as u32) }
    }

    /// Count the jobs that have finished since last asked.
    fn settle(&mut self) {
        let mut seen = [0u8; 8];
        while self.owed > 0 {
            match self.done.read_nonblock(&mut seen) {
                Ok(n) if n > 0 => self.owed -= n as u32,
                _ => return,
            }
        }
    }
}

/// A row whose process the machine's files depend on: a block service, or a
/// file server. Started before every other row, and never made a home, which
/// would be a directory on the volume it serves.
fn is_storage(program: &Program) -> bool {
    !program.roles.is_empty() || is_block(program) || is_disk_driver(program)
}

/// A `drives` row that offers its disks to the block service.
fn is_disk_driver(program: &Program) -> bool {
    !program.drives.is_empty() && program.receives.iter().any(|r| r == toyos_blockring::disk::PORT)
}

/// A row serving the block port.
fn is_block(program: &Program) -> bool {
    program.serves.iter().any(|s| s == toyos_blockring::PORT)
}

/// Everything the supervisor's loop acts on, for the machine's life.
struct Supervisor<'a> {
    system: &'static Manifest,
    /// What every launcher and every grants connector is minted on, and every
    /// launch and grants request accepted from.
    minted: Minted,
    /// Every answer about a folder of the home, as [`grants::STORE`] holds them.
    store: Store,
    /// [`consent::PORT`], where its server runs: what a launch asks the person at
    /// the screen on.
    consent: Option<Namespace>,
    syscap: &'a SysCap,
    /// The `serves` acceptors nobody has been started with yet, which a launch
    /// takes by move.
    acceptors: BTreeMap<&'a str, Acceptor>,
    connectors: BTreeMap<&'a str, Connector>,
    /// What each program's view is minted from.
    grants: Grants<'a>,
    /// The shares its services and the login sessions launches opened spend.
    sessions: Sessions,
    /// The same directories as a namespace of the supervisor's own, for the stop's syncs.
    files: &'static Namespace,
    /// What `[boot] start` named, a file server once per role.
    services: Vec<Service<'a>>,
    /// Every `drives` matcher, with its row.
    drivers: Vec<(&'static Program, PciMatch)>,
    /// Which functions have a `drives` process, and which are owed one.
    drives: Drives,
    /// The `drives` processes running.
    instances: Vec<Instance>,
    /// Every disk driver's function on this machine the boot started no
    /// driver on, whose disks the block service cannot say are absent.
    undriven: Vec<String>,
    /// Where a service the supervisor (re)starts writes its stdout and stderr, whichever
    /// process ends up holding its row.
    log: Log,
    /// Where the supervisor's calls into the file servers are made.
    worker: Worker,
    /// Inside [`Supervisor::ended`]: a file call made there leaves an end for
    /// the loop, which is the one caller that acts on it.
    restarting: bool,
}

/// One `drives` process, and the function it holds.
struct Instance {
    row: &'static Program,
    function: Pci,
    child: Child,
}

/// A service's kept acceptors, shared with what [`Grants`] mints on them.
/// Empty once its ports have closed for good.
type Ports = Rc<RefCell<Vec<(String, Acceptor)>>>;

/// One program the supervisor started at boot, and the ports it serves.
struct Service<'a> {
    program: &'a Program,
    /// The file-server role this process serves, for a row with `roles`.
    role: Option<&'a str>,
    /// The binary the running process was started from: the row's path, or a
    /// swap's installed one.
    path: String,
    /// `None` once neither binary would start.
    child: Option<Child>,
    /// The row's devices the running process was endowed: what a process
    /// started in its place is owed.
    devices: Vec<String>,
    ports: Ports,
    /// A swap owns the service: its process ending is expected, the
    /// supervisor does not watch it, and the ports stay open for the next one.
    swapping: bool,
    /// The machine's session under this service's own share, which every
    /// start of it spends: one started again holds no more of a file server.
    session: Session,
    /// The starts after an end it has spent.
    budget: Budget,
}

impl<'a> Service<'a> {
    fn new(program: &'a Program, role: Option<&'a str>, acceptors: Vec<(String, Acceptor)>, session: Session) -> Self {
        Self {
            program,
            role,
            path: program.path.clone(),
            child: None,
            devices: Vec::new(),
            ports: Rc::new(RefCell::new(acceptors)),
            swapping: false,
            session,
            budget: Budget::default(),
        }
    }

    /// Start `path` holding this service's manifest row, and answer its pid.
    ///
    /// `owed` is what the process this start follows held: a start refused any
    /// of it is no start, so a service never runs on without a device it had.
    fn spawn(
        &mut self,
        path: &str,
        owed: &[String],
        block: Vec<String>,
        system: &Manifest,
        syscap: &SysCap,
        connectors: &BTreeMap<&str, Connector>,
        grants: &Grants<'_>,
        minted: &Minted,
        log: &mut Log,
    ) -> Result<u32, StartError> {
        // **Checked again at every start, not only on arrival**: the installed
        // file lives in an ambient directory, so what the supervisor verified when it
        // wrote it is not a claim about what is there now. This narrows the
        // window to the spawn itself and does not close it
        // (`issues/a-swapped-binary-lives-where-any-process-can-rewrite-it.md`).
        if let Some(digest) = toyos_swap::installed_digest(path) {
            let bytes = read_binary(path).map_err(|why| std::io::Error::other(why.to_string()))?;
            toyos_swap::verify(&bytes, &digest).map_err(|why| {
                std::io::Error::other(format!("{path} no longer holds what was installed: {why}"))
            })?;
        }
        let mut storage = storage_endowment(self.role, syscap, grants)?;
        if is_block(self.program) {
            storage.args.extend(block);
        }
        let ports = self.ports.borrow();
        let served = Served::Keep { acceptors: &ports, owed };
        let (child, devices) = start(
            Command::new(path),
            self.program,
            system,
            syscap,
            served,
            connectors,
            grants,
            &[],
            storage,
            (minted, self.session),
            Output::Boot(log),
        )?;
        drop(ports);
        let pid = child.id();
        self.child = Some(child);
        self.path = path.to_string();
        self.devices = devices;
        Ok(pid)
    }

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// The name its lines and the supervisor's say it under: a file server's role
    /// beside its row's.
    fn label(&self) -> String {
        match self.role {
            Some(role) => format!("{} {role}", self.program.name),
            None => self.program.name.clone(),
        }
    }
}

/// Why a process did not start.
enum StartError {
    /// This machine refuses its row a start — the partition its file-server
    /// role is on, a `devices` matcher naming two functions — so the row is
    /// absent this boot: its ports close, and nothing serves them from memory.
    Refused(String),
    Other(std::io::Error),
}

impl From<std::io::Error> for StartError {
    fn from(e: std::io::Error) -> Self {
        Self::Other(e)
    }
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) => f.write_str(why),
            Self::Other(e) => e.fmt(f),
        }
    }
}

/// A swap the supervisor has answered and not finished.
struct Flight {
    /// Index into [`Supervisor::services`].
    service: usize,
    /// The installed binary it starts.
    path: String,
    phase: Phase,
}

enum Phase {
    /// Accepted and answered: the service is stopped once the requester hangs
    /// up, or [`toyos_swap::HANGUP_MS`] after the answer.
    Answered { conn: Connection, rx: LaunchRx, since: Instant },
    /// The new binary runs; it is the service if it is still running at
    /// `until`, and `previous` is started again if it is not.
    Probation { until: Instant, previous: String, owed: Vec<String> },
}

impl Flight {
    /// When this swap next has to be looked at whether or not anything wakes.
    fn deadline(&self) -> Instant {
        match &self.phase {
            Phase::Answered { since, .. } => {
                *since + Duration::from_millis(toyos_swap::HANGUP_MS)
            }
            Phase::Probation { until, .. } => *until,
        }
    }
}

impl<'a> Supervisor<'a> {
    /// Start `[boot] start`, kept for the machine's life: the supervisor is the only
    /// thing that can kill a daemon, and there is no other way back to a
    /// process it started. The storage rows go first, the block service
    /// before the file servers its grants are minted on — every other start
    /// may make a directory, and a directory is a file server's — and the
    /// session's home is made before the first row that is not one.
    ///
    /// **Every service holds its acceptors before any is started**, so what a
    /// start mints on another service's port — a role's directories, a
    /// partition, `netstack` — does not depend on the order `[boot] start`
    /// lists them in. The `drives` processes start last, on what the
    /// inventory says then.
    fn boot(&mut self, role_acceptors: &mut BTreeMap<&str, Acceptor>) {
        let system = self.system;
        let rank = |name: &&String| match system.program(name) {
            Some(p) if is_block(p) => 0,
            Some(p) if is_storage(p) => 1,
            _ => 2,
        };
        let mut storage_first: Vec<&String> = system.start.iter().collect();
        storage_first.sort_by_key(rank);
        for name in storage_first {
            let program = system
                .program(name)
                .unwrap_or_else(|| panic!("supervisor: [boot] start names `{name}`, which is not declared"));
            let instances: Vec<Option<&str>> = match program.roles.is_empty() {
                true => vec![None],
                false => program.roles.iter().map(|r| Some(r.as_str())).collect(),
            };
            for role in instances {
                let kept: Vec<(String, Acceptor)> = match role {
                    None => program
                        .serves
                        .iter()
                        .map(|served| {
                            let acceptor = self.acceptors.remove(served.as_str()).unwrap_or_else(|| {
                                panic!("supervisor: `{}` has already been given the `{served}` acceptor", program.name)
                            });
                            (served.clone(), acceptor)
                        })
                        .collect(),
                    Some(role) => {
                        let acceptor = role_acceptors
                            .remove(role)
                            .unwrap_or_else(|| panic!("supervisor: the `{role}` role is served twice"));
                        vec![(format!("{CAPABILITY_PREFIX}{role}"), acceptor)]
                    }
                };
                let service = Service::new(program, role, kept, self.sessions.machine());
                if let Some(role) = role {
                    self.grants.roles.push((role, Rc::clone(&service.ports)));
                }
                if is_block(program) {
                    assert!(self.grants.block.is_none(), "supervisor: two rows start a block service");
                    self.grants.block = Some(Rc::clone(&service.ports));
                }
                if program.serves.iter().any(|name| name == NETSTACK) {
                    self.grants.netstack = Some(Rc::clone(&service.ports));
                }
                self.services.push(service);
            }
        }
        // The disk drivers first, so the block service is started knowing how
        // many to wait for and a file server's disk is coming before it asks.
        self.undriven = self.drive_where(is_disk_driver);
        let mut homes_made = false;
        for at in 0..self.services.len() {
            let program = self.services[at].program;
            if !is_storage(program) && !homes_made {
                self.make_session_home();
                homes_made = true;
            }
            self.make_home(program);
            let block = self.block_args();
            let service = &mut self.services[at];
            let started = service.spawn(
                &program.path,
                &[],
                block,
                system,
                self.syscap,
                &self.connectors,
                &self.grants,
                &self.minted,
                &mut self.log,
            );
            match started {
                Ok(_) => {}
                Err(StartError::Refused(why)) => {
                    service.ports.borrow_mut().clear();
                    say!("supervisor: {} did not start ({why}); its ports are closed this boot", service.label());
                }
                Err(e) => panic!("supervisor: cannot start {}: {e}", program.name),
            }
        }
        if !homes_made {
            self.make_session_home();
        }
        self.load_grants();
        self.drive();
    }

    /// Start a `drives` process on each function the inventory says is owed
    /// one, and say which functions are given up on.
    fn drive(&mut self) {
        self.drive_where(|_| true);
    }

    /// [`Self::drive`], for the rows `which` picks; answers each function a
    /// start was refused on, by its entry and address.
    fn drive_where(&mut self, which: fn(&Program) -> bool) -> Vec<String> {
        let mut refused = Vec::new();
        let rows: Vec<(&'static Program, PciMatch)> = self.drivers.iter().filter(|(row, _)| which(row)).copied().collect();
        let records = match pci_records(self.syscap) {
            Ok(records) => records,
            Err(why) => {
                say!("supervisor: {why}, so no `drives` process starts on it");
                return refused;
            }
        };
        for act in self.drives.decide(&rows, &records, Instant::now()) {
            match act {
                Act::Start { row, entry, function } => match self.start_instance(row, entry, function) {
                    Ok(child) => self.instances.push(Instance { row, function, child }),
                    Err(why) => {
                        say!("supervisor: {} was not started on PCI {}: {why}", row.name, pci_at(function.at));
                        refused.push(format!("{entry} at PCI {}", pci_at(function.at)));
                        self.drives.refused(function.arrival);
                    }
                },
                Act::GiveUp { row, function } => say!(
                    "supervisor: {} was started on PCI {} {} times in {} s and is given up on",
                    row.name,
                    pci_at(function.at),
                    restart::RESTARTS + 1,
                    restart::WINDOW.as_secs()
                ),
            }
        }
        refused
    }

    /// What the block service is started with: each disk driver running, by
    /// its badge, which it waits for, and every disk driver's function the
    /// boot started none on.
    fn block_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for instance in self.instances.iter().filter(|i| is_disk_driver(i.row)) {
            args.extend(["--driver".to_string(), instance.function.arrival.0.to_string()]);
        }
        for undriven in &self.undriven {
            args.extend(["--undriven".to_string(), undriven.clone()]);
        }
        args
    }

    /// One `drives` process of `row`, holding `function`'s claim under
    /// `entry`'s spelling.
    fn start_instance(&mut self, row: &'static Program, entry: PciMatch, function: Pci) -> Result<Child, String> {
        if self.services.len() + self.instances.len() >= MAX_CHILDREN {
            return Err(format!("the supervisor watches {MAX_CHILDREN} processes already"));
        }
        let name = entry.to_string();
        let claim = self.syscap.claim_function::<toyos::Device>(function.arrival).map_err(|e| refused(&name, e))?;
        self.make_home(row);
        let mut extra = Extra { claims: vec![(format!("{DEV_PREFIX}{name}"), claim)], ..Extra::default() };
        // A disk driver is who this start says it is: its `disk` connector
        // carries the function's arrival, which the block service keys it by.
        if is_disk_driver(row) {
            extra.ports.push((toyos_blockring::disk::PORT.to_string(), self.grants.disk(function.arrival)?));
        }
        let launcher = (&self.minted, self.sessions.machine());
        let served = Served::Keep { acceptors: &[], owed: &[] };
        let output = Output::Boot(&mut self.log);
        start(Command::new(&row.path), row, self.system, self.syscap, served, &self.connectors, &self.grants, &[], extra, launcher, output)
            .map(|(child, _)| child)
            .map_err(|e| e.to_string())
    }

    /// [`grants::STORE`], read on the worker once DATA's server runs. A store
    /// refused, which any declared row can write, is said and the boot starts
    /// with no grant; the next grant written replaces it. **Read through its
    /// bound, never sized first**: a writer can grow it between the two.
    fn load_grants(&mut self) {
        let read = self.files("the grants", || -> Result<Option<String>, String> {
            use std::io::Read as _;
            let file = match std::fs::File::open(grants::STORE) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                opened => opened.map_err(|e| e.to_string())?,
            };
            let mut text = String::new();
            file.take(grants::MAX_STORE_BYTES as u64 + 1).read_to_string(&mut text).map_err(|e| e.to_string())?;
            match text.len() > grants::MAX_STORE_BYTES {
                true => Err(format!("it is longer than the {} bytes its grants fill", grants::MAX_STORE_BYTES)),
                false => Ok(Some(text)),
            }
        });
        let store = read.and_then(|read| read).and_then(|text| text.map_or(Ok(Store::default()), |t| Store::parse(&t)));
        match store {
            Ok(store) => self.store = store,
            Err(why) => say!("supervisor: grants: {} is refused ({why}); this boot starts with no grant", grants::STORE),
        }
    }

    /// One request on [`grants::PORT`], answered with its text: the grants
    /// listed, one written or one taken away.
    fn serve_grants(&mut self, conn: &Connection, caller: &Caller, msg_type: u32, payload: &[u8]) {
        let (msg, text) = match self.grant(caller, msg_type, payload) {
            Ok(text) => (grants::MSG_DONE, text),
            Err(why) => {
                say!("supervisor: grants: refused {} in {}: {why}", caller.row.name, caller.session);
                (grants::MSG_REFUSED, why)
            }
        };
        let _ = conn.try_send_bytes(msg, text.as_bytes());
    }

    /// **Only a login session reads or changes the grants**: one is a user's,
    /// and the machine's session is every service's.
    fn grant(&mut self, caller: &Caller, msg_type: u32, payload: &[u8]) -> Result<String, String> {
        let request = grants::Request::decode(msg_type, payload).ok_or("the request is not one this port reads")?;
        if !matches!(caller.session, Session::Login(..)) {
            return Err("only a login session may ask for grants".to_string());
        }
        let done = match request {
            grants::Request::List => return Ok(self.store.entries().iter().map(grants::listed).collect()),
            grants::Request::Revoke { package } => {
                let mut store = self.store.clone();
                if !store.revoke(toyos_manifest::USER, &package) {
                    return Err(format!("{package} holds no grant"));
                }
                self.commit(store)?;
                format!("{package}'s grant is revoked; its next launch holds no folder")
            }
            grants::Request::Add { package, folder } => {
                let (system, name) = (self.system, package.clone());
                let binary = self.files("a grant's files", move || -> Result<String, String> {
                    binary_digest(&installed(system, &name)?.path)
                })??;
                let said = format!("{package} is granted {} {}", folder.path, folder.access);
                self.keep(&package, binary, Answer::Granted(folder))?;
                said
            }
        };
        say!("supervisor: grants: {done}");
        Ok(done)
    }

    /// `answer` stored for the session user's `package` at `binary`, in place
    /// of any other: **the one way an answer reaches [`grants::STORE`]**, a
    /// command's and the person's alike. A folder is held to
    /// [`grants::admit`], and is a directory and not a link when it is
    /// stored, so nothing kept names what no launch could hold.
    fn keep(&mut self, package: &str, binary: String, answer: Answer) -> Result<(), String> {
        if let Answer::Granted(folder) = &answer {
            grants::admit(folder, self.system.folder)?;
            let path = folder.path.clone();
            self.files("a granted folder", move || is_folder(&path))??;
        }
        let mut store = self.store.clone();
        store.put(grants::Entry { user: toyos_manifest::USER.to_string(), package: package.to_string(), binary, answer })?;
        self.commit(store)
    }

    /// `store` written whole over [`grants::STORE`], and then held.
    fn commit(&mut self, store: Store) -> Result<(), String> {
        let text = store.render();
        self.files("the grants", move || write_store(&text))??;
        self.store = store;
        Ok(())
    }

    fn make_session_home(&mut self) {
        if let Err(why) = self.files("the session home", make_session_home) {
            say!("supervisor: the session home was not made: {why}");
        }
    }

    /// A service's own `HOME`, made before it first runs.
    fn make_home(&mut self, program: &Program) {
        if !program.service || is_storage(program) {
            return;
        }
        let home = program.home();
        let asked = home.clone();
        match self.files("a service's home", move || make_dir(&asked)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => say!("supervisor: {}: {home} could not be made: {e}", program.name),
            Err(why) => say!("supervisor: {}: {home} was not made: {why}", program.name),
        }
    }

    /// An installed package's `HOME`, its folder of the session's home, and
    /// its [`toyos_manifest::APP_FOLDERS`], made before it runs. `Err` is why
    /// one is not a directory, and the launch is refused: its grant would
    /// name nothing, or something else than a folder.
    fn make_app_home(&mut self, program: &Program) -> Result<(), String> {
        let home = program.home();
        let asked = home.clone();
        let made = self.files("an app's home", move || {
            let folders = toyos_manifest::APP_FOLDERS.iter().map(|folder| format!("{asked}/{folder}"));
            std::iter::once(asked.clone()).chain(folders).try_for_each(|dir| {
                make_dir(&dir)?;
                match std::fs::symlink_metadata(&dir)?.is_dir() {
                    true => Ok(()),
                    false => Err(std::io::Error::other(format!("{dir} is no directory"))),
                }
            })
        });
        match made {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("{home} could not be made: {e}")),
            Err(why) => Err(format!("{home} was not made: {why}")),
        }
    }

    /// The idle slot, for the one program whose row asks for it
    /// (`toyos_update::slots`): minted here, so the slot this boot runs is
    /// never among what is endowed. A machine with none says so and the
    /// program starts holding nothing, which it refuses by name.
    fn slot_storage(&mut self, program: &Program) -> Extra {
        if !program.slots {
            return Extra::default();
        }
        self.slot_grant().unwrap_or_else(|why| {
            say!("supervisor: {}: no slot to grant: {why}", program.name);
            Extra::default()
        })
    }

    /// **Which slot is idle is the loader's word, not the table's**: the
    /// running ROOT and volume are the ones the loader read, the table is the
    /// one on that ROOT's disk, and the idle slot is the one whose ROOT is not
    /// it — and its two partitions are reached only as
    /// `toyos_update::slots::grant` admits them, through connectors to the
    /// block service minted for each.
    fn slot_grant(&mut self) -> Result<Extra, String> {
        use toyos_abi::inventory::Role;
        use toyos_update::slots::{BOOT_LABEL, ROOT_LABEL, TABLE_LABEL};
        let records = inventory(self.syscap)?;
        let running = loaded(&records, Role::Root).ok_or("the loader named no ROOT")?;
        let volume = loaded(&records, Role::Slot).ok_or("the loader named no slot volume")?;
        let mint = |grants: &Grants<'_>, grant: BlockGrant| {
            grants
                .partitions(grant)?
                .ok_or_else(|| "no block service runs".to_string())
        };
        let kind = |guid: toyos_gpt::Guid| BlockGrant { scope: Scope::Kind(guid.0), writes: false };
        let tables = mint(&self.grants, kind(toyos_gpt::Guid::TOYOS_SLOTS))?;
        let boots = mint(&self.grants, kind(toyos_gpt::Guid::TOYOS_BOOT))?;
        let roots = mint(&self.grants, kind(toyos_gpt::Guid::TOYOS_ROOT))?;
        let (table, table_guid, listed) = self.files("the slot table", move || served_slots(running, tables, boots, roots))??;
        let Some(runs) = listed.iter().find(|p| p.unique_guid == running && p.type_guid == SLOT_KINDS.root) else {
            return Err("the ROOT this boot runs is on no disk the block service serves".into());
        };
        let (idle, slot) = toyos_update::slots::grant(&table, runs, volume, &listed, SLOT_KINDS).map_err(|why| why.to_string())?;
        let mut ports = Vec::new();
        for (label, guid) in [(TABLE_LABEL, table_guid), (BOOT_LABEL, slot.boot), (ROOT_LABEL, slot.root)] {
            ports.push((label.to_string(), mint(&self.grants, BlockGrant { scope: Scope::Unique(guid), writes: true })?));
        }
        say!(
            "supervisor: the idle slot is {}, granted with the slot table read through the block service",
            idle.letter()
        );
        Ok(Extra { ports, ..Extra::default() })
    }

    /// `work`, a call into the file servers or the block service, made on
    /// [`Worker`], and its answer — with every end meanwhile acted on, so a
    /// call a server's end left waiting in the port's queue goes on to the
    /// process started in its place.
    /// `Err` is the call still unanswered at [`FILES_BOUND`], which the worker
    /// goes on waiting for alone, or the worker still waiting on an earlier
    /// one.
    fn files<T: Send + 'static>(&mut self, what: &str, work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
        const DONE: u64 = 0;
        self.worker.settle();
        if self.worker.owed > 0 {
            return Err(format!("the supervisor's file worker still waits on {}", self.worker.owed_what));
        }
        let (answer, answered) = std::sync::mpsc::channel();
        let job: Job = Box::new(move || {
            // The receiver is gone once the bound has passed.
            let _ = answer.send(work());
        });
        self.worker.jobs.send(job).expect("supervisor: its file worker has ended");
        self.worker.owed = 1;
        self.worker.owed_what = what.to_string();
        let deadline = Instant::now() + FILES_BOUND;
        loop {
            self.worker.poller.watch(&self.worker.done, READABLE, DONE);
            if !self.restarting {
                self.watch_children(&self.worker.poller);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let mut woken = false;
            self.worker.poller.wait(1, left.as_nanos() as u64, |token| woken |= token == TOKEN_ENDED);
            if woken {
                self.ended();
            }
            self.worker.settle();
            if self.worker.owed == 0 {
                return Ok(answered.recv().expect("supervisor: a finished file job answered"));
            }
            if Instant::now() >= deadline {
                return Err(format!("{what} was not answered in {} s", FILES_BOUND.as_secs()));
            }
        }
    }

    /// Serve `launcher` and `swap` for the rest of the machine's life.
    ///
    /// **An event loop and not an accept loop, because the rule every other
    /// server in this tree obeys binds the supervisor hardest.** A server never blocks on
    /// a client: accept and the first frame are two events, a frame is buffered
    /// until whole before anything acts on it, and a reply is one non-blocking
    /// write.
    ///
    /// What a swap waits on is the loop's too: the requester's hang-up is an
    /// event, and the ends of the hang-up bound and of probation are deadlines
    /// the wait wakes for. The one wait the supervisor makes outside the loop is for a
    /// service it has just killed to finish ending, which is the kernel's
    /// teardown and no client's.
    fn serve_forever(&mut self, swap: &Acceptor, power: &Acceptor) -> ! {
        let poller = Poller::new(Poller::MAX_HANDLES);
        let mut pending: Vec<Pending> = Vec::new();
        let mut flight: Option<Flight> = None;
        let mut parked: Option<Parked> = None;
        let mut ready: Vec<u64> = Vec::new();
        loop {
            poller.watch(&self.minted.launcher, READABLE, TOKEN_ACCEPTOR);
            poller.watch(&self.minted.grants, READABLE, TOKEN_GRANTS_ACCEPTOR);
            poller.watch(swap, READABLE, TOKEN_SWAP_ACCEPTOR);
            poller.watch(power, READABLE, TOKEN_POWER_ACCEPTOR);
            self.watch_children(&poller);
            if let Some(Flight { phase: Phase::Answered { conn, .. }, .. }) = &flight {
                poller.watch(conn, READABLE, TOKEN_HANGUP);
            }
            if let Some(asked) = &parked {
                poller.watch(&asked.launch.conn, READABLE, TOKEN_PARKED_CALLER);
                poller.watch(&asked.consent, READABLE, TOKEN_CONSENT);
            }
            for p in &pending {
                poller.watch(&p.conn, READABLE, TOKEN_PENDING_BASE + p.conn.as_handle().0 as u64);
            }
            // A client that connects and then says nothing wakes nothing, so
            // the deadline that removes it has to be a wake in its own right —
            // otherwise a silent client is only ever timed out by some other
            // client's traffic, and with the table full there is no other
            // client.
            //
            // **What is left of the oldest one's deadline, not a fresh
            // `HANDSHAKE_TIMEOUT`.** Any readiness at all restarts a wait, so a
            // full timeout each round is a bound of nearly twice the number for
            // a client that goes quiet just before some other connection wakes
            // the loop — and a bound that is not the bound is the class this
            // whole change is about.
            let now = Instant::now();
            let timeout = pending
                .iter()
                .map(|p| HANDSHAKE_TIMEOUT.saturating_sub(now.duration_since(p.since)))
                .chain(flight.iter().map(|f| f.deadline().saturating_duration_since(now)))
                .min()
                .map_or(u64::MAX, |left| left.as_nanos() as u64);
            ready.clear();
            poller.wait(1, timeout, |token| ready.push(token));

            // Accept and the request are two events. Nothing is read here.
            for (token, acceptor, port) in [
                (
                    TOKEN_ACCEPTOR,
                    &self.minted.launcher,
                    (|s, c| s.caller(&s.minted.launcher, c).map(Port::Launcher)) as fn(&Self, &Connection) -> _,
                ),
                (TOKEN_GRANTS_ACCEPTOR, &self.minted.grants, |s, c| s.caller(&s.minted.grants, c).map(Port::Grants)),
                (TOKEN_SWAP_ACCEPTOR, swap, |_, _| Ok(Port::Swap)),
                (TOKEN_POWER_ACCEPTOR, power, |_, _| Ok(Port::Power)),
            ] {
                if !ready.contains(&token) {
                    continue;
                }
                let conn = match acceptor.accept() {
                    Ok(conn) => conn,
                    Err(e) => panic!("supervisor: an acceptor of the supervisor's own refused: {e:?}"),
                };
                if pending.len() >= MAX_PENDING_LAUNCHES {
                    say!(
                        "supervisor: launcher: refusing client {} — {MAX_PENDING_LAUNCHES} connections \
                         are already waiting to say what they want",
                        conn.as_handle().0
                    );
                    continue;
                }
                let port = match port(self, &conn) {
                    Ok(port) => port,
                    Err(why) => {
                        say!("supervisor: launcher: dropping client {} — {why}", conn.as_handle().0);
                        continue;
                    }
                };
                pending.push(Pending { conn, rx: LaunchRx::new(), since: Instant::now(), port });
            }

            // `remove` rather than `swap_remove`: the entries after `i` shift
            // down, so leaving `i` alone visits each connection exactly once.
            let mut i = 0;
            while i < pending.len() {
                let handle = pending[i].conn.as_handle();
                if !ready.contains(&(TOKEN_PENDING_BASE + handle.0 as u64)) {
                    i += 1;
                    continue;
                }
                let step = {
                    let p = &mut pending[i];
                    p.rx.pump(&p.conn)
                };
                match step {
                    RxStep::Idle => i += 1,
                    // Unlogged, and the only removal here that is: a caller may
                    // connect to find out whether it holds a launcher at all
                    // and hang up, which is its business.
                    RxStep::Eof => {
                        pending.remove(i);
                    }
                    RxStep::Malformed => {
                        say!(
                            "supervisor: launcher: dropping client {} — it sent a frame this protocol \
                             cannot describe",
                            handle.0
                        );
                        pending.remove(i);
                    }
                    RxStep::Frame { msg_type, payload_len } => {
                        let p = pending.remove(i);
                        match p.port {
                            Port::Launcher(caller) => {
                                let payload = p.rx.payload(payload_len).to_vec();
                                self.serve_launch(p.conn, &caller, msg_type, &payload, &mut parked)
                            }
                            Port::Grants(caller) => self.serve_grants(&p.conn, &caller, msg_type, p.rx.payload(payload_len)),
                            Port::Power => self.stop(&p.conn, msg_type),
                            Port::Swap => {
                                let payload = p.rx.payload(payload_len).to_vec();
                                if let Some(accepted) =
                                    self.accept_swap(p.conn, msg_type, &payload, flight.as_ref())
                                {
                                    flight = Some(accepted);
                                }
                            }
                        }
                    }
                }
            }

            // **After the requests that arrived are served**: a client whose
            // request is here is answered however late this loop came round to
            // it, and only one that has said nothing in its bound is let go.
            let now = Instant::now();
            for p in pending.iter().filter(|p| now.duration_since(p.since) >= HANDSHAKE_TIMEOUT) {
                say!(
                    "supervisor: launcher: dropping client {} — it never finished its launch",
                    p.conn.as_handle().0
                );
            }
            pending.retain(|p| now.duration_since(p.since) < HANDSHAKE_TIMEOUT);

            flight = flight.and_then(|f| self.advance(f, ready.contains(&TOKEN_HANGUP)));
            let (caller_ready, consent_ready) = (ready.contains(&TOKEN_PARKED_CALLER), ready.contains(&TOKEN_CONSENT));
            parked = parked.and_then(|asked| self.advance_parked(asked, caller_ready, consent_ready));

            if ready.contains(&TOKEN_ENDED) {
                self.ended();
            }
        }
    }

    /// The row and session the badge of a connection on `acceptor`, one of
    /// [`Minted`]'s, names. Only this supervisor mints on them, so a refusal
    /// here is its own bug, said and the connection dropped.
    fn caller(&self, acceptor: &Acceptor, conn: &Connection) -> Result<Caller, String> {
        let mut badge = [0u8; MAX_BADGE];
        let badge = acceptor.badge(conn, &mut badge).map_err(|e| format!("its connection has no badge ({e:?})"))?;
        let Some(Authority { row, session }) = Authority::decode(badge) else {
            return Err(format!("its badge is not one this supervisor writes: {badge:?}"));
        };
        let row = self.system.program(&row).ok_or_else(|| format!("its badge names `{row}`, which is no row"))?;
        Ok(Caller { row, session })
    }

    /// One swap request, answered: refused with the old service untouched, or
    /// verified, installed and accepted.
    ///
    /// **Everything in the request is sshserver's claim about what its client
    /// sent**, and the bytes are read here and held against the digest here:
    /// what the supervisor starts is what the supervisor verified, and nothing else's word for it.
    fn accept_swap(
        &self,
        conn: Connection,
        msg_type: u32,
        payload: &[u8],
        flight: Option<&Flight>,
    ) -> Option<Flight> {
        let refuse = |service: &str, why: Refusal| {
            swapped(service, Word::Refused, &why.to_string());
            let _ = conn.try_send_bytes(toyos_swap::MSG_REFUSED, why.to_string().as_bytes());
            None
        };
        if msg_type != toyos_swap::MSG_SWAP {
            return refuse("?", Refusal::Malformed(format!("message {msg_type} is no swap")));
        }
        let request = match SwapRequest::decode(payload) {
            Ok(request) => request,
            Err(why) => return refuse("?", why),
        };
        let name = request.service.as_str();
        // Read and removed before anything else can refuse, so a refused
        // request leaves nothing staged behind it.
        let bytes = read_binary(&request.staged);
        let _ = std::fs::remove_file(&request.staged);
        let Some(index) = self.services.iter().position(|s| s.program.name == name) else {
            return refuse(name, Refusal::NotAService(name.to_string()));
        };
        if let Some(busy) = flight {
            return refuse(name, Refusal::Busy(self.services[busy.service].program.name.clone()));
        }
        let service = &self.services[index];
        let running = service
            .child
            .as_ref()
            .is_some_and(|c| toyos_abi::syscall::process_wait_nonblock(
                toyos::RawHandle(c.as_raw_handle()),
            ).is_err());
        if !running {
            return refuse(name, Refusal::NotRunning(name.to_string()));
        }
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(why) => return refuse(name, why),
        };
        if let Err(why) = toyos_swap::verify(&bytes, &request.digest) {
            return refuse(name, why);
        }
        let installed = toyos_swap::installed_path(name, &request.digest);
        if installed == service.path {
            return refuse(name, Refusal::AlreadyRuns(installed));
        }
        if let Err(why) = install(&installed, &request.digest, &bytes) {
            return refuse(name, why);
        }
        swapped(
            name,
            Word::Accepted,
            &format!(
                "{installed} ({} bytes, sha256 {}) replaces {} (pid {})",
                bytes.len(),
                toyos_swap::hex(&request.digest),
                service.path,
                service.pid().map_or(0, |p| p)
            ),
        );
        // A requester that cannot take the answer has hung up, which is the go
        // either way.
        let _ = conn.try_send_bytes(toyos_swap::MSG_ACCEPTED, installed.as_bytes());
        Some(Flight {
            service: index,
            path: installed,
            phase: Phase::Answered { conn, rx: LaunchRx::new(), since: Instant::now() },
        })
    }

    /// Move a swap on as far as what has happened lets it, and answer what is
    /// left of it.
    fn advance(&mut self, flight: Flight, woken: bool) -> Option<Flight> {
        let now = Instant::now();
        match flight.phase {
            Phase::Answered { conn, mut rx, since } => {
                let hung_up = woken && !matches!(rx.pump(&conn), RxStep::Idle);
                if !hung_up && now < since + Duration::from_millis(toyos_swap::HANGUP_MS) {
                    return Some(Flight { phase: Phase::Answered { conn, rx, since }, ..flight });
                }
                drop(conn);
                self.cut_over(flight.service, flight.path)
            }
            Phase::Probation { until, previous, owed } => {
                if now < until {
                    return Some(Flight { phase: Phase::Probation { until, previous, owed }, ..flight });
                }
                self.end_probation(flight.service, &flight.path, &previous, &owed);
                None
            }
        }
    }

    /// Stop the service and start the installed binary in its place.
    fn cut_over(&mut self, index: usize, path: String) -> Option<Flight> {
        let name = self.services[index].program.name.clone();
        let previous = self.services[index].path.clone();
        // What the process being stopped holds, which its replacement and the
        // binary a failed swap starts again are each owed.
        let owed = self.services[index].devices.clone();
        let program = self.services[index].program;
        self.make_home(program);
        let block = self.block_args();
        let service = &mut self.services[index];
        swapped(
            &name,
            Word::Stopping,
            &format!("pid {} ({previous})", service.pid().map_or(0, |p| p)),
        );
        service.swapping = true;
        if let Some(mut old) = service.child.take() {
            // Killed and then waited for, because the kernel publishes a
            // process's end only after its handle table is closed: once the
            // wait returns, every device claim it held is back and can be
            // minted again.
            let _ = old.kill();
            let _ = old.wait();
        }
        let started = service.spawn(
            &path,
            &owed,
            block,
            self.system,
            self.syscap,
            &self.connectors,
            &self.grants,
            &self.minted,
            &mut self.log,
        );
        match started {
            Ok(pid) => {
                swapped(
                    &name,
                    Word::Started,
                    &format!(
                        "{path} as pid {pid}; in service if it runs {} ms",
                        toyos_swap::PROBATION_MS
                    ),
                );
                let until = Instant::now() + Duration::from_millis(toyos_swap::PROBATION_MS);
                Some(Flight { service: index, path, phase: Phase::Probation { until, previous, owed } })
            }
            Err(e) => {
                swapped(&name, Word::Failed, &format!("{path} did not start: {e}"));
                forget(&path);
                self.restore(index, &previous, &owed);
                None
            }
        }
    }

    /// Probation is over: the new binary is the service, or the one it
    /// replaced is started again.
    fn end_probation(&mut self, index: usize, path: &str, previous: &str, owed: &[String]) {
        let service = &mut self.services[index];
        let name = service.program.name.clone();
        // The loop watches its end from here, and an end before this answer
        // is this function's.
        let status = service.child.as_mut().map(|c| c.try_wait());
        if matches!(status, Some(Ok(None))) {
            service.swapping = false;
        }
        match status {
            Some(Ok(None)) => {
                swapped(
                    &name,
                    Word::InService,
                    &format!("{path} as pid {}", service.pid().map_or(0, |p| p)),
                );
                if previous != path {
                    forget(previous);
                }
            }
            ended => {
                let how = match ended {
                    Some(Ok(Some(status))) => format!("ended ({status})"),
                    Some(Err(e)) => format!("could not be asked whether it runs ({e})"),
                    _ => "is not running".to_string(),
                };
                swapped(
                    &name,
                    Word::Failed,
                    &format!("{path} {how} inside {} ms", toyos_swap::PROBATION_MS),
                );
                service.child = None;
                forget(path);
                self.restore(index, previous, owed);
            }
        }
    }

    /// A request on `power`: `logkeeper` flushed, then the kernel asked with the
    /// machine's capability. Answers only a refusal: a stop that happens takes
    /// the supervisor, and the requester, with it.
    fn stop(&self, conn: &Connection, msg_type: u32) {
        let Some(how) = Stop::from_message(msg_type) else {
            say!("supervisor: power: dropping client {} — frame {msg_type} is no stop", conn.as_handle().0);
            return;
        };
        say!("{STOPPING} ({how:?})");
        self.log.flush();
        self.sync_files();
        let refused = match how {
            Stop::Reboot => self.syscap.reboot(),
            Stop::Shutdown => self.syscap.shutdown(),
        };
        self.log.resume();
        toyos::error!("supervisor: power: the kernel refused {how:?}: {refused:?}");
        let _ = conn.try_send_bytes(power::MSG_REFUSED, &refused.to_u64().to_le_bytes());
    }

    /// Have every writable file server make its volume durable: the kernel's
    /// stop waits for no process, so what a server holds and has not written
    /// is lost unless it is asked first. After `logkeeper`'s flush, so the log's
    /// server writes what logkeeper wrote last.
    fn sync_files(&self) {
        let roles = self.system.programs.iter().flat_map(|p| p.roles.iter());
        let mut syncing = Vec::new();
        for role in roles.filter(|r| *r != "boot") {
            let Some(dir) = toyos_manifest::role_dirs(role).and_then(|dirs| dirs.first()) else { continue };
            let name = format!("{CAPABILITY_PREFIX}{}", dir.dir);
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let (asked, names) = (name.clone(), self.files);
            let spawned = std::thread::Builder::new().name(format!("sync-{role}")).spawn(move || {
                let answer = toyos::fs::Dir::connect(names, &asked).and_then(|mut dir| dir.sync());
                let _ = done_tx.send(answer);
            });
            match spawned {
                Ok(syncer) => syncing.push((name, done_rx, syncer)),
                Err(_) => say!("supervisor: power: no thread to sync {name}; its unsynced writes are lost with the stop"),
            }
        }
        let until = Instant::now() + Duration::from_millis(toyos_quiesce::SYNC_MS);
        for (name, done_rx, syncer) in syncing {
            let answered = done_rx.recv_timeout(until.saturating_duration_since(Instant::now()));
            // Joined once it has answered, so the stop never counts a thread
            // that was on its way out.
            if answered.is_ok() {
                syncer.join().expect("supervisor: a sync thread panicked");
            }
            match answered {
                Ok(Ok(())) => {}
                Ok(Err(e)) => say!("supervisor: power: {name}'s server would not sync ({e:?})"),
                Err(_) => say!(
                    "supervisor: power: {name}'s server did not sync in {} ms; stopping without it",
                    toyos_quiesce::SYNC_MS
                ),
            }
        }
    }

    /// Watch the end of every process the supervisor started and has not seen
    /// end, but a swapping service's: its swap watches that.
    fn watch_children(&self, poller: &Poller) {
        let services = self.services.iter().filter(|service| !service.swapping).filter_map(|service| service.child.as_ref());
        for child in services.chain(self.instances.iter().map(|instance| &instance.child)) {
            poller.watch_raw(toyos::RawHandle(child.as_raw_handle()), READABLE, TOKEN_ENDED);
        }
    }

    /// Act on every end since the last: a service is started again on the
    /// ports it kept, or has them closed and its clients answered `Gone`
    /// ([`restart::after_end`]); a `drives` process's function is owed
    /// another or left alone ([`Drives::ended`]); and the inventory is read
    /// again.
    fn ended(&mut self) {
        self.restarting = true;
        let mut any = false;
        for index in 0..self.services.len() {
            let service = &mut self.services[index];
            let Some(exit) = service.child.as_ref().filter(|_| !service.swapping).and_then(exit_of) else { continue };
            any = true;
            let (label, pid) = (service.label(), service.pid().unwrap_or(0));
            service.child = None;
            match restart::after_end(service.program, exit, &mut service.budget, Instant::now()) {
                AfterEnd::Close => service.ports.borrow_mut().clear(),
                AfterEnd::GiveUp => {
                    service.ports.borrow_mut().clear();
                    say!(
                        "supervisor: {label} ended {} times in {} s; its ports are closed, its clients answered Gone, and a program started from now on is given none of its directories",
                        restart::RESTARTS + 1,
                        restart::WINDOW.as_secs()
                    );
                }
                AfterEnd::Restart => {
                    let (path, owed, program) = (service.path.clone(), service.devices.clone(), service.program);
                    self.make_home(program);
                    let block = self.block_args();
                    let service = &mut self.services[index];
                    let (system, syscap, connectors, grants) = (self.system, self.syscap, &self.connectors, &self.grants);
                    match service.spawn(&path, &owed, block, system, syscap, connectors, grants, &self.minted, &mut self.log) {
                        Ok(new) => say!("supervisor: {label} (pid {pid}) ended; started again as pid {new}"),
                        Err(e) => {
                            service.ports.borrow_mut().clear();
                            say!("supervisor: {label} ended and would not start again ({e}); its ports are closed");
                        }
                    }
                }
            }
        }
        let mut at = 0;
        while at < self.instances.len() {
            let Some(exit) = exit_of(&self.instances[at].child) else {
                at += 1;
                continue;
            };
            any = true;
            let Instance { row, function, .. } = self.instances.swap_remove(at);
            say!("supervisor: {} on PCI {} ended with {exit}", row.name, pci_at(function.at));
            self.drives.ended(function.arrival, restart::owed(row, exit));
        }
        if any {
            self.drive();
        }
        self.restarting = false;
    }

    /// Start the binary a failed swap replaced, or close the service's ports
    /// when that will not start either.
    fn restore(&mut self, index: usize, previous: &str, owed: &[String]) {
        let program = self.services[index].program;
        self.make_home(program);
        let block = self.block_args();
        let service = &mut self.services[index];
        let name = service.program.name.clone();
        match service.spawn(previous, owed, block, self.system, self.syscap, &self.connectors, &self.grants, &self.minted, &mut self.log) {
            Ok(pid) => {
                service.swapping = false;
                swapped(&name, Word::Restored, &format!("{previous} as pid {pid}"));
            }
            Err(e) => {
                service.swapping = false;
                service.ports.borrow_mut().clear();
                swapped(
                    &name,
                    Word::Gone,
                    &format!("{previous} did not start either ({e}); its ports are closed"),
                );
            }
        }
    }
}

/// Installed package `name`'s row, its program as its manifest names it: a
/// package named after a declared row is none.
fn installed(system: &Manifest, name: &str) -> Result<Program, String> {
    let file = Package::path(name);
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{file} cannot be read: {e}"))?;
    let package = Package::parse(&text)?;
    if package.name != name {
        return Err(format!("{file} calls itself {:?}", package.name));
    }
    system.app_row(name, &package.program)
}

/// `Ok` where `path` is a directory, and not a link to one.
fn is_folder(path: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(format!("{path} is no directory")),
        Err(e) => Err(format!("{path} is no directory: {e}")),
    }
}

/// The SHA-256 of the binary at `path`, which a grant is keyed on.
fn binary_digest(path: &str) -> Result<String, String> {
    let bytes = read_binary(path).map_err(|why| why.to_string())?;
    Ok(toyos_swap::hex(&toyos_swap::digest(&bytes)))
}

/// [`grants::STORE`] replaced whole: a name beside it, then one rename, so the
/// store is never a partial one.
fn write_store(text: &str) -> Result<(), String> {
    let partial = format!("{}.partial", grants::STORE);
    make_dir(grants::STORE_DIR)
        .and_then(|()| std::fs::write(&partial, text))
        .and_then(|()| std::fs::rename(&partial, grants::STORE))
        .map_err(|e| format!("{} could not be written: {e}", grants::STORE))
}

/// A staged or installed binary's bytes, refused past [`toyos_swap::MAX_BINARY_BYTES`]
/// before any of it is read, and read through that bound: a file grown
/// between the two is refused, never read whole. Made from the loop: a swap's
/// files are under [`toyos_swap::STAGING`], which the kernel serves.
fn read_binary(path: &str) -> Result<Vec<u8>, Refusal> {
    use std::io::Read as _;
    let unreadable = |e: std::io::Error| Refusal::Unreadable { path: path.to_string(), why: e.to_string() };
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let len = file.metadata().map_err(unreadable)?.len();
    if len > toyos_swap::MAX_BINARY_BYTES {
        return Err(Refusal::TooLarge(len));
    }
    let mut bytes = Vec::new();
    file.take(toyos_swap::MAX_BINARY_BYTES + 1).read_to_end(&mut bytes).map_err(unreadable)?;
    match bytes.len() as u64 > toyos_swap::MAX_BINARY_BYTES {
        true => Err(Refusal::Unreadable {
            path: path.to_string(),
            why: format!("it grew past {} bytes while it was read", toyos_swap::MAX_BINARY_BYTES),
        }),
        false => Ok(bytes),
    }
}

/// Write verified bytes where a service is started from: a temporary name
/// beside it and one rename, so the path is never a partial binary.
fn install(path: &str, digest: &toyos_swap::Digest, bytes: &[u8]) -> Result<(), Refusal> {
    let failed = |e: std::io::Error| Refusal::Install { path: path.to_string(), why: e.to_string() };
    std::fs::create_dir_all(toyos_swap::installed_dir(digest)).map_err(failed)?;
    let partial = format!("{path}.partial");
    std::fs::write(&partial, bytes).map_err(failed)?;
    std::fs::rename(&partial, path).map_err(failed)
}

/// Delete a binary a swap installed once nothing runs it. The image's own
/// binaries are never touched.
fn forget(path: &str) {
    if !toyos_swap::is_installed(path) {
        return;
    }
    let _ = std::fs::remove_file(path);
    if let Some((dir, _)) = path.rsplit_once('/') {
        let _ = std::fs::remove_dir(dir);
    }
}

impl Supervisor<'_> {
    /// One `MSG_LAUNCH`, from the frame to the `Process` handle that answers it,
    /// or to the question an installed package's launch waits on ([`Parked`]).
    ///
    /// **Everything in the request is a client's claim about itself; `caller` is
    /// not.** A program nothing declares is refused by name; a frame that does
    /// not decode is a dropped connection and nothing else; a row the caller's
    /// row may not start is refused before any of its files is read. What the
    /// child ends up holding is the manifest's row for it plus whatever
    /// connectors the caller transferred, and the caller could only transfer
    /// what it already had — so a launch confers exactly the manifest row and
    /// nothing beyond it.
    fn serve_launch(
        &mut self,
        conn: Connection,
        caller: &Caller,
        msg_type: u32,
        payload: &[u8],
        parked: &mut Option<Parked>,
    ) {
        if msg_type != launch::MSG_LAUNCH {
            return;
        }
        let mut batch = [toyos::RawHandle(0); toyos_abi::syscall::MAX_TRANSFER_HANDLES];
        let received = conn.recv_handles(&mut batch).unwrap_or(0);

        // **Owned on the statement after they arrive, and before anything can
        // refuse.** The send moved them into the supervisor's table, so every path out of
        // here releases them — a launcher that leaked a handle per refused launch
        // would exhaust the one table the machine cannot do without, and a client
        // picks which refusal it takes.
        let mut held = Moved(batch[..received].to_vec());

        let Some(request) = Request::decode(payload) else { return };
        // `extra_names` drops an empty or non-UTF-8 name, so its count is what
        // will actually be paired with a handle. A frame whose two counts
        // disagree would otherwise leave the unpaired handles behind.
        let names: Vec<&str> = request.extra_names().collect();
        if received != request.handle_count() || names.len() != request.extra_count {
            say!(
                "supervisor: launcher: a frame promising {} handles under {} names carried {received}",
                request.handle_count(),
                names.len(),
            );
            return;
        }

        // Past every refusal that does not know which handle is which, so ownership
        // can be split. Every part still releases on every path below.
        let all = held.take();
        let (slot_handles, rest) = all.split_at(request.slot_count());
        let (extra_handles, place_handle) = rest.split_at(request.extra_count);
        let slots = Moved(slot_handles.to_vec());
        let place = Moved(place_handle.to_vec());
        // Owned, so they close when the launch is done with: `SYS_NAMESPACE_BUILD`
        // copies a connector into the namespace and leaves the caller's handle,
        // and the supervisor's copy of a client's connector has no life beyond
        // this launch.
        let extras: Vec<(String, Connector)> = names
            .into_iter()
            .zip(extra_handles.iter().copied())
            // SAFETY: the kernel moved these into the supervisor's table with the frame, and
            // nothing else answers for them. **Not a claim about the type** — a
            // client sends what it likes, and everything below treats a wrong one
            // as a refused launch rather than as the supervisor's own bug.
            .map(|(name, handle)| (name.to_string(), unsafe { Connector::from_raw(handle) }))
            .collect();

        // std joins a relative `current_dir` onto the supervisor's own cwd, so passing one
        // on would start the child in the supervisor's `/` — the default this field removes.
        if !request.cwd.starts_with('/') {
            say!("supervisor: launcher: refused a working directory that is not absolute");
            let _ = conn.try_signal(launch::MSG_REFUSED);
            return;
        }

        // **A launch names its parent**: the place it carries, spawned under, or
        // the supervisor. One that names neither is refused rather than given to the supervisor,
        // which would let it outlive a caller that never asked it to.
        let under = match request.parent() {
            Some(Parent::Place(())) => Some(place.0[0]),
            Some(Parent::Supervisor) => None,
            None => {
                say!("supervisor: launcher: refused a launch that names no parent");
                let _ = conn.try_signal(launch::MSG_REFUSED);
                return;
            }
        };

        // **`argv[0]` is the caller's path; the program is the row's.** An applet
        // learns which it is from its name — `/system/bin/echo` run as `ls` is an
        // `ls` — but the bytes are the row's `path` (`image_from`, below), never
        // this one opened again: `declared` may have reached the row through a
        // link the caller can re-point, and its own bytes would run under the
        // row's claims.
        let mut command = Command::new(request.program);
        // **Carried, not inherited.** A child of the launcher would otherwise get
        // the supervisor's environment and the supervisor's working directory, so `cd /tmp && ls` would
        // list `/`. The launcher is a spawn service, not a session.
        command.env_clear();
        for entry in request.env.split(|&b| b == 0).filter(|e| !e.is_empty()) {
            let Some(eq) = entry.iter().position(|&b| b == b'=') else { continue };
            if let (Ok(key), Ok(value)) =
                (std::str::from_utf8(&entry[..eq]), std::str::from_utf8(&entry[eq + 1..]))
            {
                command.env(key, value);
            }
        }
        command.current_dir(request.cwd);
        if let Some(place) = under {
            command.under(place.0);
        }
        for arg in request.argv.split(|&b| b == 0).skip(1).filter(|a| !a.is_empty()) {
            if let Ok(arg) = std::str::from_utf8(arg) {
                command.arg(arg);
            }
        }

        // On the worker, every file the launch reads: a path under `/apps` is
        // read off its package, and the program and its working directory may
        // be a file server's, which the supervisor supervises. Whether the
        // caller may start it is asked between the two, so a refused launch
        // reads no image and judges no directory. **A package's grant is keyed
        // on the bytes its spawn starts**: the image `prepare` read, hashed
        // where it lies, with no second read between the hash and the start.
        let (system, path) = (self.system, request.program.to_string());
        let (row, session) = (caller.row, caller.session);
        let found = self.files("a launch's files", move || {
            resolve(system, &path, |target| -> Result<_, authority::Refusal> {
                let starts = authority::may_start(row, session, target)?;
                let (Target::Row(program) | Target::Package(program)) = target;
                let prepared = command.image_from(Path::new(&program.path)).prepare().map(drop);
                let binary = matches!(target, Target::Package(_)).then(|| {
                    command
                        .prepared_image()
                        .map(|bytes| toyos_swap::hex(&toyos_swap::digest(bytes)))
                        .ok_or_else(|| format!("{} was not read before its start, so no grant can name it", program.path))
                });
                Ok((starts, binary, prepared.map(|()| command)))
            })
        });
        let resolved = match found {
            Ok(found) => found,
            Err(why) => {
                say!("supervisor: launcher: {} was not resolved: {why}", request.program);
                let _ = conn.try_signal(launch::MSG_REFUSED);
                return;
            }
        };
        let (program, verdict) = match resolved {
            Resolved::Row(row, verdict) => (row.clone(), verdict),
            Resolved::Package(row, verdict) => (row, verdict),
            Resolved::NotDeclared => {
                // **`try_send_bytes` and not `send`.** A blocking write is the
                // other half of the rule that made the read side an event loop: a
                // client that never drains its end decides when the supervisor runs again.
                // The `HOME` is the session's: a program no row names is no service.
                let home = toyos_manifest::session_home();
                let _ = conn.try_send_bytes(launch::MSG_NOT_DECLARED, home.as_bytes());
                return;
            }
            Resolved::Refused(why) => {
                say!("supervisor: launcher: {why}");
                let _ = conn.try_signal(launch::MSG_REFUSED);
                return;
            }
        };
        // **`MSG_REFUSED`, never `MSG_NOT_DECLARED`**: the latter is std's cue to
        // spawn the program itself.
        let (starts, binary, prepared) = match verdict {
            Ok(judged) => judged,
            Err(why) => {
                say!("supervisor: launcher: {} in {} may not start {}: {why}", caller.row.name, caller.session, program.name);
                let _ = conn.try_signal(launch::MSG_REFUSED);
                return;
            }
        };
        let command = match prepared {
            Ok(command) => command,
            Err(e) => {
                say!("supervisor: launcher: cannot start {}: {e}", program.name);
                let _ = conn.try_signal(launch::MSG_REFUSED);
                return;
            }
        };
        let binary = match binary.transpose() {
            Ok(binary) => binary,
            Err(why) => {
                say!("supervisor: launcher: {} was not started: {why}", program.name);
                let _ = conn.try_signal(launch::MSG_REFUSED);
                return;
            }
        };
        let session = starts.session(&mut self.sessions);
        let caller_slots: Vec<(u32, toyos::RawHandle)> =
            request.slot_numbers().zip(slots.0.iter().copied()).collect();
        let launch = Launch {
            conn,
            program,
            command,
            session,
            slots,
            caller_slots,
            _place: place,
            placed: under.is_some(),
            extras,
            cwd: request.cwd.to_string(),
        };
        let Some(binary) = binary else {
            return self.start_launch(launch, None);
        };
        let stored = self.store.find(toyos_manifest::USER, &launch.program.name);
        match grants::decide(stored, &binary, self.system.folder, session.at_screen()) {
            Decision::Start(folder) => {
                if folder.is_none() && self.system.folder.is_some() {
                    let name = &launch.program.name;
                    match stored.filter(|entry| entry.binary == binary).map(|entry| &entry.answer) {
                        Some(Answer::Denied) => {
                            say!("supervisor: launcher: {name} is denied a folder; `grants revoke {name}` asks again")
                        }
                        _ => say!("supervisor: launcher: {name} holds no folder; `grants add {name} <folder>` grants it one"),
                    }
                }
                self.start_launch(launch, folder);
            }
            Decision::Ask => self.ask(launch, binary, parked),
        }
    }

    /// Ask the person at the screen about `launch`, and park it until they
    /// answer ([`Parked`]). One question is up at a time: a launch that would
    /// ask a second is refused by name. A machine with no consent server to
    /// ask starts it with no folder.
    fn ask(&mut self, launch: Launch, binary: String, parked: &mut Option<Parked>) {
        let name = launch.program.name.clone();
        if let Some(up) = parked {
            say!("supervisor: consent: {name} was not started: the question about {} is up", up.launch.program.name);
            let _ = launch.conn.try_signal(launch::MSG_REFUSED);
            return;
        }
        let access = self.system.folder.expect("a launch is asked only under a ceiling");
        let asked = self
            .consent
            .as_ref()
            .ok_or_else(|| format!("this image serves no `{}`", consent::PORT))
            .and_then(|names| names.open(consent::PORT).map_err(|e| format!("its server is gone ({e:?})")))
            .and_then(|conn| {
                let ask = consent::Ask { package: name.clone(), access };
                conn.try_send_bytes(consent::MSG_ASK, &ask.encode())
                    .map(|()| conn)
                    .map_err(|e| format!("the question could not be sent ({e:?})"))
            });
        match asked {
            Ok(consent) => {
                say!("supervisor: consent: {name} asks for a folder {access}; the question is up");
                *parked = Some(Parked { launch, binary, consent, rx: ipc::FrameRx::new(), hangup: ipc::FrameRx::new() });
            }
            Err(why) => {
                say!("supervisor: consent: {name} starts with no folder: nobody was asked, {why}");
                self.start_launch(launch, None);
            }
        }
    }

    /// The parked launch moved on by what woke the loop: withdrawn when its
    /// caller hung up, started on the answer, or started with no folder when
    /// the consent server ended or said something no answer is.
    fn advance_parked(&mut self, parked: Parked, caller_ready: bool, consent_ready: bool) -> Option<Parked> {
        let Parked { launch, binary, consent, mut rx, mut hangup } = parked;
        let name = launch.program.name.clone();
        let caller = match caller_ready {
            true => heard(hangup.pump(&launch.conn), &hangup, "its caller hung up"),
            false => consent::Heard::Nothing,
        };
        let server = match consent_ready {
            true => heard(rx.pump(&consent), &rx, "the consent server ended before it answered"),
            false => consent::Heard::Nothing,
        };
        let reply = match consent::settle(caller, server, self.system.folder) {
            consent::Settled::Waiting => return Some(Parked { launch, binary, consent, rx, hangup }),
            consent::Settled::Withdrawn => {
                say!("supervisor: consent: the question about {name} is withdrawn: its caller hung up");
                return None;
            }
            consent::Settled::Answered(reply) => reply,
        };
        drop(consent);
        let reply = match reply {
            Ok(reply) => reply,
            Err(why) => {
                say!("supervisor: consent: {name} starts with no folder: {why}");
                consent::Reply::Skip
            }
        };
        say!("supervisor: consent: {name}: {}", said(&reply));
        if let Some(answer) = reply.kept() {
            if let Err(why) = self.keep(&name, binary, answer) {
                say!("supervisor: consent: {name}'s answer is not kept, so its next launch asks again: {why}");
            }
        }
        self.start_launch(launch, reply.folder().cloned());
        None
    }

    /// Start `launch`, holding `folder` and in it ([`grants::cwd`]) where it
    /// holds one, and answer its caller.
    fn start_launch(&mut self, launch: Launch, folder: Option<grants::Folder>) {
        let Launch { conn, mut program, mut command, session, slots, caller_slots, _place, placed, extras, cwd } = launch;
        if let Some(folder) = folder {
            // The image `prepare` read is kept: only the working directory is
            // judged again.
            let (path, cwd) = (folder.path.clone(), grants::cwd(&folder.path, &cwd));
            let prepared = self.files("a granted folder", move || -> Result<Command, String> {
                is_folder(&path)?;
                command.current_dir(&cwd).prepare().map_err(|e| format!("{cwd} is no working directory: {e}"))?;
                Ok(command)
            });
            match prepared.and_then(|prepared| prepared) {
                Ok(prepared) => command = prepared,
                Err(why) => {
                    say!("supervisor: launcher: {} was not started: {why}", program.name);
                    let _ = conn.try_signal(launch::MSG_REFUSED);
                    return;
                }
            }
            program.folder = Some(folder);
        }
        let program = &program;

        // `inherit_handle` duplicates into the child, so the supervisor's own copies go with
        // `slots` when this returns.
        self.make_home(program);
        if program.package().is_some() {
            if let Err(why) = self.make_app_home(program) {
                say!("supervisor: launcher: {} was not started: {why}", program.name);
                let _ = conn.try_signal(launch::MSG_REFUSED);
                return;
            }
        }
        let storage = self.slot_storage(program);
        let started = start(
            command,
            program,
            self.system,
            self.syscap,
            Served::Move(&mut self.acceptors),
            &self.connectors,
            &self.grants,
            &extras,
            storage,
            (&self.minted, session),
            Output::Launch { log: &mut self.log, slots: &caller_slots },
        );
        drop(slots);
        match started {
            Ok((child, _)) => {
                // SAFETY: `into_raw_handle` gave up the child's one handle in this table.
                let handle = unsafe { toyos::OwnedHandle::from_raw(toyos::RawHandle(child.into_raw_handle())) };
                // Consumed either way, so the supervisor keeps no `Process` handle from a
                // launch, or a client launching `/system/bin/true` in a loop exhausts the
                // one table the machine cannot do without. A frame refused after the move
                // leaves it queued on a connection this call is about to drop.
                if conn.send_handles([handle]).is_ok() {
                    let _ = conn.try_signal(launch::MSG_LAUNCHED);
                }
            }
            // std's word for the kernel's `Gone` for the place, and nothing else
            // here answers it: the command is prepared, so its spawn calls no
            // file server, and every refusal `start` makes before the spawn is
            // `Other`.
            Err(StartError::Other(e)) if e.kind() == std::io::ErrorKind::BrokenPipe && placed => {
                say!("supervisor: launcher: {} was not started: the place it names is ending", program.name);
                let _ = conn.try_signal(launch::MSG_GONE);
            }
            Err(e) => {
                say!("supervisor: launcher: cannot start {}: {e}", program.name);
                let _ = conn.try_signal(launch::MSG_REFUSED);
            }
        }
    }
}

/// A launch decided up to whether it asks: everything its start needs, owned,
/// so it can wait for an answer ([`Parked`]) as well as start at once.
struct Launch {
    /// The caller's, answered with the process or the refusal.
    conn: Connection,
    program: Program,
    /// Prepared: its image read and hashed, its working directory judged.
    command: Command,
    session: Session,
    /// The caller's stdio and extra slots, owned until the spawn duplicates them.
    slots: Moved,
    caller_slots: Vec<(u32, toyos::RawHandle)>,
    /// The place the child is spawned under, owned until then.
    _place: Moved,
    /// Whether it names a place, or the supervisor.
    placed: bool,
    extras: Vec<(String, Connector)>,
    /// The caller's working directory, which a granted folder's launch keeps
    /// where it lies inside the folder.
    cwd: String,
}

/// A launch waiting on the person at the screen ([`toyos_manifest::consent`]).
///
/// **Ended by events and never by a clock**: the person may take minutes. The
/// caller hanging up withdraws it and closes the question's connection, which
/// takes the prompt down; the consent server's answer starts it; the consent
/// server ending, or saying anything else, starts it with no folder.
struct Parked {
    launch: Launch,
    /// The SHA-256 of the image it starts, which a kept answer is keyed on.
    binary: String,
    /// The question's connection, which the supervisor alone holds.
    consent: Connection,
    rx: ipc::FrameRx<{ consent::MAX_REPLY }>,
    /// The caller's connection after its frame: anything on it is its hang-up.
    hangup: ipc::FrameRx<8>,
}

/// What `rx`'s `step` heard, in [`consent::settle`]'s words: `ended` where
/// its peer hung up.
fn heard<'a, const MAX: usize>(step: RxStep, rx: &'a ipc::FrameRx<MAX>, ended: &'static str) -> consent::Heard<'a> {
    match step {
        RxStep::Idle => consent::Heard::Nothing,
        RxStep::Frame { msg_type, payload_len } => consent::Heard::Frame(msg_type, rx.payload(payload_len)),
        RxStep::Eof => consent::Heard::Ended(ended),
        RxStep::Malformed => consent::Heard::Ended("it sent a frame the protocol cannot describe"),
    }
}

/// The supervisor's word for an answer, in its line in the log.
fn said(reply: &consent::Reply) -> String {
    match reply {
        consent::Reply::Once(folder) => format!("once, {} {}", folder.path, folder.access),
        consent::Reply::Always(folder) => format!("always, {} {}", folder.path, folder.access),
        consent::Reply::Deny => "denied, until `grants revoke`".to_string(),
        consent::Reply::Skip => "skipped, so its next launch asks again".to_string(),
    }
}

/// Handles a launch moved into the supervisor, released on every path out of it.
///
/// A `Drop` and not a close at each `return`: a client picks which way out of
/// `serve_launch` it takes by what it sends.
struct Moved(Vec<toyos::RawHandle>);

impl Moved {
    /// Give up the obligation, for a caller taking it on itself.
    fn take(&mut self) -> Vec<toyos::RawHandle> {
        std::mem::take(&mut self.0)
    }
}

impl Drop for Moved {
    fn drop(&mut self) {
        for handle in &self.0 {
            toyos_abi::syscall::close(*handle);
        }
    }
}

/// The `[programs]` row a launch's path names, following one symlink.
///
/// `/system/bin/ls` is a symlink to `/system/bin/toybox`, and what an applet may hold is
/// `toybox`'s row: the granularity of least authority is the granularity of the
/// binary.
///
/// **The row's own path has to match, and matching it is the whole check.** The
/// path is a client's claim and the row is authority: keyed on the basename
/// alone, a caller writing `/tmp/toybox` would be handed `toybox`'s namespace
/// for a binary it wrote itself. So the answer is a row only where the caller
/// named that row's binary — directly, or through a link that lands on it.
fn declared<'a>(system: &'a Manifest, path: &str) -> Option<&'a Program> {
    let key = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let row = |p: &str| system.program(&key(p)).filter(|program| program.path == p);
    if let Some(program) = row(path) {
        return Some(program);
    }
    let target = std::fs::read_link(path).ok()?;
    row(target.to_str()?)
}

/// What a launch's path resolves to, a row with `judge`'s verdict on it.
enum Resolved<'a, V> {
    Row(&'a Program, V),
    /// An installed package, whose row is the image's `[apps]` list.
    Package(Program, V),
    /// Nothing in the image declares it, and the caller spawns it itself.
    NotDeclared,
    /// A path under `/apps` whose package does not answer for it.
    Refused(String),
}

/// The row a launch's path names, `/apps` included.
///
/// **A launch path under a writable directory is classified by that directory
/// before any symlink is followed, never after.** `declared` follows one link,
/// so asking it first would let `/apps/<anything>/x` — a symlink any process
/// can plant — resolve to the whole `[programs]` row of the binary it points
/// at. A path the kernel would normalize is refused before either happens: it
/// classifies as one thing and opens as another, and `start` spawns the string
/// the client sent, so the two need not even name one file.
///
/// A path under `/apps` that no manifest answers for is refused rather than
/// answered undeclared, because the caller's fallback for undeclared is a
/// direct spawn carrying the caller's own namespace.
fn resolve<'a, V>(system: &'a Manifest, path: &str, judge: impl FnOnce(Target<'_>) -> V) -> Resolved<'a, V> {
    if !package::is_canonical(path) {
        return Resolved::Refused(format!("{path:?} is not a canonical path"));
    }
    let Some(name) = package::package_of(path) else {
        return match declared(system, path) {
            Some(row) => Resolved::Row(row, judge(Target::Row(row))),
            None => Resolved::NotDeclared,
        };
    };
    let row = match installed(system, name) {
        Ok(row) => row,
        Err(why) => return Resolved::Refused(why),
    };
    if row.path != path {
        return Resolved::Refused(format!("{} launches {:?} and this asks for {path:?}", Package::path(name), row.path));
    }
    let verdict = judge(Target::Package(&row));
    Resolved::Package(row, verdict)
}

/// The types a slot's two partitions carry.
const SLOT_KINDS: toyos_update::slots::Kinds =
    toyos_update::slots::Kinds { boot: toyos_gpt::Guid::TOYOS_BOOT.0, root: toyos_gpt::Guid::TOYOS_ROOT.0 };

/// What the block service serves of the slots: the slot table, read through
/// a session on `tables`, its unique GUID, and every partition `tables`,
/// `boots` and `roots` list — each a connector minted for one type, read-only.
/// Its session on the table ends when this returns, and diskserver reads
/// that end before it judges `update`'s open of the same partition.
///
/// **Made on the supervisor's file worker** ([`Supervisor::files`]): a call into a
/// service the supervisor restarts, from its loop alone, would wait in the
/// port's queue for a process only the loop can start.
fn served_slots(
    running: [u8; 16],
    tables: Connector,
    boots: Connector,
    roots: Connector,
) -> Result<(toyos_update::slots::Table, [u8; 16], Vec<toyos_update::slots::Listed>), String> {
    use diskserver::disk::Disk as _;
    let names = |connector: &Connector| {
        namespace::build()
            .add(toyos_blockring::PORT, connector)
            .finish()
            .map_err(|e| format!("no namespace for the block service: {e:?}"))
    };
    let list = |connector: &Connector| {
        diskserver::list(&names(connector)?, toyos_blockring::PORT)
            .map_err(|why| format!("the block service would not list the slots' partitions: {why:?}"))
    };
    let root_parts = list(&roots)?;
    let table_parts = list(&tables)?;
    // The table is the one on the running ROOT's disk.
    let disk = root_parts.iter().find(|p| p.unique == running).map(|p| p.disk);
    let on_its_disk: Vec<_> = table_parts.iter().filter(|p| Some(p.disk) == disk).collect();
    let [table_part] = on_its_disk[..] else {
        return Err(format!(
            "the block service serves {} slot tables on the running ROOT's disk, and a grant needs one",
            on_its_disk.len()
        ));
    };
    let session = diskserver::Session::open(names(&tables)?, toyos_blockring::PORT, table_part.unique)
        .map_err(|why| format!("the slot table would not open: {why:?}"))?;
    let table_unique = table_part.unique;
    let mut disk = diskserver::disk::Served::new(session);
    let (table, _) =
        toyos_update::slots::read(|copies| disk.read(0, copies.as_flattened_mut())).map_err(|why| why.to_string())?;
    let mut listed = table_parts;
    listed.extend(list(&boots)?);
    listed.extend(root_parts);
    let listed = listed
        .into_iter()
        .map(|p| toyos_update::slots::Listed { device: p.disk, type_guid: p.kind, unique_guid: p.unique })
        .collect();
    Ok((table, table_unique, listed))
}

/// What the supervisor says about a device it could not mint a claim for.
///
/// One arm per refusal the kernel distinguishes (`kernel/src/device.rs`'s
/// `ClaimError`, through the codes `sys_device_claim` answers with). The last
/// arm is not a default: it is the answer for a code this call does not
/// produce, and it prints the code rather than inventing a reason.
fn refused(name: &str, why: SyscallError) -> String {
    match why {
        SyscallError::NotFound => format!("no {name} on this machine"),
        SyscallError::AlreadyExists => format!("{name} is already claimed"),
        SyscallError::PermissionDenied => {
            format!("{name} is driven by the kernel and cannot be claimed")
        }
        SyscallError::ResourceExhausted => format!("this machine has no interrupt left for {name}"),
        SyscallError::NotSupported => {
            format!("{name} is on this machine and could not be handed over; the kernel's \
                     `pcidev:`, `isa:` or `acpi:` line says why")
        }
        other => format!("{name} was refused with {other:?}"),
    }
}

/// What the supervisor says about an entry a start holds no claim for.
fn missing(name: &str, why: &Missing) -> String {
    let functions = |at: &[PciAddr]| at.iter().map(|at| format!("PCI {}", pci_at(*at))).collect::<Vec<_>>().join(" and ");
    match why {
        Missing::Unresolved(Unresolved::Absent) => format!("no {name} on this machine"),
        Missing::Unresolved(Unresolved::KernelDriven(at)) => {
            format!("{name} is {}, which the kernel drives and which cannot be claimed", functions(at))
        }
        Missing::Unresolved(Unresolved::Ambiguous(at)) => {
            format!("{name} names {}, and its row has one process to drive one", functions(at))
        }
        Missing::Refused(e) => refused(name, *e),
    }
}

/// A function's address as the kernel's `pcidev:` lines spell it.
fn pci_at(at: PciAddr) -> String {
    format!("{:02x}:{:02x}.{}", at.bus, at.dev, at.func)
}

/// The exit code of `child` once it has ended ([`restart::exited`]).
fn exit_of(child: &Child) -> Option<i32> {
    restart::exited(toyos_abi::syscall::process_wait_nonblock(toyos::RawHandle(child.as_raw_handle())))
}

/// Where the acceptors of the names a started program serves come from.
enum Served<'m, 'a> {
    /// The machine-wide map a launch takes from, by move: such a program is
    /// started once per boot.
    Move(&'m mut BTreeMap<&'a str, Acceptor>),
    /// A boot service's own, which the supervisor keeps and endows a
    /// duplicate of, and the claims the process it follows held
    /// ([`claims::goes_on_without`]).
    Keep { acceptors: &'m [(String, Acceptor)], owed: &'m [String] },
}

/// Build one program's authority and spawn it holding exactly that.
///
/// `extras` are connectors a launching client transferred: names the manifest
/// cannot know because there is one port per instance of whoever made them —
/// a terminal's `surface`. They are added *to* the manifest's row rather than
/// replacing it, and a caller could only transfer what it already held, so a
/// launch confers the row and nothing beyond it.
///
/// Answers the child and the devices of its row it was endowed.
fn start<'a>(
    mut command: Command,
    program: &Program,
    system: &Manifest,
    syscap: &SysCap,
    served: Served<'_, 'a>,
    connectors: &BTreeMap<&str, Connector>,
    grants: &Grants<'_>,
    extras: &[(String, Connector)],
    extra: Extra,
    launcher: (&Minted, Session),
    output: Output<'_>,
) -> Result<(Child, Vec<String>), StartError> {
    let Extra { args, claims, ports } = extra;
    // A storage row's own arguments first: a file server's role leads its argv.
    command.args(&args);
    command.args(&program.args);
    let booting = matches!(output, Output::Boot(_));

    // **Set here, over whatever a launching caller carried**: the row decides
    // where a program's home is, and a service's is made before this
    // (`Supervisor::make_home`), on the supervisor's file worker.
    command.env("HOME", program.home());

    // **Everything endowed stays owned until the spawn that moves it
    // succeeds.** `endow` records a number; a refused spawn moves nothing
    // (`build_child_handles` is all-or-nothing), so a handle whose owner had
    // already been forgotten would be one the supervisor holds for ever and a device
    // class nothing can mint again. A client picks whether a spawn fails — an
    // unreachable `cwd` is enough — so this is its path as much as a bug's.
    let mut held = Moved(Vec::new());
    // Acceptors are given *back* rather than closed: a `serves` port whose
    // acceptor is gone can never be served again.
    let mut taken: Vec<(&'a str, Acceptor)> = Vec::new();

    for (label, claim) in claims {
        let raw = claim.into_raw();
        command.endow(&label, raw.0);
        held.0.push(raw);
    }
    let mut view = grants.view(program, launcher.1);
    view.extend(ports);
    view.extend(grants.netstack(program, launcher.1));
    if let Some(ns) = build_namespace(program, system, connectors, view, extras)? {
        let raw = ns.into_raw();
        command.endow(SVC_LABEL, raw.0);
        held.0.push(raw);
    }
    if let Some(ns) = swap_namespace(program, connectors) {
        let raw = ns.into_raw();
        command.endow(toyos_swap::LABEL, raw.0);
        held.0.push(raw);
    }
    if let Some(ns) = launcher_namespace(program, launcher) {
        let raw = ns.into_raw();
        command.endow(LAUNCHER, raw.0);
        held.0.push(raw);
    }
    if let Some(ns) = grants_namespace(program, launcher) {
        let raw = ns.into_raw();
        command.endow(grants::PORT, raw.0);
        held.0.push(raw);
    }

    // The namespace answers with connections, so a child holding `surface`
    // only there could never hand `surface` to a child of its own — which is
    // the terminal → shell → `locale` chain. The connector travels labelled as
    // well, and a duplicate because the namespace above took one of its own.
    for (name, connector) in extras {
        // **Not an `expect`.** The connector is a client's, and a client may
        // send one it narrowed `DUP` away from. That is a refused launch, not
        // the supervisor's bug — and the supervisor is the one process the machine cannot lose.
        let Ok(handed) = connector.duplicate() else {
            return Err(std::io::Error::other("a provided connector cannot be duplicated").into());
        };
        let raw = handed.into_raw();
        command.endow(&format!("{PROVIDE_PREFIX}{name}"), raw.0);
        held.0.push(raw);
    }

    let mut given_back = None;
    let owed: &[String] = match &served {
        Served::Keep { owed, .. } => owed,
        Served::Move(_) => &[],
    };
    match served {
        Served::Move(acceptors) => {
            for name in &program.serves {
                // `remove_entry` and not `remove`: the key is the map's own
                // `&'a str`, which is what lets a package's synthesized row
                // start through here.
                let (key, acceptor) = acceptors.remove_entry(name.as_str()).unwrap_or_else(|| {
                    // An acceptor is endowed by move, so a `serves` program
                    // started this way is started exactly once per boot. A
                    // second start with no acceptor left is refused by name
                    // rather than spawned with a hole where its own service
                    // should be.
                    panic!("supervisor: `{}` has already been given the `{name}` acceptor", program.name)
                });
                command.endow(&format!("{SERVE_PREFIX}{name}"), acceptor.as_handle().0);
                taken.push((key, acceptor));
            }
            given_back = Some(acceptors);
        }
        Served::Keep { acceptors: kept, .. } => {
            // Every acceptor the service keeps: its row's `serves`, or the
            // directories of a file server's role. None left is a service
            // whose ports closed for good.
            if kept.is_empty() && (!program.serves.is_empty() || !program.roles.is_empty()) {
                return Err(std::io::Error::other("its ports are closed for good").into());
            }
            for (name, acceptor) in kept {
                let handed = toyos_abi::syscall::dup(acceptor.as_handle()).map_err(|e| {
                    std::io::Error::other(format!("the `{name}` acceptor did not duplicate: {e:?}"))
                })?;
                command.endow(&format!("{SERVE_PREFIX}{name}"), handed.0);
                held.0.push(handed);
            }
        }
    }

    let mut endowed: Vec<String> = Vec::new();
    // Read at the first PCI matcher, and once.
    let mut functions: Option<Vec<Pci>> = None;
    for name in &program.devices {
        // The build refuses to render an entry that names no device, so a
        // failure here is an image whose halves disagree.
        let entry = Entry::parse(name).unwrap_or_else(|| panic!("supervisor: `{name}` names no device"));
        let request = match entry {
            Entry::Fixed(request) => Ok(request),
            Entry::Pci(matcher) => {
                let records = match &mut functions {
                    Some(records) => records,
                    None => functions.insert(pci_records(syscap).map_err(StartError::Refused)?),
                };
                claims::function(&matcher, records).map(DeviceRequest::Function).map_err(Missing::Unresolved)
            }
        };
        let minted = request.and_then(|request| claim(syscap, request).map_err(Missing::Refused));
        // The label is the manifest's own spelling, which is exactly what the
        // claimant looks the claim up by: one string, written once.
        let why = match minted {
            Ok(claim) => {
                let raw = claim.into_raw();
                command.endow(&format!("{DEV_PREFIX}{name}"), raw.0);
                held.0.push(raw);
                endowed.push(name.clone());
                continue;
            }
            Err(why) => why,
        };
        // Nothing was spawned, so everything minted goes back with `held`.
        if !claims::goes_on_without(name, &why, owed) {
            let held_before = if owed.contains(name) { ", and the process it replaces held it" } else { "" };
            return Err(StartError::Refused(format!("{}{held_before}", missing(name, &why))));
        }
        // Each refusal keeps the word the kernel gave it. "This machine has
        // none" is a configuration and every other answer is a fault, and one
        // sentence for all of them sends whoever reads the line looking in
        // the wrong place.
        say!("supervisor: {}: {}", program.name, missing(name, &why));
    }

    if !program.syscap.is_empty() {
        // A duplicate carrying exactly what the manifest asked for and nothing
        // else. Rights only shrink, so soundserver's `rt` cap can never mint a claim
        // or open a process however it asks.
        let rights = toyos_manifest::syscap_rights(&program.syscap)
            .unwrap_or_else(|e| panic!("supervisor: {}: {e}", program.name));
        let narrowed = syscap
            .narrowed(rights)
            .expect("supervisor: the system capability refused a narrowed duplicate");
        let raw = narrowed.into_raw();
        command.endow(SYSCAP_LABEL, raw.0);
        held.0.push(raw);
    }

    // Where the program's output goes. Last before the spawn, so nothing above
    // can refuse with a ring or `logkeeper`'s acceptor in flight.
    //
    // A service the supervisor starts gets a ring of its own as its stdout and stderr
    // and the supervisor's console to read as its stdin; `logkeeper` also gets the acceptor
    // the rings reach it on and the one console handle that may write. A
    // launch keeps the slots its caller sent, but for a ring among them —
    // the caller's own log — which is replaced by the program's.
    let mut ring: Option<SharedMemory> = None;
    let mut origins: Option<Acceptor> = None;
    let log: &mut Log = match output {
        Output::Boot(log) => {
            let own = ring.insert(new_ring()).as_handle();
            command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            command.inherit_handle(0, log.stdin.0);
            command.inherit_handle(1, own.0);
            command.inherit_handle(2, own.0);
            if program.name == LOGKEEPER {
                let Some(acceptor) = log.acceptor.take() else {
                    panic!("supervisor: `{LOGKEEPER}` is started twice, and the log's origins are the first's");
                };
                command.endow(ORIGINS, acceptor.as_handle().0);
                origins = Some(acceptor);
                let console = toyos_abi::syscall::dup(toyos::RawHandle(0))
                    .expect("supervisor: its console would not duplicate for logkeeper");
                command.endow(CONSOLE, console.0);
                held.0.push(console);
            }
            log
        }
        Output::Launch { log, slots } => {
            for &(slot, handle) in slots {
                let caller_ring = matches!(slot, 1 | 2)
                    && toyos_abi::syscall::fstat(handle)
                        .is_ok_and(|stat| stat.file_type == FileType::SharedMemory);
                if caller_ring {
                    let own = ring.get_or_insert_with(new_ring).as_handle();
                    command.inherit_handle(slot, own.0);
                } else {
                    command.inherit_handle(slot, handle.0);
                }
            }
            log
        }
    };
    // The ring's end is its writer's: a pipe whose one write end the program
    // holds under a label no child inherits.
    let mut alive = None;
    if ring.is_some() {
        let (read, write) = toyos::pipe_pair()
            .map_err(|e| std::io::Error::other(format!("no pipe for its log's end: {e:?}")))?;
        let write = write.into_raw();
        command.endow(ALIVE, write.0);
        held.0.push(write);
        alive = Some(read);
    }

    match command.spawn() {
        Ok(child) => {
            // The spawn moved every one of them into the child's table, so
            // nothing here may close them.
            held.take();
            for (_, acceptor) in taken {
                let _ = acceptor.into_raw();
            }
            if let Some(acceptor) = origins {
                let _ = acceptor.into_raw();
            }
            if let (Some(ring), Some(alive)) = (ring, alive) {
                log.register(&program.name, child.id(), ring, alive);
            }
            // A launch is answered to its caller and recorded in the kernel's
            // `spawn:`; a line of the supervisor's for each would be a line on the
            // console beside every command typed at it.
            if booting {
                say!("supervisor: started {}", program.name);
            }
            Ok((child, endowed))
        }
        Err(e) => {
            if let Some(acceptors) = given_back {
                for (name, acceptor) in taken {
                    acceptors.insert(name, acceptor);
                }
            }
            if let Some(acceptor) = origins {
                log.acceptor = Some(acceptor);
            }
            Err(e.into())
        }
    }
}

/// Where a started program's output goes: a service's own ring, or a
/// launch's caller's slots with any ring among them replaced.
enum Output<'l> {
    Boot(&'l mut Log),
    Launch { log: &'l mut Log, slots: &'l [(u32, toyos::RawHandle)] },
}

/// The namespace this program's `receives` names, [`toyos_swap::PORT`] and
/// [`toyos_blockring::PORT`] apart, with `view`'s.
///
/// A name some program *provides* rather than serves is not the supervisor's to give: it
/// is one port per instance, made by whoever spawns the holder, and it reaches
/// this program from its own parent. A name that is neither is a config the
/// build-time gate should have refused, so it is a panic here.
/// A refusal is a launch that does not happen, never a panic: `extras` are a
/// client's handles and the kernel judges their type, so `finish` answers
/// `InvalidArgument` for a client that sent a pipe where a connector belongs.
fn build_namespace(
    program: &Program,
    system: &Manifest,
    connectors: &BTreeMap<&str, Connector>,
    view: Vec<(String, Connector)>,
    extras: &[(String, Connector)],
) -> std::io::Result<Option<Namespace>> {
    // Never the swap port nor the grants port: [`swap_namespace`] and
    // [`grants_namespace`] say why. Never the block port's own connector,
    // which reaches nothing: a holder of `block` holds one minted for what it
    // reaches, in `view`, and a disk driver one of `disk` minted for who it
    // is; nor netstack's, which nothing holds: a holder of `netstack` holds
    // one minted for its row and session, in `view` too.
    let receives: Vec<&String> = program
        .receives
        .iter()
        .filter(|name| {
            ![toyos_swap::PORT, grants::PORT, toyos_blockring::PORT, toyos_blockring::disk::PORT, NETSTACK].contains(&name.as_str())
        })
        .collect();
    if receives.is_empty() && extras.is_empty() && view.is_empty() {
        return Ok(None);
    }
    let mut builder = namespace::build();
    for name in receives {
        match connectors.get(name.as_str()) {
            Some(connector) => builder = builder.add(name, connector),
            None => assert!(
                system.programs.iter().any(|p| p.provides.contains(name)),
                "supervisor: {} receives `{name}`, which nothing in this image serves or provides",
                program.name,
            ),
        }
    }
    for (name, connector) in &view {
        builder = builder.add(name, connector);
    }
    // A `provides` name reaches its holder from whoever made the port, never
    // from the supervisor, and this is where it arrives.
    for (name, connector) in extras {
        builder = builder.add(name, connector);
    }
    match builder.finish() {
        Ok(ns) => Ok(Some(ns)),
        Err(e) if extras.is_empty() => {
            panic!("supervisor: no namespace for {}: {e:?}", program.name)
        }
        Err(e) => Err(std::io::Error::other(format!("a provided connector was refused: {e:?}"))),
    }
}

/// What each program's directory capabilities are minted from: each
/// file-server role's port; and what its partitions are, the block service's.
///
/// **Each start is minted grants naming its session's share** (`toyos::fs::Grant`,
/// [`Session::share`]), one per directory of its row's view: a service has a
/// share of its own through every start of it, so the servers count it, every
/// child it spawns directly and every launch made from it that opens no
/// session against one share, and a login session's processes against one
/// more. A role whose ports closed for good is minted nothing.
///
/// **A partition is reached through a connector minted for it**
/// ([`toyos_blockring::wire::Grant`]), never through the block port's own,
/// which no program is handed. **So is netstack**, through one minted with the
/// row and session of the program that holds it.
struct Grants<'a> {
    /// Each role's service, whose kept acceptor is the role's port.
    roles: Vec<(&'a str, Ports)>,
    /// The block service the supervisor started, whose kept acceptor is
    /// [`toyos_blockring::PORT`].
    block: Option<Ports>,
    /// The service the supervisor started serving [`NETSTACK`].
    netstack: Option<Ports>,
}

impl Grants<'_> {
    /// A connector to the block service reaching what `grant` admits; `None`
    /// on a machine whose boot starts no block service, and `Err` once its
    /// port has closed for good.
    fn partitions(&self, grant: BlockGrant) -> Result<Option<Connector>, String> {
        let Some(ports) = &self.block else { return Ok(None) };
        let ports = ports.borrow();
        let Some((_, acceptor)) = ports.iter().find(|(name, _)| name == toyos_blockring::PORT) else {
            return Err("the block service's port has closed for good".into());
        };
        acceptor
            .mint(&grant.encode())
            .map(Some)
            .map_err(|e| format!("no connector to the block service could be minted: {e:?}"))
    }

    /// The block service's `disk` port minted for the disk driver of the
    /// function that arrived as `arrival`.
    fn disk(&self, arrival: Arrival) -> Result<Connector, String> {
        let ports = self.block.as_ref().ok_or("no block service runs to offer a disk to")?.borrow();
        let Some((_, acceptor)) = ports.iter().find(|(name, _)| name == toyos_blockring::disk::PORT) else {
            return Err("the block service's port has closed for good".into());
        };
        acceptor
            .mint(&toyos_blockring::disk::badge(arrival.0))
            .map_err(|e| format!("no connector to the block service could be minted: {e:?}"))
    }

    /// [`NETSTACK`] minted with `program`'s row and `session`, for a row whose
    /// `receives` names it: the badge is the view netstack keeps its DNS
    /// answers in. None once the port has closed for good, as for a role.
    ///
    /// The build renders no manifest in which such a row is netstack's own or
    /// no row `[boot] start` names serves it (`build/build.rs`).
    fn netstack(&self, program: &Program, session: Session) -> Option<(String, Connector)> {
        if !program.receives.iter().any(|name| name == NETSTACK) {
            return None;
        }
        let ports = self
            .netstack
            .as_ref()
            .unwrap_or_else(|| panic!("supervisor: {} receives `{NETSTACK}`, which no started row serves", program.name))
            .borrow();
        let (_, acceptor) = ports.iter().find(|(name, _)| name == NETSTACK)?;
        let badge = Authority { row: program.name.clone(), session }.encode();
        let connector = acceptor
            .mint(&badge)
            .unwrap_or_else(|e| panic!("supervisor: no netstack connector for {}: {e:?}", program.name));
        Some((NETSTACK.to_string(), connector))
    }

    /// The directory capabilities `program` is endowed for one start in
    /// `session`, by namespace name: its row's view (`Program::view`), but
    /// that a storage row sees none, since a file server resolving a path of
    /// its own through itself waits for ever.
    fn view(&self, program: &Program, session: Session) -> Vec<(String, Connector)> {
        if is_storage(program) {
            return Vec::new();
        }
        let wanted = program.view();
        let mut view = Vec::new();
        for (role, ports) in &self.roles {
            let ports = ports.borrow();
            let Some((_, acceptor)) = ports.first() else { continue };
            for dir in wanted.iter().filter(|dir| dir.role == *role) {
                view.push(mint(acceptor, session.share(), dir));
            }
        }
        view
    }
}

/// The longest root a view names, a package's folder of the home at the
/// longest name a package has, is one a grant carries.
const _: () = assert!(
    "home/".len() + toyos_manifest::USER.len() + "/Apps/".len() + toyos_manifest::MAX_PROGRAM_NAME
        <= toyos::fs::MAX_GRANT_ROOT
);

/// A granted folder's root is one a grant carries, and every grant listed is
/// one answer.
const _: () = assert!(
    grants::MAX_ROOT <= toyos::fs::MAX_GRANT_ROOT && grants::MAX_LIST_BYTES <= ipc::MAX_FRAME_LEN as usize
);

/// A grant on `acceptor`, `dir`'s role's port, naming `share`, by the
/// namespace name a program opens it under.
fn mint(acceptor: &Acceptor, share: u64, dir: &View) -> (String, Connector) {
    let access = if dir.write { Access::ReadWrite } else { Access::ReadOnly };
    let mut badge = [0u8; MAX_BADGE];
    let badge = Grant { share, access, root: &dir.root }
        .encode(&mut badge)
        .unwrap_or_else(|| panic!("supervisor: {}'s root {:?} is no grant's, past the bound asserted above", dir.dir, dir.root));
    let connector = acceptor
        .mint(badge)
        .unwrap_or_else(|e| panic!("supervisor: no grant on {} for share {share}: {e:?}", dir.dir));
    (format!("{CAPABILITY_PREFIX}{}", dir.dir), connector)
}

/// [`toyos_swap::PORT`] in a namespace of its own, for a program whose row
/// receives it, endowed under [`toyos_swap::LABEL`].
///
/// **Not an entry of `svc`**, because std gives a duplicate of `svc` to every
/// program its holder spawns directly — sshserver running one the manifest does not
/// declare — and to their descendants. A label other than `svc` is not
/// inherited, so the port stays with the one process the supervisor endowed.
fn swap_namespace(program: &Program, connectors: &BTreeMap<&str, Connector>) -> Option<Namespace> {
    if !program.receives.iter().any(|name| name == toyos_swap::PORT) {
        return None;
    }
    let connector = connectors
        .get(toyos_swap::PORT)
        .expect("supervisor: the manifest declares the supervisor serves `swap`");
    let ns = namespace::build()
        .add(toyos_swap::PORT, connector)
        .finish()
        .unwrap_or_else(|e| panic!("supervisor: no swap namespace for {}: {e:?}", program.name));
    Some(ns)
}

/// A `launcher` minted with `program`'s row and the session it runs in, in a
/// namespace of its own, for a row whose `starts` lists anything; endowed under
/// [`LAUNCHER`] and never an entry of `svc`, for [`swap_namespace`]'s reason.
fn launcher_namespace(program: &Program, (minted, session): (&Minted, Session)) -> Option<Namespace> {
    if program.starts.is_empty() {
        return None;
    }
    let badge = Authority { row: program.name.clone(), session }.encode();
    let connector = minted
        .launcher
        .mint(&badge)
        .unwrap_or_else(|e| panic!("supervisor: no launcher for {}: {e:?}", program.name));
    let ns = namespace::build()
        .add(LAUNCHER, &connector)
        .finish()
        .unwrap_or_else(|e| panic!("supervisor: no launcher namespace for {}: {e:?}", program.name));
    Some(ns)
}

/// [`grants::PORT`] minted with `program`'s row and the session it runs in, in
/// a namespace of its own, for the row whose `receives` names it; endowed under
/// that name and never an entry of `svc`, for [`swap_namespace`]'s reason.
/// The badge is who asks: the supervisor honours a login session's alone.
fn grants_namespace(program: &Program, (minted, session): (&Minted, Session)) -> Option<Namespace> {
    if !program.receives.iter().any(|name| name == grants::PORT) {
        return None;
    }
    let badge = Authority { row: program.name.clone(), session }.encode();
    let connector = minted
        .grants
        .mint(&badge)
        .unwrap_or_else(|e| panic!("supervisor: no grants connector for {}: {e:?}", program.name));
    let ns = namespace::build()
        .add(grants::PORT, &connector)
        .finish()
        .unwrap_or_else(|e| panic!("supervisor: no grants namespace for {}: {e:?}", program.name));
    Some(ns)
}

/// One of the supervisor's words on a swap: its line in the log.
fn swapped(service: &str, word: Word, detail: &str) {
    say!("{}", toyos_swap::said(service, word, detail));
}

/// What a process is started with beside its row: arguments, a `drives`
/// process's claim on its function endowed by label, and connectors in its
/// namespace by name, each minted for what it reaches: the partitions the
/// block service serves, or a disk driver's `disk`.
#[derive(Default)]
struct Extra {
    args: Vec<String>,
    claims: Vec<(String, toyos::Device)>,
    ports: Vec<(String, Connector)>,
}

/// Every record the kernel's inventory answers.
fn inventory(syscap: &SysCap) -> Result<Vec<toyos_abi::inventory::Record>, String> {
    syscap
        .records(|n| vec![toyos_abi::inventory::RawRecord::EMPTY; n])
        .map_err(|why| why.to_string())
}

/// Every PCI function the kernel's inventory answers.
fn pci_records(syscap: &SysCap) -> Result<Vec<Pci>, String> {
    let records = inventory(syscap).map_err(|why| format!("the inventory was not read: {why}"))?;
    Ok(records.into_iter().filter_map(|r| if let Record::Pci(pci) = r { Some(pci) } else { None }).collect())
}

/// The claim `request` asks for.
fn claim(syscap: &SysCap, request: DeviceRequest) -> Result<toyos::Device, SyscallError> {
    match request {
        DeviceRequest::Class(class) => syscap.claim(class),
        DeviceRequest::Function(arrival) => syscap.claim_function(arrival),
        DeviceRequest::Isa(set) => syscap.claim_isa(set),
    }
}

/// The unique GUID the loader named for `role`.
fn loaded(records: &[toyos_abi::inventory::Record], role: toyos_abi::inventory::Role) -> Option<[u8; 16]> {
    records.iter().find_map(|r| match r {
        toyos_abi::inventory::Record::Loaded(l) if l.role == role => Some(l.unique_guid),
        _ => None,
    })
}

fn guid_text(guid: [u8; 16]) -> String {
    toyos_gpt::Guid(guid).to_string()
}

/// A file server's arguments and block connector for one start.
///
/// A file server is told its role, and for LOG and BOOT the partition's GUID
/// the loader named, with `block` minted for that partition alone, writing for
/// the log and reading for the boot volume; DATA it finds by type, through
/// `block` minted for every DATA partition. A log or boot role the loader
/// named no partition for, and a block port closed for good, each refuse the
/// start: the role is then absent, never served from memory.
fn storage_endowment(role: Option<&str>, syscap: &SysCap, grants: &Grants<'_>) -> Result<Extra, StartError> {
    use toyos_abi::inventory::Role;
    let mut storage = Extra::default();
    let Some(role) = role else {
        return Ok(storage);
    };
    storage.args.push(role.to_string());
    let served = match role {
        "data" => BlockGrant { scope: Scope::Kind(toyos_gpt::Guid::TOYOS_DATA.0), writes: true },
        "log" | "boot" => {
            let records = inventory(syscap).map_err(|why| StartError::Other(std::io::Error::other(why)))?;
            let which = if role == "log" { Role::Log } else { Role::Boot };
            let Some(guid) = loaded(&records, which) else {
                return Err(StartError::Refused(format!("the loader named no `{role}` partition")));
            };
            storage.args.push(guid_text(guid));
            BlockGrant { scope: Scope::Unique(guid), writes: role == "log" }
        }
        other => panic!("supervisor: `{other}` is no role; the build refuses it"),
    };
    if let Some(port) = grants.partitions(served).map_err(StartError::Refused)? {
        storage.ports.push((toyos_blockring::PORT.to_string(), port));
    }
    Ok(storage)
}

/// A file of ROOT, through the kernel's own `open`.
///
/// **Not through std**: std resolves a path through this process's namespace,
/// and the first path it resolves fixes that namespace for the process's life.
/// The supervisor builds its namespace out of the manifest, so ROOT's files are read
/// before std is asked anything.
fn read_root(path: &str) -> Result<Vec<u8>, SyscallError> {
    use toyos_abi::syscall::{self, OpenFlags};
    let handle = syscall::open(path.as_bytes(), OpenFlags::READ)?;
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    let read = loop {
        match syscall::read(handle, &mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(e) => break Err(e),
        }
    };
    syscall::close(handle);
    read.map(|()| bytes)
}

/// Say which build this machine runs: the first thing it says, so a boot
/// that dies past here names its build in its log.
fn say_release() {
    let path = toyos_osrelease::GUEST_PATH;
    let bytes = read_root(path).unwrap_or_else(|e| panic!("supervisor: cannot read {path}: {e:?}"));
    let release = toyos_osrelease::parse(&bytes)
        .unwrap_or_else(|e| panic!("supervisor: {path} is not one the build wrote: {e:?}"));
    say!(
        "{}{} {}, committed {} UTC, toolchain {}, {}",
        toyos_osrelease::SAID,
        release.commit.as_str(),
        release.tree,
        toyos_wallclock::Civil::from_unix_secs(release.committed),
        release.toolchain.as_str(),
        release.arch.machine()
    );
}
