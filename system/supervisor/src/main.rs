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
//! `power` connector asks, the supervisor signs every login session out and,
//! once they have ended, has `logkeeper` flush and answer, and only then asks
//! the kernel, which stops every process at once and waits for none.
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
//! **A service keeps one job across its starts**, its row's `system.toml`
//! floor held on it, so a control path started again is owed what it was.
//! **A start refused for memory waits for it to come back**: the service
//! keeps its ports, and its job's readiness, posted when what it lacked is
//! free, is what starts it again ([`Supervisor::memory_returned`]).
//!
//! **A `drives` row has one process per PCI function it names**, each holding
//! that function's claim and started when the kernel's device set
//! ([`toyos::devset`]) shows the function free
//! ([`toyos_supervisor::drives`]). The supervisor alone holds the set, minted
//! from its `DEVICE` right, and reads it whenever it changes, by the time its
//! snapshot names, and after every end: a function that arrives is answered
//! by the same start as one at boot, and one that leaves has its process left
//! to end on its claim's `Gone`. **Every end is readiness on the process's
//! handle**, watched on the loop's own poller, and what an end owes is
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
//! read-write, and every other row the whole tree but `/state`, of which it
//! holds its own directory alone, where it keeps any (`Program::state_dir`).
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

mod sessions;
mod stopping;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::os::toyos::io::AsRawHandle;
use std::os::toyos::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use toyos_swap::{Refusal, Request as SwapRequest, Word};

use toyos_manifest::consent;
use toyos_manifest::grants::{self, Answer, Decision, Store};
use toyos_manifest::launch::{self as authority, Authority, Session, Sessions, Target};
use toyos_manifest::package::{self, Package};
use toyos_manifest::device::{Entry, PciMatch};
use toyos_manifest::{Manifest, Program, Source, View};
use toyos_supervisor::claims::{self, Missing, Unresolved};
use toyos_supervisor::drives::{self, Act, Drives};
use toyos_supervisor::restart::{self, AfterEnd, Budget};
use toyos::endow::{Endowments, FromHandle};
use toyos::fs::{Access, Grant, CAPABILITY_PREFIX};
use toyos::ipc::{self, Connection, RxStep};
use toyos::launch::{self, Parent, Request, LAUNCHER};
use toyos::namespace::{self, Namespace};
use toyos::poller::{Poller, READABLE, WRITABLE};
use toyos::port::{self, Acceptor, Connector};
use toyos::log::region::{Ring, RING_BYTES};
use toyos::power;
use toyos::say;
use toyos::wake::{self, Bell, Waker};
use toyos::shm::SharedMemory;
use toyos::syscap::SysCap;
use toyos::AsHandle;
use toyos_abi::Rights;
use toyos_blockring::wire::{Grant as BlockGrant, Scope};
use toyos_logstream::{
    Registration, Tag, ALIVE, CONSOLE, FLUSH, FLUSHED, LOGKEEPER, MAX_TAG, ORIGINS, REGISTER, RESUME, STOPPING,
};
use toyos_abi::inventory::{Arrival, Pci, PciAddr, Record};
use toyos_abi::syscall::{
    DeviceRequest, FileType, SyscallError, DEV_PREFIX, FILESERVER_IMAGE_LABEL, JOB_LABEL, MAX_BADGE, PROVIDE_PREFIX,
    ROOT_IMAGE_LABEL, SERVE_PREFIX, SVC_LABEL, SYSCAP_LABEL,
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

/// One caller's inbound frames, taken without waiting: the supervisor never
/// waits on a client. Room for the largest frame, because a launch carries
/// the child's argv, environment and working directory.
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
    /// The login authority asking for a session, off its badge.
    Login(&'static Program),
    /// The `logout` asking about the sessions open, off its badge.
    Admin(Caller),
    Swap,
    Power,
}

/// The ports the supervisor mints a row's badge on, its row and session: the
/// launcher every row whose `starts` lists anything holds, and
/// [`grants::PORT`], which its one holder asks on.
struct Minted {
    launcher: Acceptor,
    grants: Acceptor,
    /// [`toyos_session::LOGIN_PORT`], minted with a `login` row's key.
    login: Acceptor,
    /// [`toyos_session::ADMIN_PORT`], minted with its holder's row and session.
    admin: Acceptor,
}

/// What one start is minted: the session its files and grants are in, and
/// the launcher port its launches are minted on with their session.
#[derive(Clone, Copy)]
struct Minting<'m> {
    minted: &'m Minted,
    session: Session,
    /// `None` for a start that holds no launcher minted here: a `login` row
    /// outside a login session, whose launches are a session's alone.
    launches: Option<(&'m Acceptor, Session)>,
}

impl<'m> Minting<'m> {
    /// A start in the machine's session, its launches on the machine's
    /// launcher, but for a `login` row's, which hold none of it.
    fn machine(minted: &'m Minted, program: &Program, session: Session) -> Self {
        let launches = toyos_supervisor::sessions::holds_machine_launcher(program).then_some((&minted.launcher, session));
        Self { minted, session, launches }
    }
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
const TOKEN_LOGIN_ACCEPTOR: u64 = 8;
const TOKEN_ADMIN_ACCEPTOR: u64 = 9;
/// Every session owner's link, and the ends of every session's apps.
const TOKEN_LINKS: u64 = 10;
const TOKEN_APPS: u64 = 11;
/// The job of every service waiting for memory to come back to start again,
/// watched in the place of the process it has none of.
const TOKEN_MEMORY: u64 = 12;
/// The device set changed.
const TOKEN_DEVICES: u64 = 13;
const TOKEN_PENDING_BASE: u64 = 14;
/// Above every pending connection's token, which is a handle's past the base.
const TOKEN_POWER_WATCHERS: u64 = u64::MAX;

/// The processes whose end the supervisor watches at once: its services and
/// its `drives` processes, in what a poller holds beside the loop's own
/// watches. A `drives` process past it is not started, and says so.
const MAX_CHILDREN: usize = Poller::MAX_HANDLES as usize
    - TOKEN_PENDING_BASE as usize
    - MAX_PENDING_LAUNCHES
    - 2 * sessions::MAX_SESSIONS;

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
    stdin: toyos::OwnedHandle,
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

/// [`new_ring`] for a program started in `job`, counted to it: a ring is the
/// program's output, and the supervisor's own floor pays for none of it.
/// Refused where `job` has no memory for it.
fn ring_for(job: toyos::BorrowedHandle<'_>) -> std::io::Result<SharedMemory> {
    let region = SharedMemory::create_for(job, RING_BYTES).map_err(memory_refused)?;
    view(&region).lay_out_with_placeholder_owner();
    Ok(region)
}

/// A refusal the kernel answered as an `io::Error` of the kind std gives it,
/// so a start refused for memory is told apart from one refused otherwise.
fn memory_refused(e: SyscallError) -> std::io::Error {
    match e {
        SyscallError::ResourceExhausted => std::io::Error::new(std::io::ErrorKind::OutOfMemory, "no memory left for it"),
        other => std::io::Error::other(format!("{other:?}")),
    }
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
        let stdin = toyos::fd::with(0, |o| o.handle().map(|console| console.try_clone_narrowed(STDIN_RIGHTS)))
            .ok()
            .flatten()
            .expect("supervisor: the kernel gave it no console as descriptor 0")
            .expect("supervisor: its console would not narrow");
        let log = Self {
            conn,
            acceptor: Some(acceptor),
            stdin,
            rx: RefCell::new(ipc::FrameRx::new()),
            unanswered: Cell::new(0),
        };
        // The supervisor's own lines: a ring like every program's, as the two
        // descriptors the kernel gave its console.
        let ring = new_ring();
        let copy = ring.as_handle().try_clone_to_owned().expect("supervisor: its ring would not duplicate");
        let held = toyos::fd::install(toyos::fd::Object::handle(copy), false, 0)
            .expect("supervisor: no descriptor for its own ring");
        for (fd, stream) in [(1, toyos::log::stdio::Stream::Out), (2, toyos::log::stdio::Stream::Err)] {
            // SAFETY: the kernel's console descriptors, which nothing in this
            // process owns: std's stdio names them by number and asks again.
            unsafe { toyos::fd::dup2(held, fd, false) }.expect("supervisor: its ring would not take its own descriptor");
            toyos::log::stdio::forget(stream);
        }
        // SAFETY: the descriptor installed above, named nowhere else.
        unsafe { toyos::fd::close(held) }.expect("supervisor: the ring's descriptor it just made");
        let (keep, alive) = wake::pair().expect("supervisor: no event to say it lives");
        // Held for the machine's life: the supervisor's end is the machine's.
        core::mem::forget(keep);
        log.register("supervisor", toyos_abi::syscall::getpid().0, ring, alive);
        log
    }

    /// Name program `pid` the owner of `ring`, its log, and move the ring to
    /// `logkeeper` under `name`, with the bell whose only waker is that
    /// program's.
    fn register(&self, name: &str, pid: u32, ring: SharedMemory, alive: Bell) {
        let Some(tag) = Tag::new(name) else {
            panic!("supervisor: `{name}` is not a name a line of the log can carry");
        };
        view(&ring).own(pid);
        let ring = ring.share().expect("supervisor: a log ring would not duplicate");
        let mut payload = [0u8; 4 + MAX_TAG];
        let len = Registration { pid, tag }.encode(&mut payload);
        if let Err(e) = self.conn.send_bytes_with(REGISTER, &payload[..len], [ring, alive.into()]) {
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
            // Ready or the bound: the next pump and the check above say which.
            if let Err(e) = toyos::poller::wait_one(&self.conn, READABLE, left.as_nanos() as u64) {
                toyos::warn!("supervisor: the kernel refused a wait on logkeeper's answer ({e:?}); stopping without it");
                return;
            }
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
    let mut log = Log::open();

    let syscap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("supervisor: the kernel spawns this program holding the system capability");
    let boot: &'static Boot = Box::leak(Box::new(Boot::take()));
    let sources = syscap.boot_sources().expect("supervisor: the kernel answers the system capability its boot sources");
    let booted = toyos_supervisor::boot::read(&sources);
    for line in &booted.lines {
        say!("{line}");
    }
    BOOT_CONTROLLER.set(booted.controller).expect("supervisor: the boot sources are read once");

    // **Everything the supervisor's own files and the SYSTEM server need is
    // made before a file is read**, since the manifest is a file of `/system`
    // and SYSTEM is what serves it. One port per file-server role, then: a
    // program's first open works whether or not its server runs yet. Its
    // connector goes at once: every connector a program holds to it is a
    // grant minted for that program's share (`Grants`). A role no row serves
    // has its port closed once the manifest says so.
    let mut role_acceptors: BTreeMap<&str, Acceptor> = BTreeMap::new();
    for (role, _) in toyos_manifest::ROLES {
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
            .map(|view| (&role_acceptors[view.role], view))
            .map(|(acceptor, view)| mint(acceptor, authority::SUPERVISOR_SHARE, view))
            .collect();
        let build = || {
            let mut builder = namespace::build();
            for (name, connector) in &own {
                builder = builder.add(name, connector);
            }
            builder.finish().expect("supervisor: no namespace for its own files")
        };
        if let Err(e) = std::os::toyos::fs::adopt_namespace(build().into()) {
            panic!("supervisor: std would not take its namespace ({e}), and the supervisor is endowed none");
        }
        Box::leak(Box::new(build()))
    };
    // Its connector goes at once: every launcher a program holds is one minted
    // with that program's row.
    let (launcher, _) = port::create().expect("supervisor: no port for `launcher`");
    let (grants_port, grants_connector) = port::create().expect("supervisor: no port for `grants`");
    let (login, _) = port::create().expect("supervisor: no port for `login`");
    let (admin, _) = port::create().expect("supervisor: no port for `session-admin`");
    let minted = Minted { launcher, grants: grants_port, login, admin };
    let mut sessions = Sessions::default();

    // SYSTEM, from the file server's boot image: what serves `/system`. No
    // manifest row holds it, since it serves the manifest.
    let system_row: &'static Program = Box::leak(Box::new(system_row()));
    let acceptor = role_acceptors.remove(toyos_manifest::SYSTEM).expect("SYSTEM is a role");
    let mut system_service = Service::new(
        system_row,
        Some(toyos_manifest::SYSTEM),
        vec![(format!("{CAPABILITY_PREFIX}{}", toyos_manifest::SYSTEM), acceptor)],
        sessions.machine(),
    );
    let no_grants = Grants { roles: Vec::new(), block: None, netstack: None };
    let unread = Manifest::default();
    let started = system_service.spawn(
        &system_row.source,
        &[],
        Vec::new(),
        &unread,
        &syscap,
        &BTreeMap::new(),
        &no_grants,
        &minted,
        boot,
        &mut log,
    );
    if let Err(e) = started {
        bootstrap_fails(format!("the file server serving /system did not start: {e}"));
    }

    say_release(&system_service);
    let text = read_system(toyos_manifest::GUEST_PATH, &system_service);
    // For the machine's life, and `'static` so a launch's path can be resolved
    // against it on the supervisor's file worker.
    let system: &'static Manifest = Box::leak(Box::new(toyos_manifest::parse(&text)));
    // Its own floor, the protected capacity of the machine's controls: held
    // through a handle to its own job, kept for the machine's life.
    let own = toyos::job::own().expect("supervisor: no handle to its own job");
    if system.supervisor_floor > 0 {
        match syscap.hold_floor(&own, system.supervisor_floor) {
            Ok(()) => say!("supervisor: {} MiB held back for its own job", system.supervisor_floor >> 20),
            Err(e) => toyos::error!("supervisor: its floor of {} MiB could not be held ({e:?}); the controls run unprotected", system.supervisor_floor >> 20),
        }
    }
    let _own: &'static toyos::job::Job = Box::leak(Box::new(own));
    // A role no row serves: closed before anything asks it, so a connect to it
    // answers `ServerGone` rather than waiting in a queue nothing will ever
    // accept from.
    role_acceptors.retain(|role, _| system.programs.iter().any(|p| p.roles.iter().any(|r| r == role)));

    // Before anything is spawned, and for every `serves` name in the manifest
    // rather than only the ones `[boot] start` names: the filepicker is
    // launched by the compositor, and an editor holding its connector must be
    // able to ask for a file before the picker has run an instruction.
    // `grants` was made above, before the manifest said the supervisor serves it.
    assert!(
        system.supervisor_serves.iter().any(|name| name == grants::PORT),
        "supervisor: the manifest declares the supervisor serves `grants`"
    );
    let mut acceptors = BTreeMap::new();
    let mut connectors: BTreeMap<&str, Connector> = BTreeMap::from([(grants::PORT, grants_connector)]);
    let names = system
        .served_names()
        .into_iter()
        .chain(system.supervisor_serves.iter().map(String::as_str))
        .filter(|name| ![grants::PORT, toyos_session::ADMIN_PORT].contains(name));
    for name in names {
        let (acceptor, connector) =
            port::create().unwrap_or_else(|e| panic!("supervisor: no port for `{name}`: {e:?}"));
        acceptors.insert(name, acceptor);
        connectors.insert(name, connector);
    }
    // Its connector goes at once: every `netstack` a program holds is one
    // minted with that program's row and session (`Grants::netstack`).
    drop(connectors.remove(NETSTACK));

    // The supervisor alone holds a connector to it, so a connection on it is
    // the supervisor's question and nobody else's.
    let consent = connectors.remove(consent::PORT).map(|connector| {
        namespace::build()
            .add(consent::PORT, &connector)
            .finish()
            .expect("supervisor: no namespace for the consent port")
    });
    let mut grants = no_grants;
    grants.roles.push((toyos_manifest::SYSTEM, Rc::clone(&system_service.ports)));
    let mut supervisor = Supervisor {
        system,
        minted,
        store: Store::default(),
        consent,
        syscap: &syscap,
        boot,
        acceptors,
        connectors,
        grants,
        sessions,
        files,
        services: vec![system_service],
        drivers: drives::matchers(system),
        drives: Drives::default(),
        devices: syscap.device_set().expect("supervisor: its system capability mints the device set"),
        listed: Vec::new(),
        settle: None,
        instances: Vec::new(),
        undriven: Vec::new(),
        log,
        worker: Worker::start(),
        restarting: false,
        logins: sessions::Logins::default(),
        stops: Default::default(),
        watchers: Vec::new(),
        watch: sessions::Watch::new(),
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
    /// Rung as a job finishes.
    done: Bell,
    /// Jobs finished since last counted.
    finished: Arc<AtomicU32>,
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
        let (ring, done) = wake::pair().expect("supervisor: no event for its file worker");
        let finished = Arc::new(AtomicU32::new(0));
        let count = Arc::clone(&finished);
        std::thread::Builder::new()
            .name("files".into())
            .spawn(move || {
                for job in queue {
                    job();
                    count.fetch_add(1, Ordering::Release);
                    ring.wake();
                }
            })
            .expect("supervisor: its file worker could not be started");
        let poller = Poller::new((1 + MAX_CHILDREN + sessions::MAX_SESSIONS) as u32);
        Self { jobs, done, finished, owed: 0, owed_what: String::new(), poller }
    }

    /// Count the jobs that have finished since last asked: the bell is taken
    /// first, so a job counted after it rings it again.
    fn settle(&mut self) {
        self.done.try_take().expect("supervisor: its file worker has ended");
        self.owed -= self.finished.swap(0, Ordering::Acquire);
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

/// A row holding the `acpi` claim, whose holder declares the memory every PCI
/// claim waits for: started before every other row.
fn declares(program: &Program) -> bool {
    program.devices.iter().any(|device| device == "acpi")
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
    /// The images the kernel handed the supervisor.
    boot: &'static Boot,
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
    /// The kernel's device set, and the arrivals its last reading listed.
    devices: toyos::devset::DeviceSet,
    listed: Vec<Pci>,
    /// When the set may next change with no wake to say so.
    settle: Option<Instant>,
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
    /// The login sessions open, each with its owner.
    logins: sessions::Logins,
    /// The ends of every app started in a login session.
    watch: sessions::Watch,
    /// The machine's stop, if one is pending.
    stops: toyos_supervisor::stop::Stops,
    /// The requesters told what the stop waits for, as it changes.
    watchers: Vec<stopping::Watcher>,
}

/// One `drives` process, and the function it holds.
struct Instance {
    row: &'static Program,
    function: Pci,
    child: Child,
    /// The job it runs in, kept while it runs: what holds its row's floor.
    _job: toyos::job::Job,
    /// The waker of a disk driver's [`toyos_quiesce::DRIVER_STOP`] bell:
    /// closing it tells the driver the machine stops.
    stop: Option<Waker>,
}

/// A service's kept acceptors, shared with what [`Grants`] mints on them.
/// Empty once its ports have closed for good.
type Ports = Rc<RefCell<Vec<(String, Acceptor)>>>;

/// One program the supervisor started at boot, and the ports it serves.
struct Service<'a> {
    program: &'a Program,
    /// The file-server role this process serves, for a row with `roles`.
    role: Option<&'a str>,
    /// What the running process was started from: the row's source, or a
    /// swap's installed binary.
    source: Source,
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
    /// The screen's row's desktop session's launcher and control, minted for
    /// its next start, which takes them.
    desktop: Option<sessions::Desktop>,
    /// The job every process serving it runs in, made at its first start and
    /// kept: its floor, where its row has one, outlives each process, so a
    /// start after an end is not refused what the row was held.
    job: Option<toyos::job::Job>,
    /// Since when a start after its end has waited for memory to come back.
    starved: Option<Instant>,
}

impl<'a> Service<'a> {
    fn new(program: &'a Program, role: Option<&'a str>, acceptors: Vec<(String, Acceptor)>, session: Session) -> Self {
        Self {
            program,
            role,
            source: program.source.clone(),
            child: None,
            devices: Vec::new(),
            ports: Rc::new(RefCell::new(acceptors)),
            swapping: false,
            session,
            budget: Budget::default(),
            desktop: None,
            job: None,
            starved: None,
        }
    }

    /// Start `source` holding this service's manifest row, and answer its pid.
    ///
    /// `owed` is what the process this start follows held: a start refused any
    /// of it is no start, so a service never runs on without a device it had.
    fn spawn(
        &mut self,
        source: &Source,
        owed: &[String],
        block: Vec<String>,
        system: &Manifest,
        syscap: &SysCap,
        connectors: &BTreeMap<&str, Connector>,
        grants: &Grants<'_>,
        minted: &Minted,
        boot: &Boot,
        log: &mut Log,
    ) -> Result<u32, StartError> {
        // **Checked again at every start, not only on arrival**: the installed
        // file lives in an ambient directory, so what the supervisor verified when it
        // wrote it is not a claim about what is there now. This narrows the
        // window to the spawn itself and does not close it
        // (`issues/a-swapped-binary-lives-where-any-process-can-rewrite-it.md`).
        if let Some((path, digest)) = source.path().and_then(|path| Some((path, toyos_swap::installed_digest(path)?))) {
            let bytes = read_binary(path).map_err(|why| std::io::Error::other(why.to_string()))?;
            toyos_swap::verify(&bytes, &digest).map_err(|why| {
                std::io::Error::other(format!("{path} no longer holds what was installed: {why}"))
            })?;
        }
        let mut storage = storage_endowment(self.role, syscap, grants, boot)?;
        if is_block(self.program) {
            storage.args.extend(block);
        }
        // The screen's row's desktop session, minted for this start alone.
        if let Some(desktop) = self.desktop.take() {
            storage.endow.push((LAUNCHER, desktop.launcher.into()));
            storage.endow.push((toyos_session::CONTROL_LABEL, desktop.control.into()));
        }
        let job = match &self.job {
            Some(job) => job,
            None => self.job.insert(job_for(syscap, self.program)?),
        };
        let endowed = job.duplicate().map_err(memory_refused)?;
        let ports = self.ports.borrow();
        let served = Served::Keep { acceptors: &ports, owed };
        let command = match source {
            Source::Path(path) => Command::new(path),
            Source::BootFileServer => {
                let mut command = Command::new(&self.program.name);
                command.boot_image(boot.fileserver.as_handle().into());
                command
            }
        };
        let (child, devices) = start(
            command,
            self.program,
            system,
            syscap,
            served,
            connectors,
            grants,
            &[],
            storage,
            Minting::machine(minted, self.program, self.session),
            Output::Boot { log, job: endowed },
        )?;
        drop(ports);
        let pid = child.id();
        self.child = Some(child);
        self.source = source.clone();
        self.devices = devices;
        Ok(pid)
    }

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// How many requests its job was refused for memory: a start that raised
    /// it waits for memory, and one refused anything else was refused that.
    fn refusals(&self) -> u64 {
        self.job.as_ref().and_then(|job| job.account().ok()).map_or(0, |account| account.refusals)
    }

    /// What its job's account says it would have held had its last refused
    /// request been granted, for a line that says why it waits.
    fn wanted(&self) -> String {
        match self.job.as_ref().map(toyos::job::Job::account) {
            Some(Ok(account)) => format!("{} KiB, to hold {} KiB in all", account.wanted.saturating_sub(account.committed + account.kernel) >> 10, account.wanted >> 10),
            Some(Err(e)) => format!("what its account would not say: {e:?}"),
            None => String::from("memory before it had a job"),
        }
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
    Probation { until: Instant, previous: Source, owed: Vec<String> },
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
    /// process it started. The `acpi` claim's holder goes first: it declares
    /// the memory the PCI root bridges decode, which every PCI claim minted
    /// after it waits for. Then the disk drivers and the storage rows, the
    /// block service before the file servers its grants are minted on — every
    /// other start may make a directory, and a directory is a file server's —
    /// and the session's home is made before the first row that is neither;
    /// the ACPI holder's own home with it, which it is started without.
    ///
    /// **Every service holds its acceptors before any is started**, so what a
    /// start mints on another service's port — a role's directories, a
    /// partition, `netstack` — does not depend on the order `[boot] start`
    /// lists them in. The `drives` processes start last, on what the
    /// device set says then.
    fn boot(&mut self, role_acceptors: &mut BTreeMap<&str, Acceptor>) {
        let system = self.system;
        let rank = |name: &&String| match system.program(name) {
            Some(p) if declares(p) => 0,
            Some(p) if is_block(p) => 1,
            Some(p) if is_storage(p) => 2,
            _ => 3,
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
        // The `acpi` claim's holder first, since every claim waits for its
        // declaration; then the disk drivers, so the block service is started
        // knowing how many to wait for and a file server's disk is coming
        // before it asks.
        // SYSTEM, started before the manifest was read, is first in the list.
        let (declaring, rest): (Vec<usize>, Vec<usize>) =
            (0..self.services.len()).partition(|&at| declares(self.services[at].program));
        let mut homes_made = false;
        for at in declaring {
            self.start_service(at, &mut homes_made);
        }
        self.undriven = self.drive_where(is_disk_driver);
        for at in rest {
            self.start_service(at, &mut homes_made);
        }
        if !homes_made {
            self.make_session_home();
            for early in self.services.iter().map(|s| s.program).filter(|p| declares(p)).collect::<Vec<_>>() {
                self.make_state_dir(early);
            }
        }
        self.load_grants();
        self.drive();
    }

    /// [`Self::boot`]'s start of service `at`, making the session's home before
    /// the first row that is neither storage nor the `acpi` claim's holder.
    fn start_service(&mut self, at: usize, homes_made: &mut bool) {
        let system = self.system;
        let program = self.services[at].program;
        if !is_storage(program) && !declares(program) && !*homes_made {
            self.make_session_home();
            *homes_made = true;
            for early in self.services.iter().map(|s| s.program).filter(|p| declares(p)).collect::<Vec<_>>() {
                self.make_state_dir(early);
            }
        }
        if !declares(program) {
            self.make_state_dir(program);
        }
        self.desktop_for(at);
        let block = self.block_args();
        let service = &mut self.services[at];
        // SYSTEM was started before the manifest was read.
        if service.child.is_some() {
            return;
        }
        let started = service.spawn(
            &program.source,
            &[],
            block,
            system,
            self.syscap,
            &self.connectors,
            &self.grants,
            &self.minted,
            self.boot,
            &mut self.log,
        );
        match started {
            Ok(_) => {}
            Err(StartError::Refused(why)) => {
                service.ports.borrow_mut().clear();
                service.job = None;
                say!("supervisor: {} did not start ({why}); its ports are closed this boot", service.label());
            }
            Err(e) => panic!("supervisor: cannot start {}: {e}", program.name),
        }
    }

    /// Read the device set, say what arrived and what left since the last
    /// reading, start a `drives` process on each function it says is owed
    /// one, and say which functions are given up on.
    fn drive(&mut self) {
        self.drive_where(|_| true);
    }

    /// [`Self::drive`], for the rows `which` picks; answers each function a
    /// start was refused on, by its entry and address.
    fn drive_where(&mut self, which: fn(&Program) -> bool) -> Vec<String> {
        let mut refused = Vec::new();
        let rows: Vec<(&'static Program, PciMatch)> = self.drivers.iter().filter(|(row, _)| which(row)).copied().collect();
        let read = self.devices.read(|n| vec![toyos_abi::inventory::RawRecord::EMPTY; n], |set| {
            (set.functions().collect::<Vec<Pci>>(), set.header.settle_in)
        });
        let records = match read {
            Ok((records, settle_in)) => {
                self.settle = settle_in.map(|nanos| Instant::now() + Duration::from_nanos(nanos));
                records
            }
            Err(why) => {
                say!("supervisor: {why}, so no `drives` process starts on it");
                return refused;
            }
        };
        for pci in records.iter().filter(|pci| !self.listed.iter().any(|seen| seen.arrival == pci.arrival)) {
            if !self.listed.is_empty() {
                say!("supervisor: PCI {} [{:04x}:{:04x}] joined the device set as arrival {}", pci_at(pci.at), pci.vendor, pci.device, pci.arrival.0);
            }
        }
        for gone in self.listed.iter().filter(|seen| !records.iter().any(|pci| pci.arrival == seen.arrival)) {
            say!("supervisor: PCI {} (arrival {}) left the device set", pci_at(gone.at), gone.arrival.0);
        }
        self.listed = records;
        for act in self.drives.decide(&rows, &self.listed, boot_controller(), Instant::now()) {
            match act {
                Act::Start { row, entry, function } => match self.start_instance(row, entry, function) {
                    Ok((child, stop, job)) => self.instances.push(Instance { row, function, child, stop, _job: job }),
                    Err(why) => {
                        say!("supervisor: {} was not started on PCI {}: {why}", row.name, pci_at(function.at));
                        refused.push(format!("{entry} at PCI {}", pci_at(function.at)));
                        self.drives.refused(function.arrival);
                    }
                },
                Act::GiveUp { row, function } => say!(
                    "supervisor: {} was started at PCI {} {} times in {} s and is given up on there",
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
    /// its badge, which it waits for; every disk driver's function the boot
    /// started none on; and every partition the loader named, which the
    /// running system is on and no designation changes.
    fn block_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for instance in self.instances.iter().filter(|i| is_disk_driver(i.row)) {
            args.extend(["--driver".to_string(), instance.function.arrival.0.to_string()]);
        }
        for undriven in &self.undriven {
            args.extend(["--undriven".to_string(), undriven.clone()]);
        }
        match inventory(self.syscap) {
            Ok(records) => {
                let named = records.iter().filter_map(|r| match r {
                    Record::Loaded(l) => Some(l.unique_guid),
                    _ => None,
                });
                for guid in named {
                    args.extend(["--active".to_string(), guid_text(guid)]);
                }
            }
            Err(why) => {
                say!("supervisor: the inventory was not read ({why}), so the block service designates nothing");
                args.push("--active-unknown".to_string());
            }
        }
        args
    }

    /// One `drives` process of `row`, holding `function`'s claim under
    /// `entry`'s spelling.
    fn start_instance(&mut self, row: &'static Program, entry: PciMatch, function: Pci) -> Result<(Child, Option<Waker>, toyos::job::Job), String> {
        if self.services.len() + self.instances.len() >= MAX_CHILDREN {
            return Err(format!("the supervisor watches {MAX_CHILDREN} processes already"));
        }
        let name = entry.to_string();
        let claim = self.syscap.claim_function::<toyos::Device>(function.arrival).map_err(|e| refused(&name, e))?;
        let claim = translated(claim, &function).map_err(|why| missing(&name, &why))?;
        self.make_state_dir(row);
        let mut extra = Extra { claims: vec![(format!("{DEV_PREFIX}{name}"), claim)], ..Extra::default() };
        // A disk driver is who this start says it is: its `disk` connector
        // carries the function's arrival, which the block service keys it by.
        // And told when the machine stops, which it waits for (`stop_drivers`).
        let mut stop = None;
        if is_disk_driver(row) {
            extra.ports.push((toyos_blockring::disk::PORT.to_string(), self.grants.disk(function.arrival)?));
            let (waker, bell) = wake::pair().map_err(|e| format!("no event for its stop: {e:?}"))?;
            extra.stop = Some(bell);
            stop = Some(waker);
        }
        let launcher = Minting::machine(&self.minted, row, self.sessions.machine());
        let served = Served::Keep { acceptors: &[], owed: &[] };
        let path = row.source.path().ok_or_else(|| format!("{} runs {}, and a `drives` row runs a file", row.name, row.source))?;
        let job = job_for(self.syscap, row).map_err(|e| e.to_string())?;
        let endowed = job.duplicate().map_err(|e| format!("its job would not duplicate: {e:?}"))?;
        let output = Output::Boot { log: &mut self.log, job: endowed };
        start(Command::new(path), row, self.system, self.syscap, served, &self.connectors, &self.grants, &[], extra, launcher, output)
            .map(|(child, _)| (child, stop, job))
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
                    let row = installed(system, &name)?;
                    binary_digest(row.source.path().ok_or_else(|| format!("{name} runs {}", row.source))?)
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

    /// A row's own directory of machine state, a service's `HOME` among them,
    /// made before it first runs.
    fn make_state_dir(&mut self, program: &Program) {
        let Some(dir) = program.state_dir().filter(|_| !is_storage(program)) else { return };
        let asked = dir.clone();
        match self.files("a row's state directory", move || make_dir(&asked)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => say!("supervisor: {}: {dir} could not be made: {e}", program.name),
            Err(why) => say!("supervisor: {}: {dir} was not made: {why}", program.name),
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
    /// goes on waiting for alone, the worker still waiting on an earlier
    /// one, or a storage service waiting for memory to start again: no call
    /// may be answered until it has, and the loop answers the controls
    /// meanwhile.
    fn files<T: Send + 'static>(&mut self, what: &str, work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
        const DONE: u64 = 0;
        self.worker.settle();
        if self.worker.owed > 0 {
            return Err(format!("the supervisor's file worker still waits on {}", self.worker.owed_what));
        }
        self.storage_fed()?;
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
            self.storage_fed()?;
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
        self.say_floors();
        let poller = Poller::new(Poller::MAX_HANDLES);
        let mut pending: Vec<Pending> = Vec::new();
        let mut flight: Option<Flight> = None;
        let mut parked: Option<Parked> = None;
        let mut ready: Vec<u64> = Vec::new();
        loop {
            poller.watch(&self.minted.launcher, READABLE, TOKEN_ACCEPTOR);
            poller.watch(&self.minted.grants, READABLE, TOKEN_GRANTS_ACCEPTOR);
            poller.watch(&self.minted.login, READABLE, TOKEN_LOGIN_ACCEPTOR);
            poller.watch(&self.minted.admin, READABLE, TOKEN_ADMIN_ACCEPTOR);
            poller.watch(&self.watch.ended, READABLE, TOKEN_APPS);
            for login in &self.logins.open {
                poller.watch(&login.link.handle(), READABLE, TOKEN_LINKS);
            }
            poller.watch(swap, READABLE, TOKEN_SWAP_ACCEPTOR);
            poller.watch(power, READABLE, TOKEN_POWER_ACCEPTOR);
            poller.watch(&self.devices, READABLE, TOKEN_DEVICES);
            for watcher in &self.watchers {
                let writable = if watcher.owes() { WRITABLE } else { 0 };
                poller.watch(&watcher.conn, READABLE | writable, TOKEN_POWER_WATCHERS);
            }
            self.watch_children(&poller);
            for service in self.services.iter().filter(|service| service.starved.is_some()) {
                let job = service.job.as_ref().expect("supervisor: a service waiting for memory has its job");
                poller.watch(job, READABLE, TOKEN_MEMORY);
            }
            if let Some(Flight { phase: Phase::Answered { conn, .. }, .. }) = &flight {
                poller.watch(conn, READABLE, TOKEN_HANGUP);
            }
            if let Some(asked) = &parked {
                poller.watch(&asked.launch.conn, READABLE, TOKEN_PARKED_CALLER);
                poller.watch(&asked.consent, READABLE, TOKEN_CONSENT);
            }
            for p in &pending {
                poller.watch(&p.conn, READABLE, TOKEN_PENDING_BASE + p.conn.as_handle().raw().0 as u64);
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
                .chain(self.settle.map(|at| at.saturating_duration_since(now)))
                .min()
                .map_or(u64::MAX, |left| left.as_nanos() as u64);
            ready.clear();
            poller.wait(1, timeout, |token| ready.push(token));
            let open = self.logins.open.len();

            // Accept and the request are two events. Nothing is read here.
            for (token, acceptor, port) in [
                (
                    TOKEN_ACCEPTOR,
                    &self.minted.launcher,
                    (|s, c| s.caller(&s.minted.launcher, c).map(Port::Launcher)) as fn(&Self, &Connection) -> _,
                ),
                (TOKEN_GRANTS_ACCEPTOR, &self.minted.grants, |s, c| s.caller(&s.minted.grants, c).map(Port::Grants)),
                (TOKEN_LOGIN_ACCEPTOR, &self.minted.login, |s, c| s.opener(c).map(Port::Login)),
                (TOKEN_ADMIN_ACCEPTOR, &self.minted.admin, |s, c| s.caller(&s.minted.admin, c).map(Port::Admin)),
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
                        conn.as_handle().raw().0
                    );
                    continue;
                }
                let port = match port(self, &conn) {
                    Ok(port) => port,
                    Err(why) => {
                        say!("supervisor: launcher: dropping client {} — {why}", conn.as_handle().raw().0);
                        continue;
                    }
                };
                pending.push(Pending { conn, rx: LaunchRx::new(), since: Instant::now(), port });
            }

            // `remove` rather than `swap_remove`: the entries after `i` shift
            // down, so leaving `i` alone visits each connection exactly once.
            let mut i = 0;
            while i < pending.len() {
                let handle = pending[i].conn.as_handle().raw();
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
                    RxStep::Frame { msg_type, payload_len, .. } => {
                        let mut p = pending.remove(i);
                        match p.port {
                            Port::Launcher(caller) => {
                                let payload = p.rx.payload(payload_len).to_vec();
                                let carried = p.rx.take_batch();
                                self.serve_launch(p.conn, &caller, msg_type, &payload, carried, &mut parked)
                            }
                            Port::Grants(caller) => self.serve_grants(&p.conn, &caller, msg_type, p.rx.payload(payload_len)),
                            Port::Login(opener) => self.serve_login(&p.conn, opener, msg_type),
                            Port::Admin(caller) => self.serve_admin(&p.conn, &caller, msg_type, p.rx.payload(payload_len)),
                            Port::Power => {
                                let payload = p.rx.payload(payload_len).to_vec();
                                self.serve_power(p.conn, msg_type, &payload)
                            }
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
                    p.conn.as_handle().raw().0
                );
            }
            pending.retain(|p| now.duration_since(p.since) < HANDSHAKE_TIMEOUT);

            flight = flight.and_then(|f| self.advance(f, ready.contains(&TOKEN_HANGUP)));
            let (caller_ready, consent_ready) = (ready.contains(&TOKEN_PARKED_CALLER), ready.contains(&TOKEN_CONSENT));
            parked = parked.and_then(|asked| self.advance_parked(asked, caller_ready, consent_ready));

            if ready.contains(&TOKEN_LINKS) {
                self.pump_links(&mut pending);
            }
            if ready.contains(&TOKEN_APPS) {
                self.apps_ended();
            }
            if ready.contains(&TOKEN_MEMORY) {
                self.memory_returned();
            }
            if ready.contains(&TOKEN_ENDED) {
                self.ended();
            }
            if ready.contains(&TOKEN_POWER_WATCHERS) {
                self.pump_watchers();
            }
            if ready.contains(&TOKEN_LINKS) || self.logins.open.len() != open {
                self.sessions_changed();
            }
            if ready.contains(&TOKEN_DEVICES) || self.settle.is_some_and(|at| at <= Instant::now()) {
                self.drive();
            }
        }
    }

    /// The `login` row the badge of a connection on [`Minted::login`] names.
    fn opener(&self, conn: &Connection) -> Result<&'static Program, String> {
        let mut badge = [0u8; MAX_BADGE];
        let badge = self.minted.login.badge(conn, &mut badge).map_err(|e| format!("its connection has no badge ({e:?})"))?;
        let row = std::str::from_utf8(badge).map_err(|_| format!("its badge is not one this supervisor writes: {badge:?}"))?;
        self.system.program(row).filter(|p| p.login).ok_or_else(|| format!("its badge names `{row}`, which is no `login` row"))
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
        // Not SYSTEM, which no manifest row declares: it runs the signed boot
        // image whatever is installed.
        let Some(index) = self.services.iter().position(|s| s.program.name == name && s.role != Some(toyos_manifest::SYSTEM)) else {
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
        if service.source.path() == Some(installed.as_str()) {
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
                service.source,
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
        let previous = self.services[index].source.clone();
        // What the process being stopped holds, which its replacement and the
        // binary a failed swap starts again are each owed.
        let owed = self.services[index].devices.clone();
        let program = self.services[index].program;
        self.make_state_dir(program);
        self.desktop_for(index);
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
            &Source::Path(path.clone()),
            &owed,
            block,
            self.system,
            self.syscap,
            &self.connectors,
            &self.grants,
            &self.minted,
            self.boot,
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
    fn end_probation(&mut self, index: usize, path: &str, previous: &Source, owed: &[String]) {
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
                if let Some(previous) = previous.path().filter(|&previous| previous != path) {
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

    /// Tell every disk driver the machine stops, after the sync that wrote
    /// through it, and wait for each to end, within
    /// [`toyos_quiesce::DRIVERS_MS`]: usbd hands its devices back with a port
    /// reset and leaves its controller reset. One that does not end in time is
    /// named, and the machine stops without it: its function is stopped by
    /// the kernel's own stop, as one whose driver died is. Answers whether any
    /// was told.
    fn stop_drivers(&mut self) -> bool {
        let until = Instant::now() + Duration::from_millis(toyos_quiesce::DRIVERS_MS);
        let mut waiting: Vec<Arrival> = Vec::new();
        for instance in &mut self.instances {
            // The waker's closing is the word.
            if instance.stop.take().is_some() {
                waiting.push(instance.function.arrival);
            }
        }
        let told = !waiting.is_empty();
        let poller = Poller::new(waiting.len().max(1) as u32);
        loop {
            let mut at = 0;
            while at < self.instances.len() {
                let instance = &self.instances[at];
                let arrival = instance.function.arrival;
                match waiting.contains(&arrival).then(|| exit_of(&instance.child)).flatten() {
                    Some(exit) => {
                        let Instance { row, function, .. } = self.instances.swap_remove(at);
                        say!("supervisor: power: {} on PCI {} ended for the stop with {exit}", row.name, pci_at(function.at));
                        self.drives.ended(arrival, true);
                        waiting.retain(|a| *a != arrival);
                    }
                    None => at += 1,
                }
            }
            let left = until.saturating_duration_since(Instant::now());
            if waiting.is_empty() || left.is_zero() {
                break;
            }
            for instance in self.instances.iter().filter(|i| waiting.contains(&i.function.arrival)) {
                poller.watch(&instance.child, READABLE, TOKEN_ENDED);
            }
            poller.wait(1, left.as_nanos() as u64, |_| {});
        }
        for instance in self.instances.iter().filter(|i| waiting.contains(&i.function.arrival)) {
            say!(
                "supervisor: power: {} on PCI {} did not end in {} ms of being told; the machine stops without it",
                instance.row.name,
                pci_at(instance.function.at),
                toyos_quiesce::DRIVERS_MS
            );
        }
        told
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
        for child in services.chain(self.instances.iter().map(|instance| &instance.child)).chain(self.owners()) {
            poller.watch(&child, READABLE, TOKEN_ENDED);
        }
    }

    /// Act on every end since the last: a service is started again on the
    /// ports it kept, or has them closed and its clients answered `Gone`
    /// ([`restart::after_end`]); a `drives` process's function is owed
    /// another or left alone ([`Drives::ended`]); and the device set is read
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
                AfterEnd::Close => {
                    service.ports.borrow_mut().clear();
                    service.job = None;
                }
                AfterEnd::GiveUp => {
                    service.ports.borrow_mut().clear();
                    service.job = None;
                    say!(
                        "supervisor: {label} ended {} times in {} s; its ports are closed, its clients answered Gone, and a program started from now on is given none of its directories",
                        restart::RESTARTS + 1,
                        restart::WINDOW.as_secs()
                    );
                }
                AfterEnd::Restart => match self.start_again(index) {
                    Ok(new) => say!("supervisor: {label} (pid {pid}) ended; started again as pid {new}"),
                    Err((e, true)) => {
                        let wanted = self.services[index].wanted();
                        self.services[index].starved = Some(Instant::now());
                        say!(
                            "supervisor: {label} (pid {pid}) ended and could not start again for want of memory ({e}; it lacked {wanted}); \
                             it starts again once memory comes back, and its clients wait on its ports meanwhile"
                        );
                    }
                    Err((e, false)) => {
                        self.close_for_good(index);
                        say!("supervisor: {label} ended and would not start again ({e}{}); its ports are closed", unrecorded(&e));
                    }
                },
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
            say!("supervisor: {} on PCI {} ended, {exit}", row.name, pci_at(function.at));
            self.drives.ended(function.arrival, restart::owed(row, exit));
        }
        if any {
            self.drive();
        }
        self.sessions_ended();
        self.restarting = false;
    }

    /// What each floor job holds of its floor once the boot is done: the margin
    /// a floor's size is judged by.
    fn say_floors(&self) {
        let own = toyos::job::own().and_then(|job| job.account());
        let services = self.services.iter().filter_map(|s| Some((s.label(), s.job.as_ref()?.account().ok()?)));
        for (name, account) in own.ok().map(|a| ("the supervisor".to_string(), a)).into_iter().chain(services) {
            if account.floor > 0 {
                say!("supervisor: {name} holds {} KiB of its {} KiB floor, at most {} KiB so far", (account.committed + account.kernel) >> 10, account.floor >> 10, account.peak >> 10);
            }
        }
    }

    /// Service `index` serves no more: its ports close, and its job goes, and
    /// with it any floor its row was held.
    fn close_for_good(&mut self, index: usize) {
        let service = &mut self.services[index];
        service.ports.borrow_mut().clear();
        service.starved = None;
        service.job = None;
    }

    /// `Err` naming a storage service that waits for memory to start again.
    fn storage_fed(&self) -> Result<(), String> {
        match self.services.iter().find(|s| s.starved.is_some() && is_storage(s.program)) {
            Some(starved) => Err(format!("{} waits for memory to start again", starved.label())),
            None => Ok(()),
        }
    }

    /// Start service `index` again on the ports and devices it kept, from
    /// what it last ran. A refusal says whether its job was refused memory
    /// meanwhile, which is what a wait for memory to come back can answer.
    fn start_again(&mut self, index: usize) -> Result<u32, (StartError, bool)> {
        let before = self.services[index].refusals();
        let started = self.start_from(index);
        started.map_err(|e| (e, self.services[index].refusals() > before))
    }

    fn start_from(&mut self, index: usize) -> Result<u32, StartError> {
        let service = &self.services[index];
        let (source, owed, program) = (service.source.clone(), service.devices.clone(), service.program);
        self.make_state_dir(program);
        self.desktop_for(index);
        let block = self.block_args();
        let service = &mut self.services[index];
        let (system, syscap, connectors, grants) = (self.system, self.syscap, &self.connectors, &self.grants);
        service.spawn(&source, &owed, block, system, syscap, connectors, grants, &self.minted, self.boot, &mut self.log)
    }

    /// Memory came back that a service waiting to start again lacked: each
    /// is tried again, and one still short waits for the next time, said
    /// with what it lacks now.
    fn memory_returned(&mut self) {
        for index in 0..self.services.len() {
            let Some(since) = self.services[index].starved else { continue };
            let label = self.services[index].label();
            let waited = since.elapsed().as_millis();
            match self.start_again(index) {
                Ok(pid) => {
                    self.services[index].starved = None;
                    say!("supervisor: {label} started again as pid {pid} once memory came back, {waited} ms after its start was refused");
                }
                Err((e, true)) => {
                    let wanted = self.services[index].wanted();
                    say!("supervisor: {label} is still short of memory after {waited} ms ({e}; it lacks {wanted}); it waits on");
                }
                Err((e, false)) => {
                    self.close_for_good(index);
                    say!("supervisor: {label} would not start again once memory came back ({e}{}); its ports are closed", unrecorded(&e));
                }
            }
        }
    }

    /// Start the binary a failed swap replaced, or close the service's ports
    /// when that will not start either.
    fn restore(&mut self, index: usize, previous: &Source, owed: &[String]) {
        let program = self.services[index].program;
        self.make_state_dir(program);
        self.desktop_for(index);
        let block = self.block_args();
        let service = &mut self.services[index];
        let name = service.program.name.clone();
        match service.spawn(previous, owed, block, self.system, self.syscap, &self.connectors, &self.grants, &self.minted, self.boot, &mut self.log) {
            Ok(pid) => {
                service.swapping = false;
                swapped(&name, Word::Restored, &format!("{previous} as pid {pid}"));
            }
            Err(e) => {
                service.swapping = false;
                service.ports.borrow_mut().clear();
                service.job = None;
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
        carried: ipc::Batch,
        parked: &mut Option<Parked>,
    ) {
        if msg_type != launch::MSG_LAUNCH {
            return;
        }
        // Owned from the frame on, so every path out of here releases them: a
        // launcher that leaked a handle per refused launch would exhaust the
        // one table the machine cannot do without, and a client picks which
        // refusal it takes.
        let received = carried.len();
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

        // Past every refusal that does not know which handle is which: the
        // descriptors, the extras, the place if it names one, and the job.
        let mut carried = carried;
        let fds: Vec<toyos::OwnedHandle> = carried.by_ref().take(request.fd_count()).collect();
        // `SYS_NAMESPACE_BUILD` copies a connector into the namespace and
        // leaves the caller's handle, and the supervisor's copy of a client's
        // connector has no life beyond this launch. **Not a claim about the
        // type** — a client sends what it likes, and everything below treats a
        // wrong one as a refused launch rather than as the supervisor's own bug.
        let extras: Vec<(String, Connector)> =
            names.into_iter().zip(carried.by_ref()).map(|(name, handle)| (name.to_string(), Connector::from_handle(handle))).collect();
        let mut rest: Vec<toyos::OwnedHandle> = carried.collect();
        let job = toyos::job::Job::from_handle(rest.pop().expect("a launch's batch ends with its job"));
        let place = rest.pop();

        // std joins a relative `current_dir` onto the supervisor's own cwd, so passing one
        // on would start the child in the supervisor's `/` — the default this field removes.
        if !request.cwd.starts_with('/') {
            say!("supervisor: launcher: refused a working directory that is not absolute");
            let _ = conn.try_signal(launch::MSG_REFUSED);
            return;
        }

        // **A launch names its parent**: the place it carries, spawned under, or
        // detached, under the supervisor. One that names neither is refused rather than
        // detached, which would let it outlive a caller that never asked it to.
        let under = match request.parent() {
            Some(Parent::Place(())) => place.as_ref().map(toyos::AsHandle::as_handle),
            // Under the session's owner, or in the machine's session the supervisor.
            Some(Parent::Detached) => match self.logins.detached_place(caller.session) {
                Ok(place) => place,
                Err(why) => {
                    say!("supervisor: launcher: refused a detached launch: {why}");
                    let _ = conn.try_signal(launch::MSG_REFUSED);
                    return;
                }
            },
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
        let placed = under.is_some();
        if let Some(place) = under {
            command.under(place.into());
        }
        drop(place);
        for arg in request.argv.split(|&b| b == 0).skip(1).filter(|a| !a.is_empty()) {
            if let Ok(arg) = std::str::from_utf8(arg) {
                command.arg(arg);
            }
        }

        // A descriptor naming the caller's own log ring is replaced by the
        // program's, which is made for the job it joins.
        let fds: Option<Vec<(u32, LaunchFd)>> = request
            .fds()
            .zip(fds)
            .map(|((fd, kind), handle)| {
                let caller_ring = matches!(fd, 1 | 2)
                    && kind == Some(toyos::fd::Kind::Handle)
                    && toyos_abi::syscall::fstat(handle.as_handle().raw())
                        .is_ok_and(|stat| stat.file_type == FileType::SharedMemory);
                Some((fd, if caller_ring { LaunchFd::Ring } else { LaunchFd::Caller(handle, kind?) }))
            })
            .collect();
        let Some(fds) = fds else {
            say!("supervisor: launcher: refused a descriptor of a kind no launch carries");
            let _ = conn.try_signal(launch::MSG_REFUSED);
            return;
        };
        let ring = match fds.iter().any(|(_, given)| matches!(given, LaunchFd::Ring)) {
            true => match ring_for(job.as_handle()) {
                Ok(ring) => Some(ring),
                Err(e) => {
                    say!("supervisor: launcher: {} was not started: no ring for its log: {e}", request.program);
                    let _ = conn.try_signal(launch::MSG_REFUSED);
                    return;
                }
            },
            false => None,
        };

        // Endowed before `prepare` reads the program, so its image is read for the
        // job the child runs in, which keeps it as long as the child runs.
        command.endow(JOB_LABEL, job.into());

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
                authority::may_start(row, session, target)?;
                let (Target::Row(program) | Target::Package(program)) = target;
                let prepared = match program.source.path() {
                    Some(path) => command.image_from(Path::new(path)).prepare().map(drop),
                    None => Err(std::io::Error::other(format!("{} runs {}, which no launch starts", program.name, program.source))),
                };
                let binary = matches!(target, Target::Package(_)).then(|| {
                    command
                        .prepared_image()
                        .map(|bytes| toyos_swap::hex(&toyos_swap::digest(bytes)))
                        .ok_or_else(|| format!("{} was not read before its start, so no grant can name it", program.source))
                });
                Ok((binary, prepared.map(|()| command)))
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
            // **`try_signal` and not `signal`.** A blocking write is the other
            // half of the rule that made the read side an event loop: a client
            // that never drains its end decides when the supervisor runs again.
            Resolved::NotDeclared => {
                say!("supervisor: launcher: no row declares {}", request.program);
                let _ = conn.try_signal(launch::MSG_NOT_PERMITTED);
                return;
            }
            Resolved::Refused(why) => {
                say!("supervisor: launcher: {why}");
                let _ = conn.try_signal(launch::MSG_NOT_PERMITTED);
                return;
            }
        };
        let (binary, prepared) = match verdict {
            Ok(judged) => judged,
            Err(why) => {
                say!("supervisor: launcher: {} in {} may not start {}: {why}", caller.row.name, caller.session, program.name);
                let _ = conn.try_signal(launch::MSG_NOT_PERMITTED);
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
        let session = caller.session;
        let launch = Launch {
            conn,
            program,
            command,
            session,
            fds,
            ring,
            placed,
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
        let Launch { conn, mut program, mut command, session, fds, ring, placed, extras, cwd } = launch;
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
        // `fds` when this returns.
        self.make_state_dir(program);
        if program.package().is_some() {
            if let Err(why) = self.make_app_home(program) {
                say!("supervisor: launcher: {} was not started: {why}", program.name);
                let _ = conn.try_signal(launch::MSG_REFUSED);
                return;
            }
        }
        let storage = self.slot_storage(program);
        let launches = match session {
            Session::Login(..) => match self.logins.launcher_of(session) {
                Some(launcher) => Some((launcher, session)),
                None => {
                    say!("supervisor: launcher: {} was not started: its session has ended", program.name);
                    let _ = conn.try_signal(launch::MSG_REFUSED);
                    return;
                }
            },
            Session::Machine(_) => Minting::machine(&self.minted, program, session).launches,
        };
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
            Minting { minted: &self.minted, session, launches },
            Output::Launch { log: &mut self.log, fds: &fds, ring },
        );
        drop(fds);
        match started {
            Ok((child, _)) => {
                self.started_in(session, &program.name, toyos::AsHandle::as_handle(&child));
                let handle = toyos::OwnedHandle::from(std::os::toyos::io::OwnedHandle::from(child));
                // Consumed either way, so the supervisor keeps no `Process` handle from a
                // launch, or a client launching `/system/bin/true` in a loop exhausts the
                // one table the machine cannot do without.
                let _ = conn.try_signal_with(launch::MSG_LAUNCHED, [handle]);
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
    /// The descriptors its caller sent, copied into the program.
    fds: Vec<(u32, LaunchFd)>,
    /// The program's own log ring, where its caller sent its own.
    ring: Option<SharedMemory>,
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
        RxStep::Frame { msg_type, payload_len, .. } => consent::Heard::Frame(msg_type, rx.payload(payload_len)),
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
    let row = |p: &str| system.program(&key(p)).filter(|program| program.source.path() == Some(p));
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
    /// Nothing in the image declares it.
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
fn resolve<'a, V>(system: &'a Manifest, path: &str, judge: impl FnOnce(Target<'_>) -> V) -> Resolved<'a, V> {
    if !toyos::launch::is_canonical(path) {
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
    if row.source.path() != Some(path) {
        return Resolved::Refused(format!("{} launches {} and this asks for {path:?}", Package::path(name), row.source));
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
    use toyos_blockdev::{BlockDev as _, BLOCK};
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
    let disk = diskserver::disk::Served::new(session);
    let mut copies = [[0; BLOCK]; toyos_update::slots::COPIES as usize];
    toyos_blockdev::block_on(disk.read(0, &mut copies), || disk.wait())
        .map_err(|why| format!("the slot table would not read: {why:?}"))?;
    let (table, _) = toyos_update::slots::current([&copies[0], &copies[1]])
        .map_err(|why| format!("the slot table's partition holds {why}"))?;
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
        Missing::Untranslated(None) => format!(
            "{name} is a virtio function that offers no VIRTIO_F_ACCESS_PLATFORM, so no unit translates its DMA \
             (virtio 1.2 §6.1): refused"
        ),
        Missing::Untranslated(Some(why)) => format!(
            "{name} is a virtio function whose offer was not read ({why}), so whether a unit translates its DMA is \
             unknown: refused"
        ),
    }
}

/// `claim` on `function`, refused where `function` is a virtio function whose
/// DMA no unit confines: a holder is never handed one.
fn translated(claim: toyos::Device, function: &Pci) -> Result<toyos::Device, Missing> {
    if !claims::is_virtio(function) {
        return Ok(claim);
    }
    let dev = toyos::PciDev::from_handle(claim.into());
    let offered = toyos_pci_claim::virtio::offers_access_platform(&dev);
    let claim = toyos::Device::from_handle(dev.into());
    match offered {
        Ok(true) => Ok(claim),
        Ok(false) => Err(Missing::Untranslated(None)),
        Err(why) => Err(Missing::Untranslated(Some(why.to_string()))),
    }
}

/// A function's address as the kernel's `pcidev:` lines spell it.
fn pci_at(at: PciAddr) -> String {
    format!("{:02x}:{:02x}.{}", at.bus, at.dev, at.func)
}

/// How `child` ended, once it has ([`restart::exited`]).
fn exit_of(child: &Child) -> Option<toyos_abi::end::End> {
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
    minting: Minting<'_>,
    output: Output<'_>,
) -> Result<(Child, Vec<String>), StartError> {
    let Extra { args, claims, root, ports, stop, endow } = extra;
    // A storage row's own arguments first: a file server's role leads its argv.
    command.args(&args);
    command.args(&program.args);
    let booting = matches!(output, Output::Boot { .. });

    // **Set here, over whatever a launching caller carried**: the row decides
    // where a program's home is, and a service's is made before this
    // (`Supervisor::make_state_dir`), on the supervisor's file worker.
    command.env("HOME", program.home());

    // Everything endowed is the command's: a refused spawn closes it, so an
    // acceptor the supervisor must keep for a later start is endowed as a copy.
    for (label, claim) in claims {
        command.endow(&label, claim.into());
    }
    if let Some(stop) = stop {
        command.endow(toyos_quiesce::DRIVER_STOP, stop.into());
    }
    if let Some(root) = root {
        command.endow(ROOT_IMAGE_LABEL, root.into());
    }
    for (label, handle) in endow {
        command.endow(label, handle.into());
    }
    let mut view = grants.view(program, minting.session);
    view.extend(ports);
    view.extend(grants.authorizations(program).map_err(StartError::Refused)?);
    view.extend(grants.netstack(program, minting.session));
    if let Some(ns) = build_namespace(program, system, connectors, view, extras)? {
        command.endow(SVC_LABEL, ns.into());
    }
    if let Some(ns) = swap_namespace(program, connectors) {
        command.endow(toyos_swap::LABEL, ns.into());
    }
    if let Some(ns) = minting.launches.and_then(|launches| launcher_namespace(program, launches)) {
        command.endow(LAUNCHER, ns.into());
    }
    let minted = minting.minted;
    let badged = [
        (grants::PORT, program.receives.iter().any(|name| name == grants::PORT), &minted.grants),
        (toyos_session::ADMIN_PORT, program.receives.iter().any(|name| name == toyos_session::ADMIN_PORT), &minted.admin),
    ];
    for (name, _, acceptor) in badged.into_iter().filter(|(_, receives, _)| *receives) {
        command.endow(name, badged_namespace(program, name, acceptor, minting.session).into());
    }
    if program.login {
        command.endow(toyos_session::LOGIN_PORT, login_namespace(program, &minted.login).into());
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
        command.endow(&format!("{PROVIDE_PREFIX}{name}"), handed.into());
    }

    // A `serves` port whose acceptor is gone can never be served again, so
    // the child is endowed copies, and a launch's leave the map only once it
    // runs.
    let owed: &[String] = match &served {
        Served::Keep { owed, .. } => owed,
        Served::Move(_) => &[],
    };
    let moved = match served {
        Served::Move(acceptors) => {
            for name in &program.serves {
                // A `serves` program started this way is started exactly once
                // per boot. A second start with no acceptor left is refused by
                // name rather than spawned with a hole where its own service
                // should be.
                let acceptor = acceptors.get(name.as_str()).unwrap_or_else(|| {
                    panic!("supervisor: `{}` has already been given the `{name}` acceptor", program.name)
                });
                endow_served(&mut command, name, acceptor)?;
            }
            Some(acceptors)
        }
        Served::Keep { acceptors, .. } => {
            // Every acceptor the service keeps: its row's `serves`, or the
            // directories of a file server's role. None left is a service
            // whose ports closed for good.
            if acceptors.is_empty() && (!program.serves.is_empty() || !program.roles.is_empty()) {
                return Err(std::io::Error::other("its ports are closed for good").into());
            }
            for (name, acceptor) in acceptors {
                endow_served(&mut command, name, acceptor)?;
            }
            None
        }
    };

    let mut endowed: Vec<String> = Vec::new();
    // Read at the first PCI matcher, and once.
    let mut functions: Option<Vec<Pci>> = None;
    for name in &program.devices {
        // The build refuses to render an entry that names no device, so a
        // failure here is an image whose halves disagree.
        let entry = Entry::parse(name).unwrap_or_else(|| panic!("supervisor: `{name}` names no device"));
        let (request, function) = match entry {
            Entry::Fixed(request) => (Ok(request), None),
            Entry::Pci(matcher) => {
                let records = match &mut functions {
                    Some(records) => records,
                    None => functions.insert(pci_records(syscap).map_err(StartError::Refused)?),
                };
                let found = claims::function(&matcher, records, boot_controller());
                let function = found.as_ref().ok().and_then(|at| records.iter().find(|pci| pci.arrival == *at)).copied();
                (found.map(DeviceRequest::Function).map_err(Missing::Unresolved), function)
            }
        };
        let minted = request.and_then(|request| claim(syscap, request).map_err(Missing::Refused)).and_then(|minted| {
            match &function {
                Some(function) => translated(minted, function),
                None => Ok(minted),
            }
        });
        // The label is the manifest's own spelling, which is exactly what the
        // claimant looks the claim up by: one string, written once.
        let why = match minted {
            Ok(claim) => {
                command.endow(&format!("{DEV_PREFIX}{name}"), claim.into());
                endowed.push(name.clone());
                continue;
            }
            Err(why) => why,
        };
        // Nothing was spawned, so everything minted closes with the command.
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

    // The signed image's row alone starts a pager: the right stays here, and
    // the child holds only what it was started as.
    if program.pager {
        command.pager(syscap.as_handle().into());
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
        command.endow(SYSCAP_LABEL, narrowed.into());
    }

    // Where the program's output goes.
    //
    // A service the supervisor starts gets a ring of its own as its stdout and stderr
    // and the supervisor's console to read as its stdin; `logkeeper` also gets the acceptor
    // the rings reach it on and the one console handle that may write. A
    // launch keeps the descriptors its caller sent, but for a ring among them
    // — the caller's own log — which is replaced by the program's
    // ([`LaunchFd::Ring`]), and a stdio descriptor it sent nothing for is a
    // null, never this process's own.
    let mut starts_logkeeper = false;
    let (log, ring) = match output {
        Output::Boot { log, job } => {
            // **Every service the supervisor starts is a job of its own**,
            // which its caller made ([`job_for`]).
            let ring = ring_for(job.as_handle())?;
            command.endow(JOB_LABEL, job.into());
            command.inherit_handle(0, log.stdin.as_handle().into());
            command.inherit_handle(1, ring.as_handle().into());
            command.inherit_handle(2, ring.as_handle().into());
            if program.name == LOGKEEPER {
                let Some(acceptor) = &log.acceptor else {
                    panic!("supervisor: `{LOGKEEPER}` is started twice, and the log's origins are the first's");
                };
                let origins = acceptor
                    .as_handle()
                    .try_clone_to_owned()
                    .map_err(|e| std::io::Error::other(format!("the log's origins did not duplicate: {e:?}")))?;
                command.endow(ORIGINS, origins.into());
                let console = toyos::fd::with(0, |o| o.handle().map(|console| console.try_clone_to_owned()))
                    .ok()
                    .flatten()
                    .expect("supervisor: its console is descriptor 0")
                    .expect("supervisor: its console would not duplicate for logkeeper");
                command.endow(CONSOLE, console.into());
                starts_logkeeper = true;
            }
            (log, Some(ring))
        }
        Output::Launch { log, fds, ring } => {
            command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            for (fd, given) in fds {
                match given {
                    LaunchFd::Ring => {
                        let ring = ring.as_ref().expect("a launch naming its caller's ring has the program's");
                        command.inherit_handle(*fd, ring.as_handle().into());
                    }
                    LaunchFd::Caller(handle, toyos::fd::Kind::Handle) => {
                        command.inherit_handle(*fd, handle.as_handle().into());
                    }
                    LaunchFd::Caller(handle, toyos::fd::Kind::Exported) => {
                        command.inherit_exported(*fd, handle.as_handle().into());
                    }
                }
            }
            (log, ring)
        }
    };
    // The ring's end is its writer's: an event whose one waker the program
    // holds under a label no child inherits.
    let mut alive = None;
    if ring.is_some() {
        let (waker, bell) = wake::pair()
            .map_err(|e| std::io::Error::other(format!("no event for its log's end: {e:?}")))?;
        command.endow(ALIVE, waker.into());
        alive = Some(bell);
    }

    let child = command.spawn()?;
    if let Some(acceptors) = moved {
        for name in &program.serves {
            acceptors.remove(name.as_str());
        }
    }
    if starts_logkeeper {
        log.acceptor = None;
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

/// Endow a copy of `acceptor`, the `name` port's, which the supervisor keeps
/// until the start succeeds.
fn endow_served(command: &mut Command, name: &str, acceptor: &Acceptor) -> Result<(), StartError> {
    let handed = acceptor
        .as_handle()
        .try_clone_to_owned()
        .map_err(|e| std::io::Error::other(format!("the `{name}` acceptor did not duplicate: {e:?}")))?;
    command.endow(&format!("{SERVE_PREFIX}{name}"), handed.into());
    Ok(())
}

/// Where a started program's output goes: a service's own ring, made for the
/// job it joins, or a launch's caller's descriptors with any ring among them
/// replaced by the program's.
enum Output<'l> {
    Boot { log: &'l mut Log, job: toyos::job::Job },
    Launch { log: &'l mut Log, fds: &'l [(u32, LaunchFd)], ring: Option<SharedMemory> },
}

/// A launched program's descriptor.
enum LaunchFd {
    /// A handle its caller sent, a copy of which the program gets.
    Caller(toyos::OwnedHandle, toyos::fd::Kind),
    /// Its caller's own log ring, for which the program gets its own.
    Ring,
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
    // [`badged_namespace`] say why. Never the block port's own connector,
    // which reaches nothing: a holder of `block` holds one minted for what it
    // reaches, in `view`, and a disk driver one of `disk` minted for who it
    // is; nor netstack's, which nothing holds: a holder of `netstack` holds
    // one minted for its row and session, in `view` too.
    let receives: Vec<&String> = program
        .receives
        .iter()
        .filter(|name| {
            ![toyos_swap::PORT, grants::PORT, toyos_session::ADMIN_PORT, toyos_blockring::PORT, toyos_blockring::disk::PORT, NETSTACK]
                .contains(&name.as_str())
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

    /// What `program`'s row is authorized for on the machine's disks
    /// (`toyos_manifest::authorize`), each a connector to the block service
    /// minted with that one authorization's badge, under its name. The block
    /// service resolves and judges each when it is used, so nothing here
    /// waits for a disk.
    fn authorizations(&self, program: &Program) -> Result<Vec<(String, Connector)>, String> {
        use toyos_manifest::authorize::Authorization;
        let mut minted = Vec::new();
        for word in &program.authorize {
            let authorization = Authorization::parse(word).unwrap_or_else(|| panic!("supervisor: `{word}` is no authorization the build admits"));
            let badge = match authorization {
                Authorization::Tables => toyos_blockring::wire::Badge::Tables,
                Authorization::DesignateData => toyos_blockring::wire::Badge::DesignateData,
            };
            let Some(ports) = &self.block else { return Err(format!("`{word}` is authorized and no block service runs")) };
            let ports = ports.borrow();
            let Some((_, acceptor)) = ports.iter().find(|(name, _)| name == toyos_blockring::PORT) else {
                return Err("the block service's port has closed for good".into());
            };
            let connector = acceptor.mint(&badge.encode()).map_err(|e| format!("no `{word}` connector could be minted: {e:?}"))?;
            say!("supervisor: {} is authorized for `{word}`", program.name);
            minted.push((authorization.name().to_string(), connector));
        }
        Ok(minted)
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

/// A `launcher` minted with `program`'s row and the session its launches run
/// in, on that session's launcher port, in a namespace of its own, for a row
/// whose `starts` lists anything; endowed under [`LAUNCHER`] and never an
/// entry of `svc`, for [`swap_namespace`]'s reason.
fn launcher_namespace(program: &Program, (launcher, session): (&Acceptor, Session)) -> Option<Namespace> {
    if program.starts.is_empty() {
        return None;
    }
    Some(badged_namespace(program, LAUNCHER, launcher, session))
}

/// `name`, minted on `acceptor` with `program`'s row and `session`, in a
/// namespace of its own: endowed under that name and never an entry of `svc`,
/// for [`swap_namespace`]'s reason. The badge is who asks: the supervisor
/// honours a login session's alone on [`grants::PORT`] and
/// [`toyos_session::ADMIN_PORT`].
fn badged_namespace(program: &Program, name: &str, acceptor: &Acceptor, session: Session) -> Namespace {
    let badge = Authority { row: program.name.clone(), session }.encode();
    let connector = acceptor
        .mint(&badge)
        .unwrap_or_else(|e| panic!("supervisor: no `{name}` connector for {}: {e:?}", program.name));
    namespace::build()
        .add(name, &connector)
        .finish()
        .unwrap_or_else(|e| panic!("supervisor: no `{name}` namespace for {}: {e:?}", program.name))
}

/// [`toyos_session::LOGIN_PORT`] minted with a `login` row's key, the login
/// authority whose sessions it opens.
fn login_namespace(program: &Program, login: &Acceptor) -> Namespace {
    let connector = login
        .mint(program.name.as_bytes())
        .unwrap_or_else(|e| panic!("supervisor: no `login` connector for {}: {e:?}", program.name));
    namespace::build()
        .add(toyos_session::LOGIN_PORT, &connector)
        .finish()
        .unwrap_or_else(|e| panic!("supervisor: no `login` namespace for {}: {e:?}", program.name))
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
    /// SYSTEM's ROOT image, a duplicate.
    root: Option<toyos::OwnedHandle>,
    ports: Vec<(String, Connector)>,
    /// The bell of a disk driver's [`toyos_quiesce::DRIVER_STOP`].
    stop: Option<Bell>,
    /// Handles endowed by label.
    endow: Vec<(&'static str, toyos::OwnedHandle)>,
}

/// The controller a `@boot` entry names, read once at start
/// ([`toyos_supervisor::boot`]).
static BOOT_CONTROLLER: std::sync::OnceLock<Option<PciAddr>> = std::sync::OnceLock::new();

fn boot_controller() -> Option<PciAddr> {
    *BOOT_CONTROLLER.get().expect("supervisor: the boot sources are read before any start")
}

/// Every record the kernel's inventory answers.
fn inventory(syscap: &SysCap) -> Result<Vec<toyos_abi::inventory::Record>, String> {
    syscap
        .records(|n| vec![toyos_abi::inventory::RawRecord::EMPTY; n])
        .map_err(|why| why.to_string())
}

/// Every PCI function in the kernel's device set, read through a handle of
/// its own: a start reads it where the loop's handle is not to be marked read.
fn pci_records(syscap: &SysCap) -> Result<Vec<Pci>, String> {
    let set = syscap.device_set().map_err(|e| format!("the device set was not minted: {e:?}"))?;
    set.read(|n| vec![toyos_abi::inventory::RawRecord::EMPTY; n], |set| set.functions().collect()).map_err(|why| why.to_string())
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

/// A file server's arguments and the volume it is endowed for one start.
///
/// A file server is told its role. SYSTEM is handed ROOT's image; every other
/// role `block` minted with its grant (`toyos_supervisor::storage`), and LOG
/// and BOOT the GUID of the partition the loader named. A log or boot role the
/// loader named no partition for, and a block port closed for good, each
/// refuse the start: the role is then absent, never served from memory.
fn storage_endowment(role: Option<&str>, syscap: &SysCap, grants: &Grants<'_>, boot: &Boot) -> Result<Extra, StartError> {
    let mut storage = Extra::default();
    let Some(role) = role else {
        return Ok(storage);
    };
    storage.args.push(role.to_string());
    if role == toyos_manifest::SYSTEM {
        let root = boot.root.try_clone()
            .map_err(|e| std::io::Error::other(format!("ROOT's image did not duplicate: {e:?}")))?;
        storage.root = Some(root);
        return Ok(storage);
    }
    let records = inventory(syscap).map_err(|why| StartError::Other(std::io::Error::other(why)))?;
    let served = toyos_supervisor::storage::grant(role, |which| loaded(&records, which)).map_err(StartError::Refused)?;
    if let Scope::Unique(guid) = served.scope {
        storage.args.push(guid_text(guid));
    }
    if let Some(port) = grants.partitions(served).map_err(StartError::Refused)? {
        storage.ports.push((toyos_blockring::PORT.to_string(), port));
    }
    Ok(storage)
}

/// Say which build this machine runs: the first file the supervisor reads,
/// so a boot that dies past here names its build in its log.
fn say_release(server: &Service<'_>) {
    let path = toyos_osrelease::GUEST_PATH;
    let text = read_system(path, server);
    let release = toyos_osrelease::parse(text.as_bytes())
        .unwrap_or_else(|e| bootstrap_fails(format!("{path} is not one the build wrote: {e:?}")));
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

/// The file `path` of `/system`, read before the loop exists to start SYSTEM
/// again: a server that ended or will not answer within [`FILES_BOUND`] is
/// this boot's end, said by name.
fn read_system(path: &'static str, server: &Service<'_>) -> String {
    let (answer, answered) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("bootstrap".into())
        .spawn(move || {
            // The receiver is gone once the bound has passed.
            let _ = answer.send(std::fs::read_to_string(path));
        })
        .expect("supervisor: no thread to read /system on");
    let read = answered.recv_timeout(FILES_BOUND);
    // Answered, so it is on its way out.
    if read.is_ok() {
        reader.join().expect("supervisor: the bootstrap reader panicked");
    }
    match read {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => bootstrap_fails(format!("cannot read {path}: {e}")),
        Err(_) => bootstrap_fails(format!(
            "{path} was not read in {} s; {} {}",
            FILES_BOUND.as_secs(),
            server.label(),
            match server.child.as_ref().map(exit_of) {
                Some(Some(exit)) => format!("ended, {exit}"),
                _ => "is running and did not answer".to_string(),
            }
        )),
    }
}

/// The boot's end before `logkeeper` runs, which no ring reaches yet: said on
/// the console the kernel gave the supervisor, then a panic like any other.
fn bootstrap_fails(why: String) -> ! {
    let line = format!("supervisor: {why}\n");
    let _ = toyos::fd::with(0, |console| console.handle().map(|console| toyos_abi::syscall::write(console.raw(), line.as_bytes())));
    panic!("supervisor: {why}");
}

/// The images the kernel hands the supervisor beside the system capability:
/// the file server's executable and ROOT's bytes, which nobody writes
/// (`toyos_abi::boot::BootImage`). Kept for the machine's life, since SYSTEM
/// is started again from them.
///
/// Handles and not mappings: the supervisor reads neither, it hands them on.
struct Boot {
    fileserver: toyos::OwnedHandle,
    root: toyos::OwnedHandle,
}

impl Boot {
    fn take() -> Self {
        let image = |label: &str| {
            let handle: toyos::OwnedHandle = Endowments::get()
                .take(label)
                .unwrap_or_else(|| panic!("supervisor: the kernel spawns this program holding `{label}`"));
            let kind = toyos_abi::syscall::fstat(handle.as_handle().raw()).map(|stat| stat.file_type);
            assert_eq!(kind, Ok(FileType::ImmutableMemory), "supervisor: `{label}` is no image nobody writes");
            handle
        };
        Self { fileserver: image(FILESERVER_IMAGE_LABEL), root: image(ROOT_IMAGE_LABEL) }
    }
}

/// The row SYSTEM is started under, which no manifest declares since it
/// serves the manifest: the file server's boot image, serving ROOT, started
/// again when it ends, and holding nothing but its role's port and ROOT.
fn system_row() -> Program {
    Program {
        name: "fileserver".to_string(),
        source: Source::BootFileServer,
        roles: vec![toyos_manifest::SYSTEM.to_string()],
        restart: true,
        ..Program::default()
    }
}

/// What a refusal no memory was recorded for is, beside its own words: a
/// `ResourceExhausted` the job's account did not count is a limit, or the
/// supervisor's own memory, and waiting for memory would not end it.
fn unrecorded(e: &StartError) -> &'static str {
    match e {
        StartError::Other(e) if e.kind() == std::io::ErrorKind::OutOfMemory => {
            "; its job was refused no memory, so it is a limit or the supervisor's own refusal"
        }
        _ => "",
    }
}

/// A job of its own for a service or driver `row` starts: made with the system
/// capability's `JOBS`, and held its row's floor, which only a signed image's
/// `system.toml` declares. A floor refused is a start refused: a control path
/// started without the memory it is owed would answer until memory ran out
/// and then not.
fn job_for(syscap: &SysCap, row: &Program) -> std::io::Result<toyos::job::Job> {
    let job = syscap.new_job().map_err(memory_refused)?;
    if row.memory_floor > 0 {
        syscap.hold_floor(&job, row.memory_floor).map_err(|e| match e {
            SyscallError::ResourceExhausted => std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                format!("its floor of {} MiB cannot be held", row.memory_floor >> 20),
            ),
            other => std::io::Error::other(format!("its floor would not be held: {other:?}")),
        })?;
    }
    Ok(job)
}
