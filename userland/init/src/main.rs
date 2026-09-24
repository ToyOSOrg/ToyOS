//! The one program the kernel starts, and the only holder of the machine's
//! system capability.
//!
//! Everything else is started from here, holding exactly what
//! `/system/etc/system.manifest` says it holds. **Every port exists before any server
//! runs**: init creates one per `serves` name in the whole manifest, then
//! builds each program's namespace out of the connectors and spawns it with the
//! acceptor moved in. So a client's connection works from its first
//! instruction whether or not the server has reached `accept` or has even been
//! spawned, there is no instant at which a name is not bound yet, and there is
//! nothing anywhere to retry.
//!
//! **A service init starts at boot outlives any one process serving it.** init
//! keeps each of its acceptors and endows a duplicate, so a swap
//! ([`toyos_swap`]) can stop the process, start another binary holding the same
//! manifest row, and leave every client's connector naming the same port — a
//! connect made in the gap waits in that port's queue for the new process. A
//! service that ends with no swap owning it has its kept acceptors closed at
//! once, so a client's next connect is `ServerGone` exactly as it is for a
//! server nothing keeps. A program started through `launcher` still gets its
//! acceptor by move and is started once per boot.

/// One line, one `write`.
///
/// **`eprintln!` is not one write.** Stderr is unbuffered by design, so
/// `write_fmt` issues a syscall per format fragment, and on this machine the
/// console and the kernel's log ring are one stream — so a daemon's own line
/// lands inside init's. `netd: ready, at most ` and `init: started test-runner`
/// arrived interleaved and the harness parsed a cap out of the wrong number.
/// `userland/soundd` has the same macro for the same reason.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let mut line = format!($($arg)*);
        line.push('\n');
        let _ = std::io::stderr().write_all(line.as_bytes());
    }};
}

use std::collections::BTreeMap;
use std::os::toyos::process::{ChildExt, CommandExt};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use toyos_swap::{Refusal, Request as SwapRequest, Word};

use toyos_manifest::package::{self, Package};
use toyos_manifest::{Manifest, Program};
use toyos::endow::Endowments;
use toyos::ipc::{self, Connection, RxStep};
use toyos::launch::{self, Request};
use toyos::namespace::{self, Namespace};
use toyos::poller::{Poller, READABLE};
use toyos::port::{self, Acceptor, Connector};
use toyos::syscap::SysCap;
use toyos::AsHandle;
use toyos_abi::syscall::{
    DeviceRequest, SyscallError, DEV_PREFIX, PROVIDE_PREFIX, SERVE_PREFIX, SVC_LABEL,
    SYSCAP_LABEL,
};

/// The service init answers on. Its own, so it has no `[programs]` row and the
/// manifest carries it as an `init-serve` record.
const LAUNCHER: &str = "launcher";

/// Connections accepted and not yet carrying a whole launch.
///
/// **The bound is init's handle table, not memory.** A connection nobody has
/// spoken on costs a `PendingConnection` and no ring page, so what this stops a
/// client from doing is filling the one table the machine cannot do without.
/// Thirty-two is the kernel's own per-port queue depth
/// (`MAX_PENDING_CONNECTIONS`), which is the allowance one step earlier on the
/// same path — past it init refuses by name rather than growing.
const MAX_PENDING_LAUNCHES: usize = 32;

/// How long an accepted connection may go without completing its launch.
///
/// Policy, and generous: every caller sends its frame in the statement after
/// `connect` (`toyos::launch::launch`). What this bounds is the one that never
/// sends it, and it is what guarantees the table above drains.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// One caller's inbound framing.
///
/// **init never reads a client with a blocking read.** `recv_header` and
/// `recv_bytes` park the caller until the peer sends the bytes it promised, so
/// a client that connected and said nothing used to park init — and init is the
/// machine's only way to start a process. The whole request blob is kept
/// because a launch carries the child's argv, environment and working
/// directory, and a truncated one is a launch refused for a reason the caller
/// did not cause.
type LaunchRx = ipc::FrameRx<{ ipc::MAX_FRAME_LEN as usize }>;

/// A connection that has been accepted and has not yet said what to start.
///
/// It exists because accept and the request frame are two events, and init used
/// to fuse them with a blocking `recv_header` on the fresh connection.
struct Pending {
    conn: Connection,
    rx: LaunchRx,
    since: Instant,
    /// Which of init's two ports it came in on, which decides what its frame
    /// may ask for.
    port: Port,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Port {
    Launcher,
    Swap,
}

/// The poll tokens for the `launcher` and `swap` acceptors, and for the one
/// connection whose swap has been answered and whose hang-up is awaited. A
/// pending connection's token is [`TOKEN_PENDING_BASE`] plus its handle, which
/// is unique among the connections init holds at once.
const TOKEN_ACCEPTOR: u64 = 0;
const TOKEN_SWAP_ACCEPTOR: u64 = 1;
const TOKEN_HANGUP: u64 = 2;
const TOKEN_PENDING_BASE: u64 = 3;

fn main() {
    let syscap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("init: the kernel spawns this program holding the system capability");

    let text = std::fs::read_to_string(toyos_manifest::GUEST_PATH)
        .unwrap_or_else(|e| panic!("init: cannot read {}: {e}", toyos_manifest::GUEST_PATH));
    let system = toyos_manifest::parse(&text);

    // Before anything is spawned, and for every `serves` name in the manifest
    // rather than only the ones `[boot] start` names: the filepicker is
    // launched by the compositor, and an editor holding its connector must be
    // able to ask for a file before the picker has run an instruction.
    let mut acceptors = BTreeMap::new();
    let mut connectors: BTreeMap<&str, Connector> = BTreeMap::new();
    let names = system
        .served_names()
        .into_iter()
        .chain(system.init_serves.iter().map(String::as_str));
    for name in names {
        let (acceptor, connector) =
            port::create().unwrap_or_else(|e| panic!("init: no port for `{name}`: {e:?}"));
        acceptors.insert(name, acceptor);
        connectors.insert(name, connector);
    }

    // Kept for the machine's life: init is the only thing that can kill a
    // daemon, and there is no other way back to a process it started.
    let mut services: Vec<Service> = Vec::new();
    for name in &system.start {
        let program = system
            .program(name)
            .unwrap_or_else(|| panic!("init: [boot] start names `{name}`, which is not declared"));
        let kept = program
            .serves
            .iter()
            .map(|served| {
                let acceptor = acceptors.remove(served.as_str()).unwrap_or_else(|| {
                    panic!("init: `{}` has already been given the `{served}` acceptor", program.name)
                });
                (served.clone(), acceptor)
            })
            .collect();
        let mut service = Service::new(program, kept);
        if let Err(e) = service.spawn(&program.path, &[], &system, &syscap, &connectors) {
            panic!("init: cannot start {}: {e}", program.name);
        }
        services.push(service);
    }

    // Nothing else holds a `serves` acceptor that has not been launched yet, so
    // init outliving its children is what keeps those ports open. It parks
    // here.
    let launcher = acceptors
        .remove(LAUNCHER)
        .expect("init: the manifest declares init serves `launcher`");
    let swap = acceptors
        .remove(toyos_swap::PORT)
        .expect("init: the manifest declares init serves `swap`");
    let mut init = Init { system: &system, syscap: &syscap, acceptors, connectors, services };
    init.serve_forever(&launcher, &swap);
}

/// Everything init's loop acts on, for the machine's life.
struct Init<'a> {
    system: &'a Manifest,
    syscap: &'a SysCap,
    /// The `serves` acceptors nobody has been started with yet, which a launch
    /// takes by move.
    acceptors: BTreeMap<&'a str, Acceptor>,
    connectors: BTreeMap<&'a str, Connector>,
    /// What `[boot] start` named, in that order.
    services: Vec<Service<'a>>,
}

/// One program init started at boot, and the ports it serves.
struct Service<'a> {
    program: &'a Program,
    /// The binary the running process was started from: the row's path, or a
    /// swap's installed one.
    path: String,
    /// `None` once neither binary would start.
    child: Option<Child>,
    /// The row's devices the running process was endowed: what a process
    /// started in its place is owed.
    devices: Vec<String>,
    kept: Arc<Mutex<Kept>>,
}

/// What a service's waiter thread shares with the loop.
struct Kept {
    /// The acceptors of every name the service serves. Empty once the service
    /// has ended with no swap owning it: the port is then closed for good.
    acceptors: Vec<(String, Acceptor)>,
    /// Counts the service's starts, so a waiter answers only for its own.
    generation: u64,
    /// A swap owns the service: its process ending is expected and the port
    /// stays open for the next one.
    swapping: bool,
}

impl<'a> Service<'a> {
    fn new(program: &'a Program, acceptors: Vec<(String, Acceptor)>) -> Self {
        Self {
            program,
            path: program.path.clone(),
            child: None,
            devices: Vec::new(),
            kept: Arc::new(Mutex::new(Kept { acceptors, generation: 0, swapping: false })),
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
    ) -> std::io::Result<u32> {
        // **Checked again at every start, not only on arrival**: the installed
        // file lives in an ambient directory, so what init verified when it
        // wrote it is not a claim about what is there now. This narrows the
        // window to the spawn itself and does not close it
        // (`issues/isolation/a-swapped-binary-lives-where-any-process-can-rewrite-it.md`).
        if let Some(digest) = toyos_swap::installed_digest(path) {
            let bytes = read_binary(path).map_err(|why| std::io::Error::other(why.to_string()))?;
            toyos_swap::verify(&bytes, &digest).map_err(|why| {
                std::io::Error::other(format!("{path} no longer holds what was installed: {why}"))
            })?;
        }
        let mut kept = self.kept.lock().expect("init: a service's state is poisoned");
        // Every start after the first follows a process of this service
        // that init stopped or saw end.
        let served = match kept.generation {
            0 => Served::Keep(&kept.acceptors),
            _ => Served::Restart { acceptors: &kept.acceptors, owed },
        };
        let (child, devices) =
            start(Command::new(path), self.program, system, syscap, served, connectors, &[])?;
        kept.generation += 1;
        if !kept.acceptors.is_empty() {
            let process = toyos_abi::syscall::dup(toyos::RawHandle(child.as_raw_handle()))
                .unwrap_or_else(|e| panic!("init: {}'s process handle: {e:?}", self.program.name));
            let (shared, generation) = (Arc::clone(&self.kept), kept.generation);
            std::thread::Builder::new()
                .name(format!("wait-{}", self.program.name))
                .spawn(move || close_when_it_ends(&shared, generation, process))
                .expect("init: a service's waiter could not be started");
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
}

/// Wait for one start of a service to end, and close its ports if nothing was
/// expecting it to.
///
/// **A thread, because the kernel answers a process's end to a wait and to
/// nothing a poll can watch.** It parks in the kernel for the process's life
/// and costs nothing until then.
fn close_when_it_ends(kept: &Mutex<Kept>, generation: u64, process: toyos::RawHandle) {
    let _ = toyos_abi::syscall::process_wait(process);
    toyos_abi::syscall::close(process);
    let mut kept = kept.lock().expect("init: a service's state is poisoned");
    if kept.generation == generation && !kept.swapping {
        kept.acceptors.clear();
    }
}

/// A swap init has answered and not finished.
struct Flight {
    /// Index into [`Init::services`].
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

impl<'a> Init<'a> {
    /// Serve `launcher` and `swap` for the rest of the machine's life.
    ///
    /// **An event loop and not an accept loop, because the rule every other
    /// server in this tree obeys binds init hardest.** A server never blocks on
    /// a client: accept and the first frame are two events, a frame is buffered
    /// until whole before anything acts on it, and a reply is one non-blocking
    /// write. init used to read the fresh connection with `recv_header`, so any
    /// process holding a `launcher` connector — the compositor, every terminal,
    /// every shell, sshd, the one thing reachable from the network — could
    /// connect, say nothing, and take the machine's only way to create a
    /// process with two syscalls, leaving init alive and looking healthy.
    ///
    /// What a swap waits on is the loop's too: the requester's hang-up is an
    /// event, and the ends of the hang-up bound and of probation are deadlines
    /// the wait wakes for. The one wait init makes outside the loop is for a
    /// service it has just killed to finish ending, which is the kernel's
    /// teardown and no client's.
    fn serve_forever(&mut self, launcher: &Acceptor, swap: &Acceptor) -> ! {
        let poller = Poller::new(3 + MAX_PENDING_LAUNCHES as u32);
        let mut pending: Vec<Pending> = Vec::new();
        let mut flight: Option<Flight> = None;
        let mut ready: Vec<u64> = Vec::new();
        loop {
            poller.watch(launcher, READABLE, TOKEN_ACCEPTOR);
            poller.watch(swap, READABLE, TOKEN_SWAP_ACCEPTOR);
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

            let now = Instant::now();
            for p in pending.iter().filter(|p| now.duration_since(p.since) >= HANDSHAKE_TIMEOUT) {
                say!(
                    "init: launcher: dropping client {} — it never finished its launch",
                    p.conn.as_handle().0
                );
            }
            pending.retain(|p| now.duration_since(p.since) < HANDSHAKE_TIMEOUT);

            // Accept and the request are two events. Nothing is read here.
            for (token, acceptor, port) in [
                (TOKEN_ACCEPTOR, launcher, Port::Launcher),
                (TOKEN_SWAP_ACCEPTOR, swap, Port::Swap),
            ] {
                if !ready.contains(&token) {
                    continue;
                }
                let conn = match acceptor.accept() {
                    Ok(conn) => conn,
                    Err(e) => panic!("init: an acceptor of init's own refused: {e:?}"),
                };
                if pending.len() >= MAX_PENDING_LAUNCHES {
                    say!(
                        "init: launcher: refusing client {} — {MAX_PENDING_LAUNCHES} connections \
                         are already waiting to say what they want",
                        conn.as_handle().0
                    );
                } else {
                    pending.push(Pending { conn, rx: LaunchRx::new(), since: Instant::now(), port });
                }
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
                            "init: launcher: dropping client {} — it sent a frame this protocol \
                             cannot describe",
                            handle.0
                        );
                        pending.remove(i);
                    }
                    RxStep::Frame { msg_type, payload_len } => {
                        let p = pending.remove(i);
                        match p.port {
                            Port::Launcher => serve_launch(
                                &p.conn,
                                msg_type,
                                p.rx.payload(payload_len),
                                self.system,
                                self.syscap,
                                &mut self.acceptors,
                                &self.connectors,
                            ),
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

            flight = flight.and_then(|f| self.advance(f, ready.contains(&TOKEN_HANGUP)));
        }
    }

    /// One swap request, answered: refused with the old service untouched, or
    /// verified, installed and accepted.
    ///
    /// **Everything in the request is sshd's claim about what its client
    /// sent**, and the bytes are read here and held against the digest here:
    /// what init starts is what init verified, and nothing else's word for it.
    fn accept_swap(
        &self,
        conn: Connection,
        msg_type: u32,
        payload: &[u8],
        flight: Option<&Flight>,
    ) -> Option<Flight> {
        let refuse = |service: &str, why: Refusal| {
            say!("{}", toyos_swap::said(service, Word::Refused, &why.to_string()));
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
        say!(
            "{}",
            toyos_swap::said(
                name,
                Word::Accepted,
                &format!(
                    "{installed} ({} bytes, sha256 {}) replaces {} (pid {})",
                    bytes.len(),
                    toyos_swap::hex(&request.digest),
                    service.path,
                    service.pid().map_or(0, |p| p)
                ),
            )
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
        let service = &mut self.services[index];
        say!(
            "{}",
            toyos_swap::said(
                &name,
                Word::Stopping,
                &format!("pid {} ({previous})", service.pid().map_or(0, |p| p)),
            )
        );
        service.kept.lock().expect("init: a service's state is poisoned").swapping = true;
        if let Some(mut old) = service.child.take() {
            // Killed and then waited for, because the kernel publishes a
            // process's end only after its handle table is closed: once the
            // wait returns, every device claim it held is back and can be
            // minted again.
            let _ = old.kill();
            let _ = old.wait();
        }
        let started = service.spawn(&path, &owed, self.system, self.syscap, &self.connectors);
        match started {
            Ok(pid) => {
                say!(
                    "{}",
                    toyos_swap::said(
                        &name,
                        Word::Started,
                        &format!(
                            "{path} as pid {pid}; in service if it runs {} ms",
                            toyos_swap::PROBATION_MS
                        ),
                    )
                );
                let until = Instant::now() + Duration::from_millis(toyos_swap::PROBATION_MS);
                Some(Flight { service: index, path, phase: Phase::Probation { until, previous, owed } })
            }
            Err(e) => {
                say!("{}", toyos_swap::said(&name, Word::Failed, &format!("{path} did not start: {e}")));
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
            let mut kept = service.kept.lock().expect("init: a service's state is poisoned");
            let status = service.child.as_mut().map(|c| c.try_wait());
            if matches!(status, Some(Ok(None))) {
                kept.swapping = false;
            }
            status
        };
        match status {
            Some(Ok(None)) => {
                say!(
                    "{}",
                    toyos_swap::said(
                        &name,
                        Word::InService,
                        &format!("{path} as pid {}", service.pid().map_or(0, |p| p)),
                    )
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
                say!(
                    "{}",
                    toyos_swap::said(
                        &name,
                        Word::Failed,
                        &format!("{path} {how} inside {} ms", toyos_swap::PROBATION_MS),
                    )
                );
                service.child = None;
                forget(path);
                self.restore(index, previous, owed);
            }
        }
    }

    /// Start the binary a failed swap replaced, or close the service's ports
    /// when that will not start either.
    fn restore(&mut self, index: usize, previous: &str, owed: &[String]) {
        let service = &mut self.services[index];
        let name = service.program.name.clone();
        match service.spawn(previous, owed, self.system, self.syscap, &self.connectors) {
            Ok(pid) => {
                service.kept.lock().expect("init: a service's state is poisoned").swapping = false;
                say!("{}", toyos_swap::said(&name, Word::Restored, &format!("{previous} as pid {pid}")));
            }
            Err(e) => {
                let mut kept = service.kept.lock().expect("init: a service's state is poisoned");
                kept.swapping = false;
                kept.acceptors.clear();
                say!(
                    "{}",
                    toyos_swap::said(
                        &name,
                        Word::Gone,
                        &format!("{previous} did not start either ({e}); its ports are closed"),
                    )
                );
            }
        }
    }
}

/// A staged or installed binary's bytes, refused past [`toyos_swap::MAX_BINARY_BYTES`]
/// before any of it is read.
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

/// One `MSG_LAUNCH`, from the frame to the `Process` handle that answers it.
///
/// **Everything in the request is a client's claim about itself.** A program
/// nothing declares is refused by name; a frame that does not decode is a
/// dropped connection and nothing else. What the child ends up holding is the
/// manifest's row for it plus whatever connectors the caller transferred, and
/// the caller could only transfer what it already had — so a launch confers
/// exactly the manifest row and nothing beyond it.
fn serve_launch<'a>(
    conn: &Connection,
    msg_type: u32,
    payload: &[u8],
    system: &'a Manifest,
    syscap: &SysCap,
    acceptors: &mut BTreeMap<&'a str, Acceptor>,
    connectors: &BTreeMap<&str, Connector>,
) {
    if msg_type != launch::MSG_LAUNCH {
        return;
    }
    let mut batch = [toyos::RawHandle(0); toyos_abi::syscall::MAX_TRANSFER_HANDLES];
    let received = conn.recv_handles(&mut batch).unwrap_or(0);

    // **Owned on the statement after they arrive, and before anything can
    // refuse.** The send moved them into init's table, so every path out of
    // here releases them — a launcher that leaked a handle per refused launch
    // would exhaust the one table the machine cannot do without, and a client
    // picks which refusal it takes.
    let mut held = Moved(batch[..received].to_vec());

    let Some(request) = Request::decode(payload) else { return };
    // `extra_names` drops an empty or non-UTF-8 name, so its count is what
    // will actually be paired with a handle. A frame whose two counts
    // disagree would otherwise leave the unpaired handles behind.
    let names: Vec<&str> = request.extra_names().collect();
    if received != request.slot_count() + request.extra_count
        || names.len() != request.extra_count
    {
        say!(
            "init: launcher: a frame promising {} handles under {} names carried {received}",
            request.slot_count() + request.extra_count,
            names.len(),
        );
        return;
    }

    // Past every refusal that does not know which handle is which, so ownership
    // can be split. Both halves still release on every path below.
    let all = held.take();
    let (slot_handles, extra_handles) = all.split_at(request.slot_count());
    let slots = Moved(slot_handles.to_vec());
    // Owned, so they close when this call returns: `SYS_NAMESPACE_BUILD` copies
    // a connector into the namespace and leaves the caller's handle, and init's
    // copy of a client's connector has no life beyond this launch.
    let extras: Vec<(&str, Connector)> = names
        .into_iter()
        .zip(extra_handles.iter().copied())
        // SAFETY: the kernel moved these into init's table with the frame, and
        // nothing else answers for them. **Not a claim about the type** — a
        // client sends what it likes, and everything below treats a wrong one
        // as a refused launch rather than as init's own bug.
        .map(|(name, handle)| (name, unsafe { Connector::from_raw(handle) }))
        .collect();

    let installed;
    let program = match resolve(system, request.program) {
        Resolved::Row(row) => row,
        Resolved::Package(row) => {
            installed = row;
            &installed
        }
        Resolved::NotDeclared => {
            // **`try_signal` and not `send`.** A bare header is what every
            // answer here is, and a blocking write is the other half of the
            // rule that made the read side an event loop: a client that never
            // drains its end decides when init runs again.
            let _ = conn.try_signal(launch::MSG_NOT_DECLARED);
            return;
        }
        Resolved::Refused(why) => {
            say!("init: launcher: {why}");
            let _ = conn.try_signal(launch::MSG_REFUSED);
            return;
        }
    };

    // **The caller's path, not the row's, and `argv[0]` is why.** `declared`
    // has already established that the two name one binary, so this grants
    // nothing extra — and `/system/bin/echo` spawned as `/system/bin/toybox` is a toybox that
    // was never told which applet it is.
    let mut command = Command::new(request.program);
    for (slot, handle) in request.slot_numbers().zip(slots.0.iter().copied()) {
        command.inherit_handle(slot, handle.0);
    }
    // **Carried, not inherited.** A child of the launcher would otherwise get
    // init's environment and init's working directory, so `cd /tmp && ls` would
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
    if !request.cwd.is_empty() {
        command.current_dir(request.cwd);
    }
    for arg in request.argv.split(|&b| b == 0).skip(1).filter(|a| !a.is_empty()) {
        if let Ok(arg) = std::str::from_utf8(arg) {
            command.arg(arg);
        }
    }

    // `inherit_handle` duplicates into the child, so init's own copies go with
    // `slots` when this returns.
    let started =
        start(command, program, system, syscap, Served::Move(acceptors), connectors, &extras);
    match started {
        Ok((child, _)) => {
            let handle = toyos::RawHandle(child.into_raw_handle());
            // **Which side owns the handle is the whole of what the two arms
            // differ by.** A refused `handle_send` leaves it in init's table
            // and init must close it — init keeps no `Process` handle from a
            // launch, or a client launching `/system/bin/true` in a loop exhausts the
            // one table the machine cannot do without. A send that *took* it
            // and a frame that then did not go leaves it queued on a connection
            // this call is about to drop, which releases it — and closing it
            // here would be closing a handle init no longer holds, which under
            // the bad-handle policy is init exiting.
            match toyos_abi::syscall::handle_send(conn.as_handle(), &[handle]) {
                Ok(()) => {
                    let _ = conn.try_signal(launch::MSG_LAUNCHED);
                }
                Err(_) => toyos_abi::syscall::close(handle),
            }
        }
        Err(e) => {
            say!("init: launcher: cannot start {}: {e}", program.name);
            let _ = conn.try_signal(launch::MSG_REFUSED);
        }
    }
}

/// Handles a launch moved into init, released on every path out of it.
///
/// A `Drop` and not a close at each `return`: there are seven ways out of
/// `serve_launch` and a client picks which one by what it sends.
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

/// What a launch's path resolves to.
enum Resolved<'a> {
    Row(&'a Program),
    /// An installed package, whose row is the image's `[apps]` list.
    Package(Program),
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
fn resolve<'a>(system: &'a Manifest, path: &str) -> Resolved<'a> {
    if !package::is_canonical(path) {
        return Resolved::Refused(format!("{path:?} is not a canonical path"));
    }
    let Some(name) = package::package_of(path) else {
        return match declared(system, path) {
            Some(row) => Resolved::Row(row),
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
    Resolved::Package(system.app_row(name, path))
}

/// What init says about a device it could not mint a claim for.
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
                     `pcidev:` or `partclaim:` line says why")
        }
        other => format!("{name} was refused with {other:?}"),
    }
}

/// Where the acceptors of the names a started program serves come from.
enum Served<'m, 'a> {
    /// The machine-wide map a launch takes from, by move: such a program is
    /// started once per boot.
    Move(&'m mut BTreeMap<&'a str, Acceptor>),
    /// A boot service's own, which init keeps and endows a duplicate of.
    Keep(&'m [(String, Acceptor)]),
    /// The same, for a service init has just stopped: the claims that process
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
/// (`issues/kernel/deferred-release-outlives-its-syscall.md`), so a claim asked
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
    extras: &[(&str, Connector)],
) -> std::io::Result<(Child, Vec<String>)> {
    command.args(&program.args);

    // **Everything endowed stays owned until the spawn that moves it
    // succeeds.** `endow` records a number; a refused spawn moves nothing
    // (`build_child_handles` is all-or-nothing), so a handle whose owner had
    // already been forgotten would be one init holds for ever and a device
    // class nothing can mint again. A client picks whether a spawn fails — an
    // unreachable `cwd` is enough — so this is its path as much as a bug's.
    let mut held = Moved(Vec::new());
    // Acceptors are given *back* rather than closed: a `serves` port whose
    // acceptor is gone can never be served again.
    let mut taken: Vec<(&'a str, Acceptor)> = Vec::new();

    if let Some(ns) = build_namespace(program, system, connectors, extras)? {
        let raw = ns.into_raw();
        command.endow(SVC_LABEL, raw.0);
        held.0.push(raw);
    }
    if let Some(ns) = swap_namespace(program, connectors) {
        let raw = ns.into_raw();
        command.endow(toyos_swap::LABEL, raw.0);
        held.0.push(raw);
    }

    // The namespace answers with connections, so a child holding `surface`
    // only there could never hand `surface` to a child of its own — which is
    // the terminal → shell → `locale` chain. The connector travels labelled as
    // well, and a duplicate because the namespace above took one of its own.
    for (name, connector) in extras {
        // **Not an `expect`.** The connector is a client's, and a client may
        // send one it narrowed `DUP` away from. That is a refused launch, not
        // init's bug — and init is the one process the machine cannot lose.
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
                    panic!("init: `{}` has already been given the `{name}` acceptor", program.name)
                });
                command.endow(&format!("{SERVE_PREFIX}{name}"), acceptor.as_handle().0);
                taken.push((key, acceptor));
            }
            given_back = Some(acceptors);
        }
        Served::Keep(kept) | Served::Restart { acceptors: kept, .. } => {
            for name in &program.serves {
                let (_, acceptor) = kept.iter().find(|(kept, _)| kept == name).ok_or_else(|| {
                    std::io::Error::other(format!("the `{name}` port is closed for good"))
                })?;
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
            .unwrap_or_else(|| panic!("init: `{name}` is not a device this ABI has"));
        // A device this machine does not have is not endowed, and init says
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
                "init: {}: {name} came back from the process it replaces after {} ms and \
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
            Err(e) => say!("init: {}: {}", program.name, refused(name, e)),
        }
    }
    // Nothing was spawned, so everything minted goes back with `held`.
    if let Some(unpaid) = unpaid {
        return Err(unpaid);
    }

    if !program.syscap.is_empty() {
        // A duplicate carrying exactly what the manifest asked for and nothing
        // else. Rights only shrink, so soundd's `rt` cap can never mint a claim
        // or open a process however it asks.
        let rights = toyos_manifest::syscap_rights(&program.syscap)
            .unwrap_or_else(|e| panic!("init: {}: {e}", program.name));
        let narrowed = syscap
            .narrowed(rights)
            .expect("init: the system capability refused a narrowed duplicate");
        let raw = narrowed.into_raw();
        command.endow(SYSCAP_LABEL, raw.0);
        held.0.push(raw);
    }

    match command.spawn() {
        Ok(child) => {
            // The spawn moved every one of them into the child's table, so
            // nothing here may close them.
            held.take();
            for (_, acceptor) in taken {
                let _ = acceptor.into_raw();
            }
            say!("init: started {}", program.name);
            Ok((child, endowed))
        }
        Err(e) => {
            if let Some(acceptors) = given_back {
                for (name, acceptor) in taken {
                    acceptors.insert(name, acceptor);
                }
            }
            Err(e)
        }
    }
}

/// The namespace this program's `receives` names, [`toyos_swap::PORT`] apart.
///
/// A name some program *provides* rather than serves is not init's to give: it
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
    extras: &[(&str, Connector)],
) -> std::io::Result<Option<Namespace>> {
    // Never the swap port: [`swap_namespace`] says why.
    let receives: Vec<&String> =
        program.receives.iter().filter(|name| *name != toyos_swap::PORT).collect();
    if receives.is_empty() && extras.is_empty() {
        return Ok(None);
    }
    let mut builder = namespace::build();
    for name in receives {
        match connectors.get(name.as_str()) {
            Some(connector) => builder = builder.add(name, connector),
            None => assert!(
                system.programs.iter().any(|p| p.provides.contains(name)),
                "init: {} receives `{name}`, which nothing in this image serves or provides",
                program.name,
            ),
        }
    }
    // A `provides` name reaches its holder from whoever made the port, never
    // from init, and this is where it arrives.
    for (name, connector) in extras {
        builder = builder.add(name, connector);
    }
    match builder.finish() {
        Ok(ns) => Ok(Some(ns)),
        Err(e) if extras.is_empty() => {
            panic!("init: no namespace for {}: {e:?}", program.name)
        }
        Err(e) => Err(std::io::Error::other(format!("a provided connector was refused: {e:?}"))),
    }
}

/// [`toyos_swap::PORT`] in a namespace of its own, for a program whose row
/// receives it, endowed under [`toyos_swap::LABEL`].
///
/// **Not an entry of `svc`**, because std gives a duplicate of `svc` to every
/// program its holder spawns directly — sshd running one the manifest does not
/// declare — and to their descendants. A label other than `svc` is not
/// inherited, so the port stays with the one process init endowed.
fn swap_namespace(program: &Program, connectors: &BTreeMap<&str, Connector>) -> Option<Namespace> {
    if !program.receives.iter().any(|name| name == toyos_swap::PORT) {
        return None;
    }
    let connector = connectors
        .get(toyos_swap::PORT)
        .expect("init: the manifest declares init serves `swap`");
    let ns = namespace::build()
        .add(toyos_swap::PORT, connector)
        .finish()
        .unwrap_or_else(|e| panic!("init: no swap namespace for {}: {e:?}", program.name));
    Some(ns)
}
