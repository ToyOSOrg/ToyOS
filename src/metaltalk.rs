//! The cable half of a metal boot: this boot's log as the machine serves it,
//! read from its first line, and the conversation the host then has with that
//! machine over ssh — a ping, one command whose answer is compared, and
//! `reboot`, which is how the host hands the machine back.
//!
//! **The machine is found by its name.** Its netd answers multicast DNS for
//! `toyos-t14.local` once it holds a lease (`toyos_mdns`), so this host asks its
//! own resolver for that name and connects to `logd`'s port there
//! ([`toyos_logstream::PORT`]); nothing is baked into the image about this
//! host, and nothing on this host listens. The ping and both ssh exchanges go
//! to the address the name answered with.
//!
//! **Every answer is recorded, including the ones that are not.** A boot whose
//! name never answered, a command that was refused, a reboot the machine went
//! down under without a word — each is a [`Conversation`] field a judge reads,
//! and the loop that ran it hands the machine to its own fallback (the boot's
//! hold ends, the runner reboots) rather than stopping at the first.
//!
//! The ssh client is `tests/ssh-client-host`, russh from source: no host `ssh`
//! reaches ToyOS.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, ToSocketAddrs};
#[cfg(test)]
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// What the host asks the machine to say, and so what it must answer.
pub const PHRASE: &str = "the T14 answers over its own cable";

/// The command that hands the machine back: `/system/bin/reboot`, which the
/// boot config endows with `power`.
pub const REBOOT: &str = "reboot";

/// The port sshd listens on, which is the protocol's own.
pub const SSH_PORT: u16 = 22;

/// How many echo requests the machine gets to answer one.
const PING_TRIES: u32 = 10;
const PING_WAIT: Duration = Duration::from_secs(1);

/// How long one connect waits for the machine's answer to its SYN.
const CONNECT_WAIT: Duration = Duration::from_secs(5);

/// How long [`Peer::Forwarded`] waits between refusals: a closed host port
/// answers a refusal at once, and asking again with nothing between would
/// spend the wait budget spinning rather than giving the guest time to boot.
const FORWARD_RETRY: Duration = Duration::from_millis(200);

/// The floor under a resilient stream's reconnections: never less than this
/// between one connection ending and the next dial, however fast the last one
/// ended.
const RECONNECT_PACE: Duration = Duration::from_millis(200);

/// The ceiling the pace backs off to while connections keep ending almost as
/// soon as they open: a device mid-reset (a NIC's own handover, not this
/// side's doing) can answer and drop several in a row, and asking again at
/// the floor's pace the whole time would spend the reset's own window
/// hammering a path that is not there yet instead of giving it room to settle.
const RECONNECT_PACE_CEILING: Duration = Duration::from_secs(2);

/// Under this, an episode counts as one more of "ended almost as soon as it
/// opened" for the backoff; at or over it, the pace resets to its floor — the
/// connection was good for a while, so whatever just ended it is a fresh
/// event and not the same reset still settling.
const RAPID_EPISODE: Duration = Duration::from_millis(500);

/// Under this, a failed ask did not wait on anything: this host's resolver
/// holds an unanswered `.local` question for five seconds (measured on the
/// development host, three asks in a row, 5.00 s each), and a connect that
/// failed sooner never had a machine to wait for. Asking again would spin, so
/// it is an answer.
const WAITED: Duration = Duration::from_secs(1);

/// How long a resilient stream keeps dialing after its first connection: not
/// [`Duration::MAX`], which overflows the clock a wait adds it to.
const FOREVER: Duration = Duration::from_secs(365 * 24 * 3600);

/// Where the stream is asked for.
#[derive(Clone, Debug)]
pub enum Peer {
    /// A name this host's resolver answers — `toyos-t14.local`, over multicast
    /// DNS — asked again until the machine answers for it. Each asking is a
    /// wait on the answer: this host's resolver holds an unanswered `.local`
    /// question for five seconds before it gives up.
    Named { host: String, port: u16 },
    /// One address, asked once: a forward onto a guest that is already
    /// serving.
    At(SocketAddr),
    /// One address, asked again on refusal until the machine answers there:
    /// a host-forwarded port, which exists at the bind before the guest that
    /// will serve on it does — unlike [`Peer::At`], a refusal here is the
    /// guest not up yet and not an answer.
    Forwarded(SocketAddr),
}

/// The record stream, as this host reads it: this host dials `logd`'s port and
/// reads until the connection ends.
///
/// **Connection after connection, the newest replacing the one before it**
/// ([`Stream::connect_resilient`]). `logd` serves the whole boot from its
/// first line to every connection, so a reconnect replays lines this reader
/// already has; [`read`] skips them rather than keeping them twice, so every
/// line from every connection is one sequence in arrival order.
#[derive(Clone)]
pub struct Stream {
    shared: Arc<Shared>,
}

struct Shared {
    lines: Mutex<Vec<String>>,
    /// The latest connection's peer.
    peer: Mutex<Option<SocketAddr>>,
    /// The connection currently being read, kept so [`Stream::reconnect`] can
    /// force it closed: a swapped netd leaves it open with no FIN or reset, so
    /// nothing but this host ever ends it.
    current: Mutex<Option<TcpStream>>,
    /// How many connections have been made.
    connections: AtomicUsize,
    /// The latest connection has ended and no other has been made since.
    ended: AtomicBool,
    /// How the latest connection ended and how long after it opened, once it
    /// has.
    end: Mutex<Option<End>>,
    /// Lines a connection's end cut short, discarded rather than kept: a
    /// reconnect's replay carries the same content whole.
    torn: AtomicUsize,
    /// Why no connection was ever made, once that is settled.
    unopened: Mutex<Option<String>>,
    /// Paired with no field of its own: every wait below re-reads whichever
    /// field it cares about after taking this lock, so what it guards is
    /// never more than a wake.
    gate: Mutex<()>,
    /// Woken by every change above.
    moved: Condvar,
    /// Set to stop asking for a peer that has not answered, and to stop
    /// reconnecting once the current connection ends.
    stop: AtomicBool,
}

impl Stream {
    /// Ask `peer` for its log within `by`, and read it on a thread of its own
    /// until the connection ends: every line is appended to `file` as it
    /// arrives — a machine that dies mid-boot leaves what it said on disk —
    /// and, with `echo`, printed. One connection, never asked for again.
    pub fn connect(peer: Peer, file: &Path, echo: bool, by: Duration) -> Result<Self, String> {
        Self::dial(peer, file, echo, by, false)
    }

    /// [`Stream::connect`], asked for again for as long as this process runs
    /// or until [`Stream::give_up`]: a connection that ends is a boot that
    /// left and came back, never a reason to stop reading it.
    pub fn connect_resilient(peer: Peer, file: &Path, echo: bool, by: Duration) -> Result<Self, String> {
        Self::dial(peer, file, echo, by, true)
    }

    fn dial(peer: Peer, file: &Path, echo: bool, by: Duration, resilient: bool) -> Result<Self, String> {
        let out = Arc::new(Mutex::new(
            std::fs::File::create(file).map_err(|e| format!("{}: {e}", file.display()))?,
        ));
        let shared = Arc::new(Shared {
            lines: Mutex::new(Vec::new()),
            peer: Mutex::new(None),
            current: Mutex::new(None),
            connections: AtomicUsize::new(0),
            ended: AtomicBool::new(false),
            end: Mutex::new(None),
            torn: AtomicUsize::new(0),
            unopened: Mutex::new(None),
            gate: Mutex::new(()),
            moved: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let theirs = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("metal-stream".into())
            .spawn(move || {
                let mut bound = by;
                let mut pace = RECONNECT_PACE;
                loop {
                    if theirs.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let conn = match open(&peer, bound, &theirs.stop) {
                        Ok(conn) => conn,
                        Err(why) => {
                            *theirs.unopened.lock().expect("the stream's unopened state") = Some(why);
                            let _gate = theirs.gate.lock().expect("the stream's gate");
                            theirs.moved.notify_all();
                            return;
                        }
                    };
                    let at = conn.peer_addr().ok();
                    if echo {
                        println!("  stream: reading {peer:?} at {at:?}");
                    }
                    *theirs.peer.lock().expect("the stream's peer") = at;
                    *theirs.current.lock().expect("the stream's current connection") =
                        conn.try_clone().ok();
                    theirs.ended.store(false, Ordering::SeqCst);
                    theirs.connections.fetch_add(1, Ordering::SeqCst);
                    let index = theirs.connections.load(Ordering::SeqCst);
                    {
                        let _gate = theirs.gate.lock().expect("the stream's gate");
                        theirs.moved.notify_all();
                    }
                    let opened = Instant::now();
                    read(conn, index, &theirs, &out, echo);
                    if !resilient || theirs.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    // **A floor under every reconnection, whatever ended the
                    // last one, backed off while they keep ending almost as
                    // soon as they open.** A device mid-reset (its own NIC
                    // handover, not this side's doing) can accept and drop
                    // several connections in a row; asking again at the floor's
                    // pace the whole time would spend the reset's own window
                    // hammering a path that is not there yet, so a run of rapid
                    // episodes backs the pace off, and one that holds resets it.
                    pace = if opened.elapsed() < RAPID_EPISODE {
                        (pace * 2).min(RECONNECT_PACE_CEILING)
                    } else {
                        RECONNECT_PACE
                    };
                    std::thread::sleep(pace);
                    // Every reconnection replays the whole boot from its first
                    // line, so this side keeps dialing for as long as it takes
                    // rather than the bound its first connection had.
                    bound = FOREVER;
                }
            })
            .map_err(|e| format!("the stream's reader could not be started: {e}"))?;
        Ok(Self { shared })
    }

    /// The latest connection's peer.
    pub fn peer(&self) -> Option<SocketAddr> {
        *self.shared.peer.lock().expect("the stream's peer")
    }

    pub fn lines(&self) -> Vec<String> {
        self.shared.lines.lock().expect("the stream's lines").clone()
    }

    /// How many connections this reader has made.
    pub fn connections(&self) -> usize {
        self.shared.connections.load(Ordering::SeqCst)
    }

    /// Lines a connection's end cut short.
    pub fn torn(&self) -> usize {
        self.shared.torn.load(Ordering::SeqCst)
    }

    /// Whether the latest connection has ended with none made since.
    pub fn ended(&self) -> bool {
        self.end().is_some()
    }

    /// How the latest connection ended, or `None` while it is open or never
    /// opened.
    pub fn end(&self) -> Option<End> {
        self.shared.end.lock().expect("the stream's end").clone()
    }

    /// Why no connection was ever made, once that is settled.
    pub fn unopened(&self) -> Option<String> {
        self.shared.unopened.lock().expect("the stream's unopened state").clone()
    }

    /// Stop asking for a peer that has not answered, and stop reconnecting
    /// once the current connection ends. A connection already made is read on.
    pub fn give_up(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// Force the current connection closed, so a resilient stream dials again
    /// at once rather than waiting on a byte its peer may never send: a
    /// swapped netd leaves the old connection open with no FIN or reset.
    /// Nothing on a stream with no open connection.
    pub fn reconnect(&self) {
        if let Some(conn) = &*self.shared.current.lock().expect("the stream's current connection") {
            let _ = conn.shutdown(std::net::Shutdown::Both);
        }
    }

    /// The peer, once one has connected, or `None` after `by` or once the
    /// host has given up on one.
    pub fn wait_connected(&self, by: Duration) -> Option<SocketAddr> {
        self.wait_for_connection(0, by)
    }

    /// The peer of a connection made after the first `seen`, or `None` after
    /// `by`, once the host has given up, or once no connection was ever made.
    pub fn wait_for_connection(&self, seen: usize, by: Duration) -> Option<SocketAddr> {
        let began = Instant::now();
        let mut gate = self.shared.gate.lock().expect("the stream's gate");
        loop {
            if self.connections() > seen {
                return self.peer();
            }
            if self.unopened().is_some() {
                return None;
            }
            let left = by.saturating_sub(began.elapsed());
            if left.is_zero() || self.shared.stop.load(Ordering::SeqCst) {
                return None;
            }
            gate = self.shared.moved.wait_timeout(gate, left).expect("the stream's gate").0;
        }
    }

    /// Wait for a line carrying `needle`, or `by`; whether one arrived. Woken
    /// by each line as it lands, never a poll.
    pub fn wait_for(&self, needle: &str, by: Duration) -> bool {
        let began = Instant::now();
        let mut gate = self.shared.gate.lock().expect("the stream's gate");
        loop {
            if self.lines().iter().any(|l| l.contains(needle)) {
                return true;
            }
            let left = by.saturating_sub(began.elapsed());
            if left.is_zero() || self.unopened().is_some() {
                return false;
            }
            gate = self.shared.moved.wait_timeout(gate, left).expect("the stream's gate").0;
        }
    }

    /// Wait until the connection ends, or `by` has passed; whether it ended.
    pub fn wait_ended(&self, by: Duration) -> bool {
        let began = Instant::now();
        let mut gate = self.shared.gate.lock().expect("the stream's gate");
        while !self.ended() {
            let left = by.saturating_sub(began.elapsed());
            if left.is_zero() || self.unopened().is_some() {
                return self.ended();
            }
            gate = self.shared.moved.wait_timeout(gate, left).expect("the stream's gate").0;
        }
        true
    }
}

/// The connection, or why there is none by `by`.
///
/// **A refused connection is an answer, not a wait**: a machine that answers
/// for its name holds a lease, and `logd` binds its port before netd can have
/// one, so a refusal there is a machine that is not serving its log.
fn open(peer: &Peer, by: Duration, stop: &AtomicBool) -> Result<TcpStream, String> {
    let (host, port) = match peer {
        Peer::At(at) => {
            return TcpStream::connect_timeout(at, CONNECT_WAIT)
                .map_err(|e| format!("{at} did not take the connection: {e}"));
        }
        Peer::Forwarded(at) => return open_forwarded(*at, by, stop),
        Peer::Named { host, port } => (host.as_str(), *port),
    };
    let began = Instant::now();
    let mut last = String::from("never asked");
    while began.elapsed() < by {
        if stop.load(Ordering::SeqCst) {
            return Err(format!("given up on {host} after {} s: {last}", began.elapsed().as_secs()));
        }
        let asked = Instant::now();
        let addrs: Vec<SocketAddr> = match (host, port).to_socket_addrs() {
            Ok(addrs) => addrs.filter(SocketAddr::is_ipv4).collect(),
            Err(e) if asked.elapsed() < WAITED => {
                return Err(format!("this host's resolver did not ask for {host} at all: {e}"));
            }
            Err(e) => {
                last = format!("{host} did not resolve: {e}");
                continue;
            }
        };
        for at in addrs {
            let asked = Instant::now();
            match TcpStream::connect_timeout(&at, CONNECT_WAIT) {
                Ok(conn) => return Ok(conn),
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    return Err(format!(
                        "{host} answered as {at}, and nothing there serves the log on port \
                         {port}: {e}"
                    ));
                }
                Err(e) if asked.elapsed() < WAITED => {
                    return Err(format!("{host} answered as {at}, which this host cannot reach: {e}"));
                }
                Err(e) => last = format!("{host} answered as {at}, which did not answer: {e}"),
            }
        }
    }
    Err(format!("{host} was not serving its log within {} s: {last}", by.as_secs()))
}

/// [`Peer::Forwarded`]'s connect: `at` is asked again on every refusal, unlike
/// [`open`]'s [`Peer::Named`] arm, because there is no name whose resolution
/// already answers for the machine being up — the forward exists at the host
/// before the guest that will accept on it does.
fn open_forwarded(at: SocketAddr, by: Duration, stop: &AtomicBool) -> Result<TcpStream, String> {
    let began = Instant::now();
    let mut last = String::from("never asked");
    while began.elapsed() < by {
        if stop.load(Ordering::SeqCst) {
            return Err(format!("given up on {at} after {} s: {last}", began.elapsed().as_secs()));
        }
        match TcpStream::connect_timeout(&at, CONNECT_WAIT) {
            Ok(conn) => return Ok(conn),
            Err(e) => {
                last = format!("{at} did not take the connection: {e}");
                std::thread::sleep(FORWARD_RETRY);
            }
        }
    }
    Err(format!("{at} was not serving its log within {} s: {last}", by.as_secs()))
}

/// Read one connection's lines into the stream until it ends, and record how
/// it ended if it is still the latest.
///
/// **Every connection replays the whole boot from its first line**
/// (`serve.rs`'s design): the lines this reader already has are skipped
/// rather than kept twice, and a line the connection ends inside is dropped
/// rather than counted, so the same content lands whole when the next
/// connection replays it.
fn read(conn: TcpStream, index: usize, shared: &Shared, out: &Mutex<std::fs::File>, echo: bool) {
    let opened = Instant::now();
    let mut skip = shared.lines.lock().expect("the stream's lines").len();
    let mut reader = BufReader::new(conn);
    let how = loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break "the peer closed it".to_string(),
            Err(e) => break e.to_string(),
            Ok(_) if !line.ends_with('\n') => {
                shared.torn.fetch_add(1, Ordering::SeqCst);
            }
            Ok(_) if skip > 0 => {
                skip -= 1;
            }
            Ok(_) => {
                {
                    let mut out = out.lock().expect("the stream's file");
                    let _ = out.write_all(line.as_bytes());
                    let _ = out.flush();
                }
                if echo {
                    print!("  stream| {line}");
                    let _ = std::io::stdout().flush();
                }
                shared.lines.lock().expect("the stream's lines").push(line);
                // Under the gate's lock, so a waiter between its look and its
                // wait cannot miss this line.
                let _gate = shared.gate.lock().expect("the stream's gate");
                shared.moved.notify_all();
            }
        }
    };
    let end = End { after_ms: opened.elapsed().as_millis() as u64, how };
    if echo {
        println!("  stream: ended {} ms after it opened: {}", end.after_ms, end.how);
    }
    if shared.connections.load(Ordering::SeqCst) == index {
        *shared.end.lock().expect("the stream's end") = Some(end);
        shared.ended.store(true, Ordering::SeqCst);
    }
    let _gate = shared.gate.lock().expect("the stream's gate");
    shared.moved.notify_all();
}

/// How a stream's connection ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct End {
    /// Milliseconds from the connect to the end, on this host's clock.
    pub after_ms: u64,
    /// The peer's close, or the error the read ended on.
    pub how: String,
}

/// What one exchange with the machine's sshd came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exec {
    pub stdout: Vec<u8>,
    pub status: Option<u32>,
}

/// The harness's ssh client, and the key the image authorizes.
#[derive(Debug, Clone)]
pub struct Ssh {
    /// The repository whose `tests/ssh-client-host` built the client.
    root: PathBuf,
    key: PathBuf,
}

impl Ssh {
    /// The client this repository builds, refused by name where it is not
    /// built: `build::build_host_judges` is what makes it.
    pub fn at(root: &Path, key: PathBuf) -> Result<Self, String> {
        let client = crate::build::ssh_client_host(root);
        if !client.is_file() {
            return Err(format!(
                "{} is not built; the metal staging of a talking boot builds it",
                client.display()
            ));
        }
        if !key.is_file() {
            return Err(format!("{} is no key: the staging mints it beside the image", key.display()));
        }
        Ok(Self { root: root.to_path_buf(), key })
    }

    fn run(&self, argv: &[&str]) -> Result<String, String> {
        let out = Command::new(crate::build::ssh_client_host(&self.root))
            .args(argv)
            .output()
            .map_err(|e| format!("the ssh client would not start: {e}"))?;
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() {
            return Err(said);
        }
        Ok(said)
    }

    /// Run `command` and collect its stdout and status.
    pub fn exec(&self, at: SocketAddr, command: &str, scratch: &Path) -> Result<Exec, String> {
        let (out, err) = (scratch.join("exec.out"), scratch.join("exec.err"));
        let (host, port) = (at.ip().to_string(), at.port().to_string());
        let said = self.run(&[
            "exec",
            &host,
            &port,
            path_str(&self.key)?,
            path_str(&out)?,
            path_str(&err)?,
            command,
        ])?;
        let status = match said.lines().last().unwrap_or("") {
            "no-exit-status" => None,
            line => Some(
                line.strip_prefix("exit ")
                    .and_then(|code| code.parse().ok())
                    .ok_or_else(|| format!("the client answered {said:?}"))?,
            ),
        };
        let stdout = std::fs::read(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        Ok(Exec { stdout, status })
    }

    /// Ask for `command` and answer the machine's reply to the request, without
    /// waiting for the program.
    pub fn fire(&self, at: SocketAddr, command: &str) -> Result<String, String> {
        let (host, port) = (at.ip().to_string(), at.port().to_string());
        let said = self.run(&["fire", &host, &port, path_str(&self.key)?, command])?;
        Ok(said.lines().last().unwrap_or("").to_string())
    }

    /// Send `binary` as `service`'s replacement, naming `digest` for it, and
    /// answer the machine's word: `accepted <path>`, `refused <why>`,
    /// `unanswered <what>` or `no-subsystem`.
    pub fn swap(
        &self,
        at: SocketAddr,
        service: &str,
        binary: &Path,
        digest: &toyos_swap::Digest,
    ) -> Result<String, String> {
        let (host, port) = (at.ip().to_string(), at.port().to_string());
        let said = self.run(&[
            "swap",
            &host,
            &port,
            path_str(&self.key)?,
            service,
            path_str(binary)?,
            &toyos_swap::hex(digest),
        ])?;
        Ok(said.lines().last().unwrap_or("").to_string())
    }
}

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str().ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

/// Everything the host heard from one boot over the cable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// Who opened the stream: the address the boot leased.
    pub peer: Ipv4Addr,
    /// Whether it answered a ping, or `None` where none was asked.
    pub ping: Option<bool>,
    /// What `echo` answered, or the client's refusal.
    pub exec: Result<Exec, String>,
    /// The machine's reply to `reboot`, or the client's refusal.
    pub reboot: Result<String, String>,
    /// From the stream opening to the command's answer, on this host's clock.
    pub exec_ms: u64,
    /// How the stream's connection had ended when the conversation did, or
    /// `None` for one still open.
    pub stream_end: Option<End>,
}

/// The command asked, spelled once for the asker and the judge.
pub fn asked() -> String {
    format!("echo {PHRASE}")
}

/// The answer that command owes, byte for byte.
pub fn owed() -> Vec<u8> {
    format!("{PHRASE}\n").into_bytes()
}

/// Talk to the machine `stream` reads: ping it, ask it to say [`PHRASE`], and
/// then ask it to reboot — whatever the first two said, because handing the
/// machine back is owed either way.
///
/// `ssh_at` is where sshd is reached, and `None` is the peer's own port 22;
/// QEMU's forward is the other case. `ping` is `false` where no ICMP can reach
/// the machine at all, which is QEMU's user-mode network.
///
/// **One ask of each**, each a wait on the machine's answer: a machine serving
/// its log holds a lease, and sshd binds its port before netd can have one.
///
/// `Err` is only a stream that never opened.
pub fn converse(
    stream: &Stream,
    ssh: &Ssh,
    ssh_at: Option<SocketAddr>,
    ping: bool,
    scratch: &Path,
) -> Result<Conversation, String> {
    // The bound the connection itself already applied is `connect`'s `by`; this
    // wait is for that settling, not a second bound on it.
    let peer = stream
        .wait_connected(FOREVER)
        .ok_or_else(|| stream.unopened().unwrap_or_else(|| "the stream never opened".to_string()))?;
    let SocketAddr::V4(peer_v4) = peer else {
        return Err(format!("the stream's peer is {peer}, which is no IPv4 address"));
    };
    let began = Instant::now();
    let peer = *peer_v4.ip();
    let ssh_at = ssh_at.unwrap_or(SocketAddr::V4(SocketAddrV4::new(peer, SSH_PORT)));

    let ping = if ping {
        let mut answered = false;
        for _ in 0..PING_TRIES {
            if crate::icmp::echo(peer, PING_WAIT)? {
                answered = true;
                break;
            }
        }
        println!("  talk: {peer} {} a ping", if answered { "answered" } else { "did not answer" });
        Some(answered)
    } else {
        None
    };

    std::fs::create_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let command = asked();
    let exec = ssh.exec(ssh_at, &command, scratch);
    let exec_ms = began.elapsed().as_millis() as u64;
    match &exec {
        Ok(got) => println!(
            "  talk: `{command}` answered {:?}, status {:?}, {exec_ms} ms after the stream opened",
            String::from_utf8_lossy(&got.stdout),
            got.status
        ),
        Err(why) => println!("  talk: `{command}` was not answered: {why}"),
    }

    let reboot = ssh.fire(ssh_at, REBOOT);
    println!("  talk: `{REBOOT}` {reboot:?}");
    Ok(Conversation { peer, ping, exec, reboot, exec_ms, stream_end: stream.end() })
}

/// The keys a conversation is written under, one `<key> <value>` per line, in
/// the loop's readback beside the host's other facts.
const PEER: &str = "talk_peer";
const PING: &str = "talk_ping";
const EXEC_STATUS: &str = "talk_exec_status";
const EXEC_STDOUT: &str = "talk_exec_stdout";
const EXEC_REFUSED: &str = "talk_exec_refused";
const EXEC_MS: &str = "talk_exec_ms";
const REBOOTED: &str = "talk_reboot";
const REBOOT_REFUSED: &str = "talk_reboot_refused";
const STREAM_END: &str = "talk_stream_end";

/// A value on one line, whatever it carried: Rust's own escaping, read back by
/// comparison with the same rendering rather than parsed.
fn one_line(text: &str) -> String {
    format!("{text:?}")
}

impl Conversation {
    pub fn render(&self) -> String {
        let mut out = format!("{PEER} {}\n", self.peer);
        match self.ping {
            Some(true) => out.push_str(&format!("{PING} yes\n")),
            Some(false) => out.push_str(&format!("{PING} no\n")),
            None => {}
        }
        match &self.exec {
            Ok(exec) => {
                match exec.status {
                    Some(code) => out.push_str(&format!("{EXEC_STATUS} {code}\n")),
                    None => out.push_str(&format!("{EXEC_STATUS} none\n")),
                }
                out.push_str(&format!(
                    "{EXEC_STDOUT} {}\n",
                    one_line(&String::from_utf8_lossy(&exec.stdout))
                ));
            }
            Err(why) => out.push_str(&format!("{EXEC_REFUSED} {}\n", one_line(why))),
        }
        out.push_str(&format!("{EXEC_MS} {}\n", self.exec_ms));
        match &self.stream_end {
            Some(end) => out.push_str(&format!("{STREAM_END} {} {}\n", end.after_ms, one_line(&end.how))),
            None => out.push_str(&format!("{STREAM_END} open\n")),
        }
        match &self.reboot {
            Ok(word) => out.push_str(&format!("{REBOOTED} {word}\n")),
            Err(why) => out.push_str(&format!("{REBOOT_REFUSED} {}\n", one_line(why))),
        }
        out
    }

    /// What a readback's boot file says about the cable's conversation, or
    /// `None` where the boot had none.
    pub fn parse(text: &str) -> Result<Option<Heard>, String> {
        let word = |key: &str| -> Option<String> {
            text.lines().find_map(|line| {
                let (name, rest) = line.split_once(' ')?;
                (name == key).then(|| rest.to_string())
            })
        };
        let Some(peer) = word(PEER) else { return Ok(None) };
        let peer = peer.parse().map_err(|_| format!("{PEER} reads {peer:?}"))?;
        let ping = match word(PING).as_deref() {
            Some("yes") => Some(true),
            Some("no") => Some(false),
            None => None,
            Some(other) => return Err(format!("{PING} reads {other:?}")),
        };
        let exec = match (word(EXEC_STATUS), word(EXEC_STDOUT), word(EXEC_REFUSED)) {
            (Some(status), Some(stdout), None) => Ok((status, stdout)),
            (None, None, Some(why)) => Err(why),
            _ => return Err(format!("the boot file's exec keys are not one answer:\n{text}")),
        };
        let reboot = match (word(REBOOTED), word(REBOOT_REFUSED)) {
            (Some(said), None) => Ok(said),
            (None, Some(why)) => Err(why),
            _ => return Err(format!("the boot file's reboot keys are not one answer:\n{text}")),
        };
        let exec_ms = word(EXEC_MS)
            .and_then(|ms| ms.parse().ok())
            .ok_or_else(|| format!("the boot file names no {EXEC_MS}"))?;
        let stream_end = match word(STREAM_END).as_deref() {
            Some("open") => None,
            Some(said) => {
                let (ms, how) = said.split_once(' ').unwrap_or((said, ""));
                let after_ms =
                    ms.parse().map_err(|_| format!("{STREAM_END} reads {said:?}"))?;
                Some(End { after_ms, how: how.to_string() })
            }
            None => return Err(format!("the boot file names no {STREAM_END}:\n{text}")),
        };
        Ok(Some(Heard { peer, ping, exec, reboot, exec_ms, stream_end }))
    }
}

/// A conversation as a readback carries it: the command's answer as rendered,
/// for comparison with [`Heard::answered_as_owed`] rather than for reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    pub peer: Ipv4Addr,
    pub ping: Option<bool>,
    /// `(status, stdout rendered)`, or the refusal rendered.
    pub exec: Result<(String, String), String>,
    pub reboot: Result<String, String>,
    pub exec_ms: u64,
    /// How the stream had ended when the conversation did, its reason as
    /// rendered; `None` for one still open.
    pub stream_end: Option<End>,
}

impl Heard {
    /// The command ran, ended 0, and said exactly [`PHRASE`] and a newline.
    pub fn answered_as_owed(&self) -> Result<(), String> {
        match &self.exec {
            Ok((status, stdout)) => {
                let want = one_line(&String::from_utf8_lossy(&owed()));
                if status != "0" || *stdout != want {
                    return Err(format!(
                        "`{}` answered {stdout} with status {status}, where {want} and 0 are owed",
                        asked()
                    ));
                }
                Ok(())
            }
            Err(why) => Err(format!("`{}` was never answered: {why}", asked())),
        }
    }

    /// The machine took `reboot`: it said so, or it went away under the
    /// request — which only the boot's own log can then tell from a machine
    /// that was already going.
    pub fn reboot_was_taken(&self) -> Result<&str, String> {
        match &self.reboot {
            Ok(word) if matches!(word.as_str(), "accepted" | "closed" | "silent") => Ok(word),
            Ok(word) => Err(format!("the machine answered `{REBOOT}` with {word:?}")),
            Err(why) => Err(format!("`{REBOOT}` could not be asked: {why}")),
        }
    }
}

/// What the host heard over the cable, judged: every finding, or what was
/// heard said in one line per fact.
///
/// **The stream is judged by what it carries, not by its length.** It owes the
/// boot's `Boot: complete` — the record that says the lines are this boot's,
/// from its first — and the peer that served it is the machine that then
/// answered the ping and the command, because both were asked of that address
/// and no other.
pub fn judge(heard: &Heard, stream: &[String]) -> Result<Vec<String>, Vec<String>> {
    let mut bad = Vec::new();
    let mut said = Vec::new();
    match crate::bootlog::boot_millis(&stream.concat()) {
        Some(ms) => said.push(format!(
            "{} line(s) arrived from {} over the cable, `Boot: complete` ({ms} ms) among them",
            stream.len(),
            heard.peer
        )),
        None => bad.push(format!(
            "{} line(s) arrived over the cable and none is this boot's `Boot: complete`",
            stream.len()
        )),
    }
    // The machine serves the boot from its first line to whoever connects, so
    // a connection that ends before a byte is one it refused or dropped.
    if let (true, Some(end)) = (stream.is_empty(), &heard.stream_end) {
        bad.push(format!(
            "{} closed the stream {} ms after this host connected, {}, before a byte arrived",
            heard.peer, end.after_ms, end.how
        ));
    }
    match heard.ping {
        Some(true) => said.push(format!("{} answered a ping", heard.peer)),
        Some(false) => bad.push(format!("{} answered no ping in {PING_TRIES} tries", heard.peer)),
        None => said.push("no ping was asked".to_string()),
    }
    match heard.answered_as_owed() {
        Ok(()) => said.push(format!(
            "`{}` answered byte for byte with status 0, {} ms after the stream opened",
            asked(),
            heard.exec_ms
        )),
        Err(why) => bad.push(why),
    }
    match heard.reboot_was_taken() {
        Ok(word) => said.push(format!("`{REBOOT}` was taken ({word})")),
        Err(why) => bad.push(why),
    }
    if bad.is_empty() {
        Ok(said)
    } else {
        Err(bad)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard(exec: Result<Exec, String>, reboot: Result<String, String>) -> Heard {
        let said = Conversation {
            peer: Ipv4Addr::new(192, 168, 1, 49),
            ping: Some(true),
            exec,
            reboot,
            exec_ms: 812,
            stream_end: None,
        };
        Conversation::parse(&format!("back_secs 60\n{}stick_secs 0\n", said.render()))
            .expect("a rendered conversation reads back")
            .expect("and names a peer")
    }

    /// What the loop writes is what the judge reads, answer by answer, and the
    /// answer the command owes is compared byte for byte through the rendering.
    #[test]
    fn a_conversation_reads_back_as_it_was_written() {
        let good = heard(Ok(Exec { stdout: owed(), status: Some(0) }), Ok("accepted".into()));
        assert_eq!(good.peer, Ipv4Addr::new(192, 168, 1, 49));
        assert_eq!(good.ping, Some(true));
        assert_eq!(good.exec_ms, 812);
        good.answered_as_owed().expect("the owed answer");
        assert_eq!(good.reboot_was_taken(), Ok("accepted"));

        // One byte off, a status off, no status: each is a red of its own.
        let mut short = owed();
        short.pop();
        for (stdout, status) in [(short, Some(0)), (owed(), Some(1)), (owed(), None)] {
            let bad = heard(Ok(Exec { stdout, status }), Ok("accepted".into()));
            assert!(bad.answered_as_owed().is_err(), "{bad:?}");
        }
        let never = heard(Err("connecting: refused\nand more".into()), Ok("closed".into()));
        assert!(never.answered_as_owed().unwrap_err().contains("never answered"));
        assert_eq!(never.reboot_was_taken(), Ok("closed"));
        assert!(heard(Ok(Exec { stdout: owed(), status: Some(0) }), Ok("refused".into()))
            .reboot_was_taken()
            .is_err());
        assert!(heard(Ok(Exec { stdout: owed(), status: Some(0) }), Err("no key".into()))
            .reboot_was_taken()
            .is_err());
    }

    /// The judge wants the boot's own records on the wire and every answer the
    /// host asked for; each missing fact is a finding of its own.
    #[test]
    fn the_judge_reads_what_the_stream_carried_and_what_the_peer_answered() {
        let stream = vec![
            "[---------- -------- 0.011 cpu0] SMP: AP cpu1 online\n".to_string(),
            "[---------- -------- 1.216 cpu0] Boot: complete (1216ms)\n".to_string(),
        ];
        let good = heard(Ok(Exec { stdout: owed(), status: Some(0) }), Ok("accepted".into()));
        let said = judge(&good, &stream).expect("every fact is there");
        assert_eq!(said.len(), 4, "{said:?}");

        let bad = judge(&good, &stream[..1]).unwrap_err();
        assert!(bad.iter().any(|b| b.contains("Boot: complete")), "{bad:?}");
        let mut silent = good.clone();
        silent.ping = Some(false);
        assert_eq!(judge(&silent, &stream).unwrap_err().len(), 1);
        let mut unasked = good;
        unasked.ping = None;
        assert!(judge(&unasked, &stream).is_ok());
        let bad = judge(&heard(Err("refused".into()), Ok("refused".into())), &[]).unwrap_err();
        assert_eq!(bad.len(), 3, "{bad:?}");
    }

    /// A machine that closes the stream before a byte is named as that, and
    /// not read as a boot that said nothing.
    #[test]
    fn a_stream_closed_before_a_byte_is_named_and_not_read_as_silence() {
        let dir = std::env::temp_dir().join(format!("metaltalk-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = server.local_addr().unwrap();
        let stream = Stream::connect(Peer::At(at), &dir.join("s.log"), false, Duration::from_secs(5))
            .expect("a loopback reader");
        drop(server.accept().unwrap());
        assert!(stream.wait_ended(Duration::from_secs(5)), "the close is read as an end");
        let end = stream.end().expect("the end is recorded");
        assert_eq!(end.how, "the peer closed it");
        assert!(stream.lines().is_empty());

        let said = Conversation {
            peer: Ipv4Addr::LOCALHOST,
            ping: Some(true),
            exec: Ok(Exec { stdout: owed(), status: Some(0) }),
            reboot: Ok("accepted".into()),
            exec_ms: 457,
            stream_end: Some(end),
        };
        let cut = Conversation::parse(&said.render()).unwrap().unwrap();
        let bad = judge(&cut, &[]).unwrap_err();
        assert!(bad.iter().any(|b| b.contains("before a byte arrived")), "{bad:?}");
        let lines = vec!["[---------- -------- 1.216 cpu0] Boot: complete (1216ms)\n".to_string()];
        assert!(judge(&cut, &lines).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A boot file with no conversation is a boot that had none, and one with
    /// half of one is refused rather than read as either.
    #[test]
    fn a_boot_file_without_a_conversation_names_none_and_half_of_one_is_refused() {
        assert_eq!(Conversation::parse("back_secs 60\nstick_secs 0\n"), Ok(None));
        let half = "talk_peer 10.0.2.15\ntalk_exec_status 0\ntalk_exec_ms 5\ntalk_reboot accepted\n";
        assert!(Conversation::parse(half).is_err());
        let both = "talk_peer 10.0.2.15\ntalk_exec_refused \"x\"\ntalk_exec_ms 5\n\
                    talk_reboot accepted\ntalk_reboot_refused \"y\"\n";
        assert!(Conversation::parse(both).is_err());
    }

    /// **A name is asked of this host's resolver and the machine found by it**:
    /// the reader connects to what `localhost` answers, names that peer, and
    /// keeps every line whole and in order.
    #[test]
    fn the_stream_is_read_from_the_address_its_name_answers() {
        let dir = std::env::temp_dir().join(format!("metaltalk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("stream.log");
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let peer = Peer::Named { host: "localhost".to_string(), port };
        let stream = Stream::connect(peer, &file, false, Duration::from_secs(10)).unwrap();
        let (mut conn, _) = server.accept().unwrap();
        for i in 0..100 {
            writeln!(conn, "[kernel 0.{i:03} cpu0] line {i}").unwrap();
        }
        drop(conn);
        let peer = stream.wait_connected(Duration::from_secs(5)).expect("the peer");
        assert_eq!(peer.ip(), std::net::IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(stream.wait_ended(Duration::from_secs(5)));
        let lines = stream.lines();
        assert_eq!(lines.len(), 100);
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(*line, format!("[kernel 0.{i:03} cpu0] line {i}\n"));
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), lines.concat());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A machine that answers for its name and refuses the port is an
    /// answer**: it is up and not serving its log, and asking again would
    /// only ask it again.
    #[test]
    fn a_refused_port_at_an_answering_name_is_the_answer() {
        let dir = std::env::temp_dir().join(format!("metaltalk-refused-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let peer = Peer::Named { host: "localhost".to_string(), port };
        let began = Instant::now();
        let stream = Stream::connect(peer, &dir.join("s.log"), false, Duration::from_secs(60)).unwrap();
        assert!(stream.wait_connected(Duration::from_secs(60)).is_none());
        let why = stream.unopened().expect("nothing serves there");
        assert!(why.contains("nothing there serves the log"), "{why}");
        assert!(began.elapsed() < Duration::from_secs(10), "a refusal was waited on");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A peer that connects and says nothing yet is a stream, not an end**:
    /// the reader blocks on its next line rather than reading the silence as a
    /// close. The reader has no timer, so the silence is an order of events and
    /// not a length of time: the connection is seen before the peer speaks, and
    /// the stream is still open once what it said has been read.
    #[test]
    fn a_peer_quiet_after_connecting_is_still_read() {
        let dir = std::env::temp_dir().join(format!("metaltalk-quiet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = server.local_addr().unwrap();
        let stream = Stream::connect(Peer::At(at), &dir.join("s.log"), false, Duration::from_secs(5))
            .expect("a loopback reader");
        let (mut conn, _) = server.accept().unwrap();
        stream.wait_connected(Duration::from_secs(5)).expect("the peer");
        writeln!(conn, "[kernel 1.216 cpu0] Boot: complete (1216ms)").unwrap();
        assert!(stream.wait_for("Boot: complete", Duration::from_secs(5)), "the first line was not read");
        assert!(!stream.wait_ended(Duration::ZERO), "a quiet peer was read as a closed one");
        writeln!(conn, "[kernel 1.217 cpu0] init: started logd").unwrap();
        drop(conn);
        assert!(stream.wait_ended(Duration::from_secs(5)));
        assert_eq!(
            stream.lines(),
            vec!["[kernel 1.216 cpu0] Boot: complete (1216ms)\n", "[kernel 1.217 cpu0] init: started logd\n"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A swapped netd leaves the old connection open on this side**: no FIN,
    /// no reset. [`Stream::reconnect`] is what ends it, and the second
    /// connection's replay — the whole boot from its first line, `logd`'s own
    /// design — is skipped where this reader already has it rather than kept
    /// twice.
    #[test]
    fn a_reconnect_replays_the_boot_and_skips_what_this_reader_already_has() {
        let dir = std::env::temp_dir().join(format!("metaltalk-again-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = server.local_addr().unwrap();
        let stream =
            Stream::connect_resilient(Peer::At(at), &dir.join("s.log"), false, Duration::from_secs(5))
                .expect("a loopback reader");
        let (mut first, _) = server.accept().unwrap();
        writeln!(first, "[kernel 0.001 cpu0] before the swap").unwrap();
        assert!(stream.wait_for("before the swap", Duration::from_secs(5)));
        stream.reconnect();
        // The replay, from the boot's first line, as `logd` hands it to every
        // connection: the line this reader already has, once more, and the
        // one that is new to it.
        let (mut second, _) = server.accept().unwrap();
        writeln!(second, "[kernel 0.001 cpu0] before the swap").unwrap();
        writeln!(second, "[kernel 9.000 cpu0] after the swap").unwrap();
        let began = Instant::now();
        while stream.lines().len() < 2 && began.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(stream.connections(), 2);
        assert_eq!(
            stream.lines(),
            vec!["[kernel 0.001 cpu0] before the swap\n", "[kernel 9.000 cpu0] after the swap\n"]
        );
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
