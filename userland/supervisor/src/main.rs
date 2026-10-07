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
//! keeps each of its acceptors and endows a duplicate, so a swap
//! ([`toyos_swap`]) can stop the process, start another binary holding the same
//! manifest row, and leave every client's connector naming the same port — a
//! connect made in the gap waits in that port's queue for the new process. A
//! service that ends with no swap owning it has its kept acceptors closed at
//! once, so a client's next connect is `ServerGone` exactly as it is for a
//! server nothing keeps. A program started through `launcher` still gets its
//! acceptor by move and is started once per boot.
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
//! anything a launching caller carried: a service its own `/state/<name>`, made
//! before it runs, and everything else the session user's home, made at boot.
//! A launch of a program no row names is answered with the session's, which
//! the caller's direct spawn carries in place of its own.
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
use std::collections::{BTreeMap, VecDeque};
use std::os::toyos::process::{ChildExt, CommandExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use toyos_swap::{Refusal, Request as SwapRequest, Word};

use toyos_manifest::launch::{self as authority, Authority, Session, Target};
use toyos_manifest::package::{self, Package};
use toyos_manifest::{Manifest, Program};
use toyos::endow::Endowments;
use toyos::fs::{Grant, CAPABILITY_PREFIX};
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
use toyos_logstream::{
    Registration, Tag, ALIVE, CONSOLE, FLUSH, FLUSHED, LOGKEEPER, MAX_TAG, ORIGINS, REGISTER, RESUME, STOPPING,
    SWAP, SWAP_BACK, SWAP_LEAVING,
};
use toyos_abi::syscall::{
    DeviceRequest, FileType, SyscallError, DEV_PREFIX, MAX_BADGE, PROVIDE_PREFIX, SERVE_PREFIX,
    SVC_LABEL, SYSCAP_LABEL,
};

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
    Swap,
    Power,
}

/// The poll tokens for the `launcher`, `swap` and `power` acceptors, and for the
/// one connection whose swap has been answered and whose hang-up is awaited. A
/// pending connection's token is [`TOKEN_PENDING_BASE`] plus its handle, which
/// is unique among the connections the supervisor holds at once.
const TOKEN_ACCEPTOR: u64 = 0;
const TOKEN_SWAP_ACCEPTOR: u64 = 1;
const TOKEN_HANGUP: u64 = 2;
const TOKEN_POWER_ACCEPTOR: u64 = 3;
const TOKEN_WAKE: u64 = 4;
const TOKEN_PENDING_BASE: u64 = 5;

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

    /// Tell `logkeeper` a word on a swap of the service its readers are carried by.
    fn carrier(&self, word: u8) {
        if let Err(e) = self.conn.send_bytes(SWAP, &[word]) {
            panic!("supervisor: logkeeper's origins connection refused a swap's word: {e:?}");
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

    // One port per file-server role, made here for the same reason: a
    // program's first open works whether or not its server runs yet. Its
    // connector goes at once: every connector a program holds to it is a
    // grant minted for that program's start (`Grants`).
    let mut role_acceptors: BTreeMap<&str, Acceptor> = BTreeMap::new();
    for role in system.programs.iter().flat_map(|p| p.roles.iter()) {
        let (acceptor, _) = port::create().unwrap_or_else(|e| panic!("supervisor: no port for the `{role}` role: {e:?}"));
        role_acceptors.insert(role, acceptor);
    }
    // The supervisor's own files are resolved through grants like every
    // program's, under the instance no start is given, and it is the one
    // process nobody endows a namespace: std resolves through this one, and
    // the stop's syncs through the second.
    let files: &'static Namespace = {
        let own: Vec<(String, Connector)> = role_acceptors
            .iter()
            .flat_map(|(role, acceptor)| mint_grants(role, acceptor, SUPERVISOR_INSTANCE))
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
    let (wake_read, wake_write) = toyos::pipe_pair().expect("supervisor: no pipe to hear a service end");
    let mut supervisor = Supervisor {
        system,
        launcher,
        syscap: &syscap,
        acceptors,
        connectors,
        grants: Grants { roles: Vec::new(), next: Cell::new(SUPERVISOR_INSTANCE + 1) },
        files,
        services: Vec::new(),
        log,
        wake: wake_read,
        worker: Worker::start(),
        restarting: false,
    };
    supervisor.boot(&mut role_acceptors, wake_write.into_raw());

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
    /// Waits on `done` and on a service's end together.
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
        Self { jobs, done, owed: 0, owed_what: String::new(), poller: Poller::new(2) }
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
    !program.roles.is_empty() || program.serves.iter().any(|s| s == toyos_blockring::PORT)
}

/// Everything the supervisor's loop acts on, for the machine's life.
struct Supervisor<'a> {
    system: &'static Manifest,
    /// What every launcher is minted on and every launch accepted from.
    launcher: Acceptor,
    syscap: &'a SysCap,
    /// The `serves` acceptors nobody has been started with yet, which a launch
    /// takes by move.
    acceptors: BTreeMap<&'a str, Acceptor>,
    connectors: BTreeMap<&'a str, Connector>,
    /// What each program's view is minted from.
    grants: Grants<'a>,
    /// The same directories as a namespace of the supervisor's own, for the stop's syncs.
    files: &'static Namespace,
    /// What `[boot] start` named, a file server once per role.
    services: Vec<Service<'a>>,
    /// Where a service the supervisor (re)starts writes its stdout and stderr, whichever
    /// process ends up holding its row.
    log: Log,
    /// Readable when a `restart` service has ended and is owed a new process.
    wake: Pipe,
    /// Where the supervisor's calls into the file servers are made.
    worker: Worker,
    /// Inside [`Supervisor::restart_ended`]: a file call made there leaves the wake
    /// for the loop, which is the one caller that acts on it.
    restarting: bool,
}

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
    kept: Arc<Mutex<Kept>>,
    /// When each recent start after an end happened, for [`toyos_manifest::RESTARTS`].
    restarts: VecDeque<Instant>,
}

/// What a service's waiter thread shares with the loop.
struct Kept {
    /// The acceptors of every name the service serves. Empty once the service
    /// has ended with no swap owning it and no restart owed: the port is then
    /// closed for good.
    acceptors: Vec<(String, Acceptor)>,
    /// Counts the service's starts, so a waiter answers only for its own.
    generation: u64,
    /// A swap owns the service: its process ending is expected and the port
    /// stays open for the next one.
    swapping: bool,
    /// The row says the supervisor starts it again when it ends.
    restart: bool,
    /// It ended, and the loop owes it a new process on the same ports.
    ended: bool,
    /// The write end of [`Supervisor::wake`].
    wake: toyos::RawHandle,
}

impl<'a> Service<'a> {
    fn new(
        program: &'a Program,
        role: Option<&'a str>,
        acceptors: Vec<(String, Acceptor)>,
        wake: toyos::RawHandle,
    ) -> Self {
        Self {
            program,
            role,
            path: program.path.clone(),
            child: None,
            devices: Vec::new(),
            kept: Arc::new(Mutex::new(Kept {
                acceptors,
                generation: 0,
                swapping: false,
                restart: program.restart,
                ended: false,
                wake,
            })),
            restarts: VecDeque::new(),
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
        system: &Manifest,
        syscap: &SysCap,
        connectors: &BTreeMap<&str, Connector>,
        grants: &Grants<'_>,
        launcher: &Acceptor,
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
        let mut kept = self.kept.lock().expect("supervisor: a service's state is poisoned");
        // Every start after the first follows a process of this service
        // that the supervisor stopped or saw end.
        let served = match kept.generation {
            0 => Served::Keep(&kept.acceptors),
            _ => Served::Restart { acceptors: &kept.acceptors, owed },
        };
        let storage = storage_endowment(self.program, self.role, syscap)?;
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
            (launcher, Session::Machine),
            Output::Boot(log),
        )?;
        kept.generation += 1;
        kept.ended = false;
        if !kept.acceptors.is_empty() {
            let process = toyos_abi::syscall::dup(toyos::RawHandle(child.as_raw_handle()))
                .unwrap_or_else(|e| panic!("supervisor: {}'s process handle: {e:?}", self.program.name));
            let (shared, generation) = (Arc::clone(&self.kept), kept.generation);
            std::thread::Builder::new()
                .name(format!("wait-{}", self.program.name))
                .spawn(move || close_when_it_ends(&shared, generation, process))
                .expect("supervisor: a service's waiter could not be started");
        }
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

/// Why a service did not start.
enum StartError {
    /// The partition its file-server role is on was refused, so the role is
    /// absent: its ports close and nothing serves its paths from memory.
    Partition(String),
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
            Self::Partition(why) => f.write_str(why),
            Self::Other(e) => e.fmt(f),
        }
    }
}

/// Wait for one start of a service to end, and close its ports if nothing was
/// expecting it to, or wake the loop to start it again if its row says so.
fn close_when_it_ends(kept: &Mutex<Kept>, generation: u64, process: toyos::RawHandle) {
    let _ = toyos_abi::syscall::process_wait(process);
    toyos_abi::syscall::close(process);
    let mut kept = kept.lock().expect("supervisor: a service's state is poisoned");
    if kept.generation == generation && !kept.swapping {
        if kept.restart {
            kept.ended = true;
            // A full pipe is a wake already pending, which is all this is.
            let _ = toyos_abi::syscall::write_nonblock(kept.wake, &[1]);
        } else {
            kept.acceptors.clear();
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
    /// process it started. The storage rows go first — every other start may
    /// make a directory, and a directory is a file server's — and the
    /// session's home is made before the first row that is not one.
    fn boot(&mut self, role_acceptors: &mut BTreeMap<&str, Acceptor>, wake: toyos::RawHandle) {
        let system = self.system;
        let storage_first = system
            .start
            .iter()
            .filter(|n| system.program(n).is_some_and(is_storage))
            .chain(system.start.iter().filter(|n| system.program(n).is_none_or(|p| !is_storage(p))));
        let mut homes_made = false;
        for name in storage_first {
            let program = system
                .program(name)
                .unwrap_or_else(|| panic!("supervisor: [boot] start names `{name}`, which is not declared"));
            if !is_storage(program) && !homes_made {
                self.make_session_home();
                homes_made = true;
            }
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
                self.make_home(program);
                let mut service = Service::new(program, role, kept, wake);
                if let Some(role) = role {
                    self.grants.roles.push((role, Arc::clone(&service.kept)));
                }
                let started =
                    service.spawn(&program.path, &[], system, self.syscap, &self.connectors, &self.grants, &self.launcher, &mut self.log);
                match started {
                    Ok(_) => {}
                    Err(StartError::Partition(why)) => {
                        service.kept.lock().expect("supervisor: a service's state is poisoned").acceptors.clear();
                        say!("supervisor: {} did not start ({why}); its ports are closed this boot", service.label());
                    }
                    Err(e) => panic!("supervisor: cannot start {}: {e}", program.name),
                }
                self.services.push(service);
            }
        }
        if !homes_made {
            self.make_session_home();
        }
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

    /// `work`, a call into the file servers, made on [`Worker`], and its
    /// answer — with every server that ends meanwhile started again, so a call
    /// its end left waiting in the port's queue goes on to the new process.
    /// `Err` is the call still unanswered at [`FILES_BOUND`], which the worker
    /// goes on waiting for alone, or the worker still waiting on an earlier
    /// one.
    fn files<T: Send + 'static>(&mut self, what: &str, work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
        const DONE: u64 = 0;
        const WAKE: u64 = 1;
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
                self.worker.poller.watch(&self.wake, READABLE, WAKE);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let mut woken = false;
            self.worker.poller.wait(1, left.as_nanos() as u64, |token| woken |= token == WAKE);
            if woken {
                let mut sink = [0u8; 64];
                while matches!(self.wake.read_nonblock(&mut sink), Ok(n) if n > 0) {}
                self.restart_ended();
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
        let poller = Poller::new(5 + MAX_PENDING_LAUNCHES as u32);
        let mut pending: Vec<Pending> = Vec::new();
        let mut flight: Option<Flight> = None;
        let mut ready: Vec<u64> = Vec::new();
        loop {
            poller.watch(&self.launcher, READABLE, TOKEN_ACCEPTOR);
            poller.watch(swap, READABLE, TOKEN_SWAP_ACCEPTOR);
            poller.watch(power, READABLE, TOKEN_POWER_ACCEPTOR);
            poller.watch(&self.wake, READABLE, TOKEN_WAKE);
            if let Some(Flight { phase: Phase::Answered { conn, .. }, .. }) = &flight {
                poller.watch(conn, READABLE, TOKEN_HANGUP);
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
                (TOKEN_ACCEPTOR, &self.launcher, (|s, c| s.caller(c).map(Port::Launcher)) as fn(&Self, &Connection) -> _),
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
                                self.serve_launch(&p.conn, &caller, msg_type, p.rx.payload(payload_len))
                            }
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

            if ready.contains(&TOKEN_WAKE) {
                let mut sink = [0u8; 64];
                while matches!(self.wake.read_nonblock(&mut sink), Ok(n) if n > 0) {}
                self.restart_ended();
            }
        }
    }

    /// The row and session a launcher connection's badge names. Only this
    /// supervisor mints on its launcher, so a refusal here is its own bug, said
    /// and the connection dropped.
    fn caller(&self, conn: &Connection) -> Result<Caller, String> {
        let mut badge = [0u8; MAX_BADGE];
        let badge = self.launcher.badge(conn, &mut badge).map_err(|e| format!("its connection has no badge ({e:?})"))?;
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
            swapped(&self.log, service, Word::Refused, &why.to_string());
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
            &self.log,
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
        let service = &mut self.services[index];
        swapped(
            &self.log,
            &name,
            Word::Stopping,
            &format!("pid {} ({previous})", service.pid().map_or(0, |p| p)),
        );
        service.kept.lock().expect("supervisor: a service's state is poisoned").swapping = true;
        if let Some(mut old) = service.child.take() {
            // Killed and then waited for, because the kernel publishes a
            // process's end only after its handle table is closed: once the
            // wait returns, every device claim it held is back and can be
            // minted again.
            let _ = old.kill();
            let _ = old.wait();
        }
        let started =
            service.spawn(&path, &owed, self.system, self.syscap, &self.connectors, &self.grants, &self.launcher, &mut self.log);
        match started {
            Ok(pid) => {
                swapped(
                    &self.log,
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
                swapped(&self.log, &name, Word::Failed, &format!("{path} did not start: {e}"));
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
        let status = {
            // Asked under the lock the waiter takes, so an end after this
            // answer is the waiter's to act on and an end before it is this
            // function's.
            let mut kept = service.kept.lock().expect("supervisor: a service's state is poisoned");
            let status = service.child.as_mut().map(|c| c.try_wait());
            if matches!(status, Some(Ok(None))) {
                kept.swapping = false;
            }
            status
        };
        match status {
            Some(Ok(None)) => {
                swapped(
                    &self.log,
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
                    &self.log,
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
                let answer = toyos::fs::Dir::connect(names, &asked)
                    .and_then(|mut dir| {
                        dir.sync().map_err(|e| match e {
                            toyos::fs::Refused::Error(e) => e,
                            toyos::fs::Refused::Link(_) => SyscallError::Unknown,
                        })
                    });
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

    /// Start again every `restart` service that ended, on the ports it kept,
    /// unless it has ended [`toyos_manifest::RESTARTS`] times inside
    /// [`toyos_manifest::RESTART_WINDOW_SECS`]: then its ports close, and a
    /// client's next connection is answered `Gone`.
    fn restart_ended(&mut self) {
        self.restarting = true;
        let window = Duration::from_secs(toyos_manifest::RESTART_WINDOW_SECS);
        for index in 0..self.services.len() {
            let ended = self.services[index].kept.lock().expect("supervisor: a service's state is poisoned").ended;
            if !ended {
                continue;
            }
            let service = &mut self.services[index];
            let label = service.label();
            let now = Instant::now();
            while service.restarts.front().is_some_and(|at| now.duration_since(*at) > window) {
                service.restarts.pop_front();
            }
            let pid = service.pid().map_or(0, |p| p);
            if let Some(mut old) = service.child.take() {
                let _ = old.wait();
            }
            if service.restarts.len() as u32 >= toyos_manifest::RESTARTS {
                let mut kept = service.kept.lock().expect("supervisor: a service's state is poisoned");
                kept.ended = false;
                kept.acceptors.clear();
                say!(
                    "supervisor: {label} ended {} times in {} s; its ports are closed, its clients answered Gone, and a program started from now on is given none of its directories",
                    toyos_manifest::RESTARTS + 1,
                    toyos_manifest::RESTART_WINDOW_SECS
                );
                continue;
            }
            service.restarts.push_back(now);
            let (path, owed, program) = (service.path.clone(), service.devices.clone(), service.program);
            self.make_home(program);
            let service = &mut self.services[index];
            match service.spawn(&path, &owed, self.system, self.syscap, &self.connectors, &self.grants, &self.launcher, &mut self.log) {
                Ok(new) => say!("supervisor: {label} (pid {pid}) ended; started again as pid {new}"),
                Err(e) => {
                    let mut kept = service.kept.lock().expect("supervisor: a service's state is poisoned");
                    kept.ended = false;
                    kept.acceptors.clear();
                    say!("supervisor: {label} ended and would not start again ({e}); its ports are closed");
                }
            }
        }
        self.restarting = false;
    }

    /// Start the binary a failed swap replaced, or close the service's ports
    /// when that will not start either.
    fn restore(&mut self, index: usize, previous: &str, owed: &[String]) {
        let program = self.services[index].program;
        self.make_home(program);
        let service = &mut self.services[index];
        let name = service.program.name.clone();
        match service.spawn(previous, owed, self.system, self.syscap, &self.connectors, &self.grants, &self.launcher, &mut self.log) {
            Ok(pid) => {
                service.kept.lock().expect("supervisor: a service's state is poisoned").swapping = false;
                swapped(&self.log, &name, Word::Restored, &format!("{previous} as pid {pid}"));
            }
            Err(e) => {
                let mut kept = service.kept.lock().expect("supervisor: a service's state is poisoned");
                kept.swapping = false;
                kept.acceptors.clear();
                swapped(
                    &self.log,
                    &name,
                    Word::Gone,
                    &format!("{previous} did not start either ({e}); its ports are closed"),
                );
            }
        }
    }
}

/// A staged or installed binary's bytes, refused past [`toyos_swap::MAX_BINARY_BYTES`]
/// before any of it is read. Made from the loop: a swap's files are under
/// [`toyos_swap::STAGING`], which the kernel serves.
fn read_binary(path: &str) -> Result<Vec<u8>, Refusal> {
    let unreadable = |e: std::io::Error| Refusal::Unreadable { path: path.to_string(), why: e.to_string() };
    let len = std::fs::metadata(path).map_err(unreadable)?.len();
    if len > toyos_swap::MAX_BINARY_BYTES {
        return Err(Refusal::TooLarge(len));
    }
    std::fs::read(path).map_err(unreadable)
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
    /// One `MSG_LAUNCH`, from the frame to the `Process` handle that answers it.
    ///
    /// **Everything in the request is a client's claim about itself; `caller` is
    /// not.** A program nothing declares is refused by name; a frame that does
    /// not decode is a dropped connection and nothing else; a row the caller's
    /// row may not start is refused before any of its files is read. What the
    /// child ends up holding is the manifest's row for it plus whatever
    /// connectors the caller transferred, and the caller could only transfer
    /// what it already had — so a launch confers exactly the manifest row and
    /// nothing beyond it.
    fn serve_launch(&mut self, conn: &Connection, caller: &Caller, msg_type: u32, payload: &[u8]) {
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
        // Owned, so they close when this call returns: `SYS_NAMESPACE_BUILD` copies
        // a connector into the namespace and leaves the caller's handle, and the supervisor's
        // copy of a client's connector has no life beyond this launch.
        let extras: Vec<(&str, Connector)> = names
            .into_iter()
            .zip(extra_handles.iter().copied())
            // SAFETY: the kernel moved these into the supervisor's table with the frame, and
            // nothing else answers for them. **Not a claim about the type** — a
            // client sends what it likes, and everything below treats a wrong one
            // as a refused launch rather than as the supervisor's own bug.
            .map(|(name, handle)| (name, unsafe { Connector::from_raw(handle) }))
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

        let installed;
        // On the worker, every file the launch reads: a path under `/apps` is
        // read off its package, and the program and its working directory may
        // be a file server's, which the supervisor supervises. Whether the
        // caller may start it is asked between the two, so a refused launch
        // reads no image and judges no directory.
        let (system, path) = (self.system, request.program.to_string());
        let (row, session) = (caller.row, caller.session);
        let found = self.files("a launch's files", move || {
            resolve(system, &path, |target| -> Result<_, authority::Refusal> {
                let session = authority::may_start(row, session, target)?;
                let (Target::Row(program) | Target::Package(program)) = target;
                let prepared = command.image_from(Path::new(&program.path)).prepare().map(drop);
                Ok((session, prepared.map(|()| command)))
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
            Resolved::Row(row, verdict) => (row, verdict),
            Resolved::Package(row, verdict) => {
                installed = row;
                (&installed, verdict)
            }
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
        let (session, prepared) = match verdict {
            Ok(judged) => judged,
            Err(why) => {
                say!("{}", authority::refused(&caller.row.name, caller.session, &program.name, why));
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
        let caller_slots: Vec<(u32, toyos::RawHandle)> =
            request.slot_numbers().zip(slots.0.iter().copied()).collect();

        // `inherit_handle` duplicates into the child, so the supervisor's own copies go with
        // `slots` when this returns.
        self.make_home(program);
        let started = start(
            command,
            program,
            self.system,
            self.syscap,
            Served::Move(&mut self.acceptors),
            &self.connectors,
            &self.grants,
            &extras,
            Storage::default(),
            (&self.launcher, session),
            Output::Launch { log: &mut self.log, slots: &caller_slots },
        );
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
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe && under.is_some() => {
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
    let file = Package::path(name);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) => return Resolved::Refused(format!("{file} cannot be read: {e}")),
    };
    let installed = match Package::parse(&text) {
        Ok(installed) => installed,
        Err(why) => return Resolved::Refused(why),
    };
    if installed.name != name {
        return Resolved::Refused(format!("{file} calls itself {:?}", installed.name));
    }
    if installed.program != path {
        return Resolved::Refused(format!(
            "{file} launches {:?} and this asks for {path:?}",
            installed.program
        ));
    }
    let row = system.app_row(name, path);
    let verdict = judge(Target::Package(&row));
    Resolved::Package(row, verdict)
}

/// The slot table's partition and the idle slot's two, claimed: the grant a
/// `slots` row is endowed (`toyos_update::slots`).
///
/// **Which slot is idle is the kernel's word, not the table's**: the running
/// ROOT is the TOYOS-ROOT partition the kernel holds, the table is the one on
/// that ROOT's disk, and the idle slot is the one whose ROOT is not it — and
/// its two partitions are claimed only as `toyos_update::slots::grant` admits
/// them.
fn slot_grant(syscap: &SysCap) -> Result<[(&'static str, toyos::Device); 3], String> {
    use toyos_abi::inventory::{PartState, Partition, Record};
    let parts: Vec<Partition> = inventory(syscap)?
        .into_iter()
        .filter_map(|r| match r {
            Record::Partition(p) => Some(p),
            _ => None,
        })
        .collect();
    let one = |what: &str, found: Vec<&Partition>| match found[..] {
        [p] => Ok(*p),
        _ => Err(format!("the machine has {} {what}, and a grant needs one", found.len())),
    };
    let running = one(
        "ROOT partitions the kernel holds",
        parts
            .iter()
            .filter(|p| p.type_guid == toyos_gpt::Guid::TOYOS_ROOT.0 && p.state == PartState::Kernel)
            .collect(),
    )?;
    let table_part = one(
        "slot tables on the running ROOT's disk",
        parts
            .iter()
            .filter(|p| p.device == running.device && p.type_guid == toyos_gpt::Guid::TOYOS_SLOTS.0)
            .collect(),
    )?;
    let claim = |guid: [u8; 16], what: &str| {
        syscap
            .claim_partition::<toyos::Device>(toyos_abi::part::PartGuid(guid))
            .map_err(|e| refused(&format!("the {what}"), e))
    };
    let table_claim = claim(table_part.unique_guid, "slot table")?;
    let mut copies: [toyos_abi::part::Block; 2] = [[0; toyos_abi::part::BLOCK_BYTES]; 2];
    toyos_abi::syscall::partition_read(table_claim.as_handle(), 0, &mut copies)
        .map_err(|e| format!("the slot table would not read: {e:?}"))?;
    let (table, _) = toyos_update::slots::current([&copies[0], &copies[1]])
        .map_err(|why| format!("the slot table's partition holds {why}"))?;
    // The table is the grantee's to write, so what it names is held to the
    // inventory before anything is claimed.
    let listed = |p: &Partition| toyos_update::slots::Listed {
        device: p.device,
        type_guid: p.type_guid,
        unique_guid: p.unique_guid,
    };
    let kinds = toyos_update::slots::Kinds { boot: toyos_gpt::Guid::TOYOS_BOOT.0, root: toyos_gpt::Guid::TOYOS_ROOT.0 };
    let all: Vec<_> = parts.iter().map(listed).collect();
    let (idle, slot) =
        toyos_update::slots::grant(&table, &listed(&running), &all, kinds).map_err(|why| why.to_string())?;
    let boot = claim(slot.boot, "idle slot's volume")?;
    let root = claim(slot.root, "idle slot's ROOT")?;
    say!("supervisor: the idle slot is {}, granted with the slot table", idle.letter());
    Ok([
        (toyos_update::slots::TABLE_LABEL, table_claim),
        (toyos_update::slots::BOOT_LABEL, boot),
        (toyos_update::slots::ROOT_LABEL, root),
    ])
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
        SyscallError::InvalidArgument => {
            format!("{name} names more than one device on this machine")
        }
        SyscallError::PermissionDenied => {
            format!("{name} is driven by the kernel and cannot be claimed")
        }
        SyscallError::ResourceExhausted => format!("no claim slot is free for {name}"),
        SyscallError::NotSupported => {
            format!("{name} is on this machine and could not be handed over; the kernel's \
                     `pcidev:`, `partclaim:`, `isa:` or `acpi:` line says why")
        }
        other => format!("{name} was refused with {other:?}"),
    }
}

/// Where the acceptors of the names a started program serves come from.
enum Served<'m, 'a> {
    /// The machine-wide map a launch takes from, by move: such a program is
    /// started once per boot.
    Move(&'m mut BTreeMap<&'a str, Acceptor>),
    /// A boot service's own, which the supervisor keeps and endows a duplicate of.
    Keep(&'m [(String, Acceptor)]),
    /// The same, for a service the supervisor has just stopped: the claims that process
    /// held may still be on their way back, and `owed` names them. A start
    /// that cannot mint one of them is refused, never a service running on
    /// without a device the process before it held.
    Restart { acceptors: &'m [(String, Acceptor)], owed: &'m [String] },
}

/// How long a restart waits for the claims of the process it stopped to come
/// back before it mints the new process's.
///
/// **A compromise over a kernel defect, recorded as one**: the kernel publishes
/// a process's end before every deferred release its handles queued has run
/// (`issues/deferred-release-outlives-its-syscall.md`), so a claim asked
/// for the instant `wait` returns is refused as still held. Its release is one
/// pass of the zero-handle drain; this bound is a liveness guard, and it goes
/// when that issue closes.
const CLAIM_RETURN: Duration = Duration::from_secs(2);

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
    extras: &[(&str, Connector)],
    storage: Storage,
    launcher: (&Acceptor, Session),
    output: Output<'_>,
) -> std::io::Result<(Child, Vec<String>)> {
    // A storage row's own arguments first: a file server's role leads its argv.
    command.args(&storage.args);
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

    for (label, claim) in storage.claims {
        let raw = claim.into_raw();
        command.endow(&label, raw.0);
        held.0.push(raw);
    }
    if let Some(ns) = build_namespace(program, system, connectors, grants, extras)? {
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

    // The namespace answers with connections, so a child holding `surface`
    // only there could never hand `surface` to a child of its own — which is
    // the terminal → shell → `locale` chain. The connector travels labelled as
    // well, and a duplicate because the namespace above took one of its own.
    for (name, connector) in extras {
        // **Not an `expect`.** The connector is a client's, and a client may
        // send one it narrowed `DUP` away from. That is a refused launch, not
        // the supervisor's bug — and the supervisor is the one process the machine cannot lose.
        let Ok(handed) = connector.duplicate() else {
            return Err(std::io::Error::other("a provided connector cannot be duplicated"));
        };
        let raw = handed.into_raw();
        command.endow(&format!("{PROVIDE_PREFIX}{name}"), raw.0);
        held.0.push(raw);
    }

    let mut given_back = None;
    let (restart, owed): (bool, &[String]) = match &served {
        Served::Restart { owed, .. } => (true, owed),
        Served::Move(_) | Served::Keep(_) => (false, &[]),
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
        Served::Keep(kept) | Served::Restart { acceptors: kept, .. } => {
            // Every acceptor the service keeps: its row's `serves`, or the
            // directories of a file server's role. None left is a service
            // whose ports closed for good.
            if kept.is_empty() && (!program.serves.is_empty() || !program.roles.is_empty()) {
                return Err(std::io::Error::other("its ports are closed for good"));
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
    let mut unpaid = None;
    for name in &program.devices {
        // The build system already refused a config this cannot parse
        // (`names_only_real_capabilities`), so a failure here is an image built
        // against a different ABI rather than somebody's typo.
        let request = DeviceRequest::parse(name)
            .unwrap_or_else(|| panic!("supervisor: `{name}` is not a device this ABI has"));
        // A device this machine does not have is not endowed, and the supervisor says
        // which: "did I get an HDA or a virtio-sound?" becomes "which claims
        // are in my endowment table?", which is the same question with the
        // answer already in hand.
        //
        // The label is the manifest's own spelling, which is exactly what the
        // claimant looks the claim up by: one string, written once.
        let mint = || match request {
            DeviceRequest::Class(class) => syscap.claim::<toyos::Device>(class),
            DeviceRequest::Pci(id) => syscap.claim_pci::<toyos::Device>(id),
            DeviceRequest::Partition(name) => syscap.claim_partition::<toyos::Device>(name),
            DeviceRequest::Isa(set) => syscap.claim_isa::<toyos::Device>(set),
        };
        let asked = Instant::now();
        let mut held_still = 0u32;
        let minted = loop {
            match mint() {
                Err(SyscallError::AlreadyExists)
                    if restart && asked.elapsed() < CLAIM_RETURN =>
                {
                    held_still += 1;
                    std::thread::sleep(Duration::from_millis(1));
                }
                minted => break minted,
            }
        };
        // Said, so what the kernel's deferred release costs a restart is
        // counted wherever it is paid.
        if held_still > 0 && minted.is_ok() {
            say!(
                "supervisor: {}: {name} came back from the process it replaces after {} ms and \
                 {held_still} refusal(s)",
                program.name,
                asked.elapsed().as_millis()
            );
        }
        match minted {
            Ok(claim) => {
                let raw = claim.into_raw();
                command.endow(&format!("{DEV_PREFIX}{name}"), raw.0);
                held.0.push(raw);
                endowed.push(name.clone());
            }
            Err(e) if owed.contains(name) => {
                unpaid = Some(std::io::Error::other(format!(
                    "{}, and the process it replaces held it",
                    refused(name, e)
                )));
                break;
            }
            // Each refusal keeps the word the kernel gave it. "This machine
            // has none" is a configuration and every other answer is a fault,
            // and one sentence for all six sends whoever reads the line looking
            // in the wrong place.
            Err(e) => {
                say!("supervisor: {}: {}", program.name, refused(name, e));
                // A block service is told, so the partitions on a controller
                // the machine has are refused and never taken for none.
                if e != SyscallError::NotFound && program.serves.iter().any(|s| s == toyos_blockring::PORT) {
                    command.args(["--claim-refused", name.as_str()]);
                }
            }
        }
    }
    // The idle slot, for the one program whose row asks for it: minted here,
    // against the ROOT the kernel holds, so the slot this boot runs is never
    // among what is endowed. A machine with none says so and the program
    // starts holding nothing, which it refuses by name.
    if program.slots {
        match slot_grant(syscap) {
            Ok(claims) => {
                for (label, claim) in claims {
                    let raw = claim.into_raw();
                    command.endow(label, raw.0);
                    held.0.push(raw);
                }
            }
            Err(why) => say!("supervisor: {}: no slot to grant: {why}", program.name),
        }
    }
    // Nothing was spawned, so everything minted goes back with `held`.
    if let Some(unpaid) = unpaid {
        return Err(unpaid);
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
            Err(e)
        }
    }
}

/// Where a started program's output goes: a service's own ring, or a
/// launch's caller's slots with any ring among them replaced.
enum Output<'l> {
    Boot(&'l mut Log),
    Launch { log: &'l mut Log, slots: &'l [(u32, toyos::RawHandle)] },
}

/// The namespace this program's `receives` names, [`toyos_swap::PORT`] apart.
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
    grants: &Grants<'_>,
    extras: &[(&str, Connector)],
) -> std::io::Result<Option<Namespace>> {
    // Never the swap port: [`swap_namespace`] says why.
    let receives: Vec<&String> =
        program.receives.iter().filter(|name| *name != toyos_swap::PORT).collect();
    let view = grants.view(program);
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

/// The instance of the supervisor's own grants, which no start is given.
const SUPERVISOR_INSTANCE: u64 = 0;

/// What each program's directory capabilities are minted from: each
/// file-server role's port, and the count of starts.
///
/// **Each start is minted grants of its own** (`toyos::fs::Grant`), one per
/// directory of every role, all naming one instance, so the servers count the
/// process and every child it spawns directly, which holds the same
/// connectors, against one share. A role whose ports closed for good is
/// minted nothing.
struct Grants<'a> {
    /// Each role's service, whose kept acceptor is the role's port.
    roles: Vec<(&'a str, Arc<Mutex<Kept>>)>,
    /// The instance the next start is minted.
    next: Cell<u64>,
}

impl Grants<'_> {
    /// The directory capabilities `program` is endowed for one start, by
    /// namespace name.
    ///
    /// **Every program sees the whole tree the file servers serve**, which is
    /// the kernel's old view kept whole until each row declares its own
    /// (`issues/every-program-sees-only-the-files-it-was-given.md`, stage 2),
    /// with one exception: a storage row sees none, since a file server
    /// resolving a path of its own through itself waits for ever. Asked before
    /// anything is locked, since a storage row's start holds its own kept state.
    fn view(&self, program: &Program) -> Vec<(String, Connector)> {
        if is_storage(program) {
            return Vec::new();
        }
        let instance = self.next.get();
        self.next.set(instance + 1);
        let mut view = Vec::new();
        for (role, kept) in &self.roles {
            let kept = kept.lock().expect("supervisor: a service's state is poisoned");
            if let Some((_, acceptor)) = kept.acceptors.first() {
                view.extend(mint_grants(role, acceptor, instance));
            }
        }
        view
    }
}

/// A grant on `acceptor`, the `role`'s port, for each of its directories,
/// naming `instance`: each by the namespace name a program opens it under.
fn mint_grants(role: &str, acceptor: &Acceptor, instance: u64) -> Vec<(String, Connector)> {
    let dirs = toyos_manifest::role_dirs(role).expect("supervisor: the build refuses a role it does not know");
    dirs.iter()
        .map(|dir| {
            let mut badge = [0u8; MAX_BADGE];
            let badge = Grant { instance, root: dir.root }
                .encode(&mut badge)
                .unwrap_or_else(|| panic!("supervisor: {}'s root {:?} is no grant's", dir.dir, dir.root));
            let connector = acceptor
                .mint(badge)
                .unwrap_or_else(|e| panic!("supervisor: no grant on {} for instance {instance}: {e:?}", dir.dir));
            (format!("{CAPABILITY_PREFIX}{}", dir.dir), connector)
        })
        .collect()
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
fn launcher_namespace(program: &Program, (launcher, session): (&Acceptor, Session)) -> Option<Namespace> {
    if program.starts.is_empty() {
        return None;
    }
    let badge = Authority { row: program.name.clone(), session }.encode();
    let connector = launcher
        .mint(&badge)
        .unwrap_or_else(|e| panic!("supervisor: no launcher for {}: {e:?}", program.name));
    let ns = namespace::build()
        .add(LAUNCHER, &connector)
        .finish()
        .unwrap_or_else(|e| panic!("supervisor: no launcher namespace for {}: {e:?}", program.name));
    Some(ns)
}

/// One of the supervisor's words on a swap: its line in the log, and — for the service
/// `logkeeper`'s network readers are carried by — the frame `logkeeper` acts on. The
/// frame and not the line: nothing a program writes is a word `logkeeper` obeys.
fn swapped(log: &Log, service: &str, word: Word, detail: &str) {
    say!("{}", toyos_swap::said(service, word, detail));
    if service == toyos_logstream::CARRIER {
        match word {
            Word::Accepted => log.carrier(SWAP_LEAVING),
            // Each said once the netstack it replaces has been waited for.
            Word::Started | Word::Failed | Word::Restored | Word::Gone => log.carrier(SWAP_BACK),
            Word::Refused | Word::Stopping | Word::InService => {}
        }
    }
}

/// What a storage row's process is started with beside its row: its
/// arguments, and the claims on the partition a file server's role is on
/// where a disk the kernel drives carries it.
#[derive(Default)]
struct Storage {
    args: Vec<String>,
    claims: Vec<(String, toyos::Device)>,
}

/// Every record the kernel's inventory answers.
fn inventory(syscap: &SysCap) -> Result<Vec<toyos_abi::inventory::Record>, String> {
    syscap
        .records(|n| vec![toyos_abi::inventory::RawRecord::EMPTY; n])
        .map_err(|why| why.to_string())
}

/// The unique GUID the loader named for `role`.
fn loaded(records: &[toyos_abi::inventory::Record], role: toyos_abi::inventory::Role) -> Option<[u8; 16]> {
    records.iter().find_map(|r| match r {
        toyos_abi::inventory::Record::Loaded(l) if l.role == role => Some(l.unique_guid),
        _ => None,
    })
}

fn guid_text(guid: [u8; 16]) -> String {
    let mut buf = [0u8; toyos_abi::part::GUID_TEXT_LEN];
    toyos_abi::part::PartGuid(guid).write_text(&mut buf).to_string()
}

/// A storage row's arguments and claims for one start.
///
/// A block service is told the ROOT the machine runs from, which it serves no
/// session on. A file server is told its role, and gets a claim on every
/// partition of its role a disk the kernel drives carries — the stick, until
/// usbd serves it — and otherwise the partition's GUID, which it opens through
/// the block service; DATA it finds by type there itself, and counts with its
/// claims. A log or boot role the loader named no partition for, and a claim
/// the kernel refuses, each refuse the start: the role is then absent, never
/// served from memory.
fn storage_endowment(program: &Program, role: Option<&str>, syscap: &SysCap) -> Result<Storage, StartError> {
    use toyos_abi::inventory::{Record, Role};
    let mut storage = Storage::default();
    let is_block = program.serves.iter().any(|s| s == toyos_blockring::PORT);
    if !is_block && role.is_none() {
        return Ok(storage);
    }
    let records = inventory(syscap).map_err(|why| StartError::Other(std::io::Error::other(why)))?;
    if is_block {
        if let Some(root) = loaded(&records, Role::Root) {
            storage.args.extend(["--running".to_string(), guid_text(root)]);
        }
        return Ok(storage);
    }
    let role = role.expect("checked above");
    storage.args.push(role.to_string());
    let on_kernel_disk = |pick: &dyn Fn(&toyos_abi::inventory::Partition) -> bool| -> Vec<[u8; 16]> {
        records
            .iter()
            .filter_map(|r| match r {
                Record::Partition(p) if pick(p) => Some(p.unique_guid),
                _ => None,
            })
            .collect()
    };
    let (kernel, named) = match role {
        "data" => (on_kernel_disk(&|p| p.type_guid == toyos_gpt::Guid::TOYOS_DATA.0), None),
        "log" | "boot" => {
            let which = if role == "log" { Role::Log } else { Role::Boot };
            let Some(guid) = loaded(&records, which) else {
                return Err(StartError::Partition(format!("the loader named no `{role}` partition")));
            };
            (on_kernel_disk(&|p| p.unique_guid == guid), Some(guid))
        }
        other => panic!("supervisor: `{other}` is no role; the build refuses it"),
    };
    for guid in &kernel {
        let request = toyos_abi::syscall::DeviceRequest::Partition(toyos_abi::part::PartGuid(*guid));
        let mut buf = [0u8; toyos_abi::syscall::DeviceRequest::MAX_NAME];
        let name = request.write_name(&mut buf).to_string();
        match syscap.claim_partition::<toyos::Device>(toyos_abi::part::PartGuid(*guid)) {
            Ok(claim) => storage.claims.push((format!("{DEV_PREFIX}{name}"), claim)),
            Err(e) => return Err(StartError::Partition(refused(&name, e))),
        }
    }
    if let (Some(guid), []) = (named, kernel.as_slice()) {
        storage.args.push(guid_text(guid));
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
