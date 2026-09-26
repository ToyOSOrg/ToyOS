// The bindings name the core's deeply nested socket types, and proving them
// `Sync` takes the depth the core itself raises its own limit to.
#![recursion_limit = "256"]

use std::collections::HashMap;
use std::num::{NonZeroU16, NonZeroUsize};
use std::time::{Duration, Instant};
use toyos::poller::{READABLE, WRITABLE, Poller};
use toyos::ipc;
use toyos::AsHandle;
use toyos::ipc::{Connection, IpcPayload, RxStep};
/// One line, one `write`.
///
/// **`eprintln!` is not one write.** Stderr is unbuffered by design, so
/// `write_fmt` issues a syscall per format fragment, and on this machine the
/// console and the kernel's log ring are one stream — so somebody else's whole
/// line lands inside this daemon's. `netd: ready, at most ` and
/// `init: started test-runner` arrived interleaved and the harness parsed a cap
/// out of the wrong number. `userland/soundd` has the same macro for the same
/// reason. **The class is closed now**: this daemon's output is a pipe of its
/// own to `logd`, which ends a line at its newline, so another program's line
/// cannot land inside one; what this still buys is one `write` per line, which
/// keeps a line whole against this daemon's own other threads.
/// Exported so the driver beside this file can speak in netd's own name: a line
/// from a module of this program is still this program's.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let mut line = format!($($arg)*);
        line.push('\n');
        // Refused only once logd is gone, and logd's end is the machine's own
        // record; a network stack that ended with its logger would be a second
        // outage for the same cause.
        let _ = std::io::stderr().write_all(line.as_bytes());
    }};
}

mod device;
mod dhcp;
mod egress;
mod i219;
mod mdns;
mod net;
mod report;
mod resolve;
mod stack;
mod stream;
mod virtio_net;

/// The cards this program can drive, named by what identifies one rather than
/// by the slot firmware put it in, and each with the driver that opens it. The
/// manifest row spells the same pair and the claim arrives under a label
/// composed from it, so which of these exists is `/system/bin/init`'s answer
/// and not this program's — at most one is ever endowed, and a machine with
/// none is a machine netd leaves.
///
/// `1af4:1041` is virtio's transitional device id `1000 + 1` for a network
/// device (virtio 1.2 §5.1.1). `8086:15fc` is the ThinkPad T14's onboard I219
/// at `00:1f.6`; `8086:10d3` is the 82574L, which QEMU's `e1000e` models. One
/// driver takes both, and each row names which part it is because below the
/// register file they are not one.
const CARDS: [(PciId, fn(toyos::PciDev, bool) -> Card); 3] = [
    (PciId { vendor: 0x8086, device: 0x15fc }, |c, p| Card::intel(c, Part::I219, p)),
    (PciId { vendor: 0x8086, device: 0x10d3 }, |c, p| Card::intel(c, Part::E82574, p)),
    (PciId { vendor: 0x1af4, device: 0x1041 }, Card::virtio),
];

/// The actuator that makes the card raise one interrupt on purpose, so a boot
/// whose only reading of the interrupt path is a count of messages can tell a
/// part nothing made speak from a message that reached no CPU.
///
/// **Nothing a shipped machine runs arms it**: the argument comes from the
/// `[programs.netd] args` row of a boot config, and the one config that carries
/// it is `tests/lanicscase`. A boot that always raised a message would make the
/// kernel's first-message record read the same on a working card and a dead
/// one.
const PROVOKE_MESSAGE: &str = "--provoke-message";

/// The probe under which this process brings the card up and serves exactly as
/// it always does for [`LEASE_WINDOW`], leaves what happened on the log volume
/// one durable line at a time (`report::PATH`), and then ends with
/// `toyos_i219::lease::Verdict`'s code: whether a leased address is held when
/// the window ends, and where none is, what the bring-up and the link said.
///
/// **A lease is a frame out and a frame in, answered by a server this machine
/// does not control**, which is the claim the probe is flashed for; the lines
/// beside it say what the driver and the MAC counted each way, and so which
/// half went missing on a boot that got none. Armed the same way as
/// [`PROVOKE_MESSAGE`] and never beside it.
const EXIT_WITH_LEASE: &str = "--exit-with-lease";

/// How long [`EXIT_WITH_LEASE`] serves before it ends, counted from this
/// process's start.
///
/// **It ends inside the job that holds its boot open**: `test_rs_lan_hold`
/// sleeps `toyos_tco::LEASE_BOUND_MS` from a start after this process's, so
/// the exit record and the report's last line land before the runner reboots,
/// with two seconds to spare. Every moment of it after the lease is a moment
/// the machine answers the host's ping at the leased address.
const LEASE_WINDOW: Duration = Duration::from_millis(toyos_tco::LEASE_BOUND_MS - 2_000);

/// The two, which cannot share a boot.
const ACTUATORS: [&str; 2] = [PROVOKE_MESSAGE, EXIT_WITH_LEASE];

fn armed(actuator: &str) -> bool {
    std::env::args().any(|arg| arg == actuator)
}

use toyos::endow;
use toyos::Pipe;
use toyos_abi::syscall::PciId;
use toyos_i219::lease::{Event, Verdict};
use toyos_i219::Part;
use toyos_inspect::Snapshot;
use virtio_net::VirtioNet;

use net_types::ip::{Ipv4, Ipv4Addr};
use net_types::{SpecifiedAddr, ZonedAddr};
use netstack3_core::error::LocalAddressError;
use netstack3_core::socket::{ListenerInfo, SocketInfo};
use netstack3_core::tcp::{AcceptError, BindError, ConnectError, ConnectionError, TcpSocketState};
use netstack3_core::udp::UdpRemotePort;
use packet::Buf;

use net::Net;
use resolve::UdpId;
use stack::{Inbox, SocketExtra, Shared};
use stream::{Closing, TcpId};

type PipedConnection = stream::PipedConnection<Pipe, stream::SendPipe>;

use toyos::net::*;

// --- The card ---

/// The NIC this program drives, whichever one the manifest gave it.
///
/// **One enum and not a trait object**: there are two of them, both known at
/// build time, and what a `dyn` would buy is a vtable on the frame path.
enum Card {
    Virtio(VirtioNet),
    Intel(i219::Nic),
}

impl Card {
    /// A device this driver cannot bring up is not a machine without a NIC: the
    /// claim was minted, so something the device said is not what this driver
    /// understands, and that is loud.
    fn undrivable(why: impl std::fmt::Display) -> ! {
        panic!("netd: the NIC this program was given is not one it can drive — {why}")
    }

    /// `provoke` is [`PROVOKE_MESSAGE`], carried out once the card is up.
    fn intel(claim: toyos::PciDev, part: Part, provoke: bool) -> Self {
        match i219::Nic::open(claim, part) {
            Ok(nic) => {
                if provoke {
                    nic.provoke_message();
                }
                Self::Intel(nic)
            }
            Err(why) => Self::undrivable(why),
        }
    }

    /// [`PROVOKE_MESSAGE`] is the Intel driver's: armed here it is refused, not
    /// skipped, before `open` touches the card.
    fn virtio(claim: toyos::PciDev, provoke: bool) -> Self {
        if provoke {
            panic!("netd: {PROVOKE_MESSAGE} is the Intel driver's and this card is virtio");
        }
        match VirtioNet::open(claim) {
            Ok(nic) => Self::Virtio(nic),
            Err(why) => Self::undrivable(why),
        }
    }

    fn mac(&self) -> [u8; 6] {
        match self {
            Self::Virtio(nic) => nic.mac(),
            Self::Intel(nic) => nic.mac(),
        }
    }

    /// The claim, for the poller: readable means an interrupt has landed.
    fn claim(&self) -> &toyos::PciDev {
        match self {
            Self::Virtio(nic) => nic.claim(),
            Self::Intel(nic) => nic.claim(),
        }
    }

    /// The Intel driver, for [`EXIT_WITH_LEASE`]: the bring-up it reports
    /// beside the lease is that driver's.
    fn intel_driver(&self) -> &i219::Nic {
        match self {
            Self::Virtio(_) => Self::undrivable(format_args!(
                "{EXIT_WITH_LEASE} reports the Intel driver's bring-up beside the lease, which \
                 this card has not"
            )),
            Self::Intel(nic) => nic,
        }
    }

    /// **The record has to be taken, not merely noticed.** A claim reads ready
    /// while it holds an undrained interrupt, so a pass that saw the token and
    /// left it would find the same one on the next `wait` and every one after
    /// it. What the message meant is in the rings, which the pass reads. This
    /// is also where a driver with a per-pass budget gets it back.
    ///
    /// A claim that refuses the read for anything but `WouldBlock` is the
    /// kernel saying this function is no longer this process's: a fault at the
    /// unit is the one that happens, and by the time it is answered the
    /// function's bus mastering is gone. Every frame from here on is one that
    /// silently never arrives, so this dies where it can be read — once, for
    /// whichever driver is running.
    ///
    /// Answers the link where the pass found it changed; virtio reports none.
    fn begin_pass(&self) -> Option<toyos_i219::Link> {
        let answered = match self {
            Self::Virtio(nic) => nic.take_interrupt().map(|_| None),
            Self::Intel(nic) => nic.begin_pass(),
        };
        answered
            .unwrap_or_else(|why| panic!("netd: this NIC's claim refused an interrupt read: {why:?}"))
    }

    /// Hand the next received frame to `take`, and the buffer back to the
    /// device after it; false when none waits.
    fn receive(&self, take: impl FnOnce(&[u8])) -> bool {
        match self {
            Self::Virtio(nic) => {
                let Some((index, len)) = nic.poll_rx() else { return false };
                take(nic.rx_frame(index, len));
                nic.rx_done(index);
            }
            Self::Intel(nic) => {
                let Some(frame) = nic.poll_rx() else { return false };
                take(nic.rx_frame(&frame));
                nic.rx_done(frame);
            }
        }
        true
    }

    /// What `inspect` reads about the card: which driver, its address, its
    /// link, and on the Intel parts what the driver and the MAC counted.
    ///
    /// **virtio's link is `unreported`, not `up`**: the device tells netd
    /// nothing about one, and netd serving as though it were up is netd's
    /// assumption rather than something it measured.
    fn inspect(&self, snap: &mut Snapshot) {
        let m = self.mac();
        snap.put(
            "mac",
            format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]),
        );
        let nic = match self {
            Self::Virtio(_) => {
                snap.put("driver", "virtio-net");
                snap.put("link.state", "unreported");
                return;
            }
            Self::Intel(nic) => nic,
        };
        snap.put(
            "driver",
            match nic.part() {
                Part::I219 => "i219",
                Part::E82574 => "82574",
            },
        );
        match nic.link() {
            toyos_i219::Link::Down => snap.put("link.state", "down"),
            toyos_i219::Link::Up { speed, full_duplex } => {
                snap.put("link.state", "up");
                snap.put(
                    "link.speed_mbps",
                    match speed {
                        toyos_i219::Speed::Mbps10 => 10u32,
                        toyos_i219::Speed::Mbps100 => 100,
                        toyos_i219::Speed::Mbps1000 => 1000,
                    },
                );
                snap.put("link.duplex", if full_duplex { "full" } else { "half" });
            }
        }
        let counts = nic.counts();
        snap.put("descriptors.sent", counts.sent);
        snap.put("descriptors.received", counts.received);
        snap.put("wire.sent", counts.wire.sent);
        snap.put("wire.received", counts.wire.received);
        snap.put("wire.seen", counts.wire.seen);
        snap.put("errors.missed", counts.wire.missed);
        snap.put("errors.crc", counts.wire.crc_errors);
    }

    /// Whether a frame handed to [`Card::tx`] now would reach the wire.
    ///
    /// The Intel driver answers yes whatever its ring holds, and drops a
    /// frame it has no slot for (`issues/design-debt/the-intel-driver-drops-what-its-transmit-ring-cannot-hold.md`).
    fn tx_room(&self) -> bool {
        match self {
            Self::Virtio(nic) => nic.tx_room(),
            Self::Intel(_) => true,
        }
    }

    fn tx(&self, frame: &[u8]) {
        match self {
            Self::Virtio(nic) => nic.tx(frame.len(), |slot| slot.copy_from_slice(frame)),
            Self::Intel(nic) => nic.tx(frame.len(), |slot| slot.copy_from_slice(frame)),
        }
    }

    /// Say what the driver counted, once a pass and after every frame the pass
    /// sent: a line per dropped frame is itself more frames to send.
    fn report(&self) {
        match self {
            Self::Virtio(nic) => nic.report(),
            Self::Intel(nic) => nic.report(),
        }
    }
}

/// How many frames are read before the rest of a pass runs, and the pass goes
/// on: a flood read as fast as it comes holds the connections back no longer
/// than reading this many takes. Reads are never held back otherwise: a frame
/// left unread holds every frame behind it.
const READ_A_TURN: usize = 64;

/// What the frames the stack made cost the ring, which `inspect` reports.
#[derive(Default)]
struct Transmit {
    /// Frames that waited for a transmit slot since netd started: the proof a
    /// ring was ever full.
    waited: u64,
}

impl Transmit {
    /// Hand the ring every waiting frame it has a slot for, in their turn.
    fn flush(&mut self, card: &Card, net: &mut Net) {
        while !net.bindings.egress.is_empty() && card.tx_room() {
            let frame = net.bindings.egress.pop().expect("a frame waits");
            card.tx(&frame);
        }
        self.waited += net.bindings.egress.len() as u64;
    }
}

// --- Socket tracking ---

/// What a client's socket id names.
enum SocketKind {
    TcpStream,
    TcpListener,
    Udp,
}

struct UdpSocket {
    id: UdpId,
    tx_read: Pipe,
    rx_write: Pipe,
}

struct PendingUdpRecv {
    client: Client,
    socket_id: u32,
    max_len: u32,
}

/// A piped TCP listener: netd writes 1 byte to its notify pipe for each
/// connection there is to accept.
struct PipedListener {
    id: TcpId,
    shared: std::sync::Arc<Shared>,
    notify_write: Pipe,
    notified: bool,
}

struct PendingPipedConnect {
    client: Client,
    socket_id: u32,
    id: TcpId,
    shared: std::sync::Arc<Shared>,
    /// Held from the moment the request arrived. The ends came *with* it, so
    /// there is nothing left to open when the handshake completes and nothing
    /// to fail there — where a pipe id could still be refused after netd had
    /// already told the stack to connect.
    pipes: StreamPipes,
    deadline: Option<Instant>,
}

/// A stream's two ends: [`DataPipes`], its send pipe's header mapped.
struct StreamPipes {
    to_client: Pipe,
    from_client: stream::SendPipe,
}

impl StreamPipes {
    /// Take the pair the frame just read off `client` promised, `None` where
    /// either is missing or the send pipe will not map.
    fn take(client: &Client) -> Option<Self> {
        let DataPipes { to_client, from_client } = DataPipes::take(client)?;
        Some(Self { to_client, from_client: stream::SendPipe::map(from_client)? })
    }
}

/// The two ends of a client's data path, as the client's request handed them
/// over.
///
/// A pipe end travels as itself now: the client makes both pipes, keeps the
/// ends facing itself, and moves these two. They used to be ids in the request
/// payload, which netd reopened by number — and any peer of the pipe's creator
/// could have named the same one.
struct DataPipes {
    to_client: Pipe,
    from_client: Pipe,
}

impl DataPipes {
    /// Take the pair the frame just read off `client` promised.
    fn take(client: &Client) -> Option<Self> {
        let [to_client, from_client] = client.conn.recv_handles_exact::<{ DATA_HANDLES }>()?;
        Some(Self {
            to_client: unsafe { Pipe::from_raw(to_client) },
            from_client: unsafe { Pipe::from_raw(from_client) },
        })
    }
}

// --- One request, and the client waiting for its answer ---

const RESP_RESULT: u32 = RespType::Result as u32;
const RESP_ERROR: u32 = RespType::Error as u32;

/// One client's connection, which is also how netd names it.
///
/// netd answers a connection exactly once and then lets it close, so a handler
/// owns this for as long as its operation lasts: the synchronous ones drop it
/// where they answer, and the three asynchronous ones keep it across passes
/// until what they started finishes. The handle closes with it.
struct Client {
    conn: Connection,
}

impl Client {
    fn result<T: IpcPayload>(&self, payload: &T) {
        self.answered(self.conn.try_send(RESP_RESULT, payload));
    }

    fn result_bytes(&self, data: &[u8]) {
        self.answered(self.conn.try_send_bytes(RESP_RESULT, data));
    }

    fn done(&self) {
        self.answered(self.conn.try_signal(RESP_RESULT));
    }

    fn error(&self, code: u32) {
        self.answered(self.conn.try_send(RESP_ERROR, &ErrorResponse { code }));
    }

    fn snapshot(&self, encoded: &[u8]) {
        self.answered(self.conn.try_send_bytes(toyos_inspect::MSG_SNAPSHOT, encoded));
    }

    /// Whether this client, waiting for its answer, has left. A connection
    /// carries one request, so hanging up is the one thing it may say while
    /// it waits; a client that says more is dropped by name.
    fn gone(&self) -> bool {
        let mut byte = [0u8; 1];
        match self.conn.read_nonblock(&mut byte) {
            Err(toyos_abi::syscall::SyscallError::WouldBlock) => false,
            Ok(0) | Err(_) => true,
            Ok(_) => {
                say!("netd: dropping client {} — it spoke again before its answer", self.conn.as_handle().0);
                true
            }
        }
    }

    /// **The answer goes out in one non-blocking write, and a refusal is not
    /// retried.** `ipc::send` parks in `sys_write` until the client drains,
    /// which is a client deciding when the network stack runs again; and
    /// `TrySendError::Full` can have left part of the frame in the pipe, so
    /// there is nothing here to retry either. The connection closes either way.
    /// The log is the only place the machine this runs on gets told that a
    /// client asked something and was never answered.
    fn answered(&self, sent: Result<(), ipc::TrySendError>) {
        if let Err(e) = sent {
            let why = match e {
                ipc::TrySendError::Full => {
                    "its pipe will not take the answer and it is not reading"
                }
                ipc::TrySendError::TooLarge => "the answer netd built is larger than a frame",
                ipc::TrySendError::Syscall(_) => "its connection is gone",
            };
            say!("netd: dropping client {} — {why}", self.conn.as_handle().0);
        }
    }
}

/// Poll registrations that are not piped connections: the service listener and
/// the NIC claim.
const FIXED_POLL_HANDLES: u32 = 2;

/// Connections accepted and not yet carrying a whole request.
///
/// The kernel queues 32 unaccepted connections per listener
/// (`listener::MAX_PENDING_CONNECTIONS`); this is the same allowance one step
/// further along, for a client that has been accepted and has not yet said what
/// it wants. Past it netd refuses by name rather than growing, and
/// [`HANDSHAKE_TIMEOUT`] is what guarantees the table drains.
const MAX_PENDING_CONNS: u32 = 32;

/// How long an accepted connection may go without completing its request.
///
/// Policy, and generous: every client in the tree sends its request in the
/// statement after `connect` (`toyos::net`'s `NetdConn::request`). What this
/// bounds is the one that never sends it.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// The largest request payload netd keeps.
///
/// `MsgType::DnsLookup` is the only request carrying bytes rather than a struct,
/// and `toyos::net::dns_lookup` frames a hostname into a 256-byte buffer; every
/// typed request is far smaller, `TcpConnectPipedRequest` at 32 bytes being the
/// widest. A client may declare anything up to `ipc::MAX_FRAME_LEN` — the excess
/// is counted down and discarded, never waited for.
const MAX_KEPT_REQUEST: usize = 256;

/// Registrations one piped connection can make in a batch: its tx pipe, and
/// its rx pipe while that pipe is holding bytes back.
const POLL_HANDLES_PER_PIPED: u32 = 2;

/// Registrations the lookups make in a batch: each waiting client's
/// connection, which is how netd hears it hang up.
const LOOKUP_POLL_HANDLES: u32 = resolve::MAX_LOOKUPS as u32;

/// UDP receives netd holds open at once, waiting for a datagram. Each one's
/// client connection is watched, which is how netd hears it hang up, so the
/// poller's batch carries one registration apiece; one past this is refused
/// as `ERR_RESOURCE_EXHAUSTED`.
const MAX_PENDING_RECVS: u32 = 16;

/// The UDP sockets the multicast DNS responder holds.
const MDNS_SOCKETS: usize = 1;

/// Connections a listener's queue holds, handshake done and not yet accepted.
const LISTEN_BACKLOG: NonZeroUsize = NonZeroUsize::new(16).expect("a backlog holds one");

/// Hard ceiling on live piped connections, from the poller rather than from
/// memory: netd registers every connection's pipes in the same batch as the two
/// fixed registrations, the pending connections, the lookups' clients and the
/// pending receives' clients, and `Poller::MAX_HANDLES` is the widest set one
/// poller can carry. A connect waiting for its SYN-ACK registers its client in
/// the room its slot has for two pipes. The memory budget below binds first on
/// a machine whose eighth holds fewer connections.
const MAX_PIPED_SLOTS: u64 = ((Poller::MAX_HANDLES
    - FIXED_POLL_HANDLES
    - MAX_PENDING_CONNS
    - LOOKUP_POLL_HANDLES
    - MAX_PENDING_RECVS)
    / POLL_HANDLES_PER_PIPED) as u64;

/// One client's inbound framing.
///
/// **netd never reads a client with a blocking read.** That is the whole point
/// of [`ipc::FrameRx`]: a peer that stops halfway through a frame costs a
/// buffer and a deadline instead of the event loop.
type ClientRx = ipc::FrameRx<MAX_KEPT_REQUEST>;

/// A connection that has been accepted and has not yet said what it wants.
///
/// It exists because `accept` and the request frame are two events.
struct PendingConn {
    conn: Connection,
    rx: ClientRx,
    since: Instant,
}

/// A whole request, off the connection and in memory: the read side is
/// finished before anything acts on a message, so no handler below can park
/// on the client that sent it.
struct Request {
    client: Client,
    msg_type: u32,
    payload: [u8; MAX_KEPT_REQUEST],
    payload_len: usize,
}

impl Request {
    fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len]
    }
}

/// Physical memory one piped connection costs. A kernel pipe is exactly one
/// 2 MiB page (`kernel/src/pipe.rs`: `PIPE_SIZE = PAGE_2M`) and a piped socket
/// is two of them, one per direction. The client allocates them, but netd
/// holding the far ends is what keeps them alive, so this is netd's to bound.
const PIPED_CONNECTION_BYTES: u64 = 2 * 2 * 1024 * 1024;

/// Share of physical memory netd will keep tied up in client pipes.
///
/// Policy, not derivation, and the same eighth the compositor takes for the
/// same reason: nothing in the kernel says what a process may use — no
/// per-process limit, no pressure signal, no OOM killer — so the quantity that
/// would make this derivable does not exist yet.
const PIPE_BUDGET_SHARE: u64 = 8;

/// How many piped connections netd will hold, given total physical memory.
///
/// An eighth of memory divided by the two pipes a connection costs, floored at
/// one and capped at what one poller can watch.
///
/// **A mitigation, not a policy anyone chose.** A piped connection's 4 MiB is
/// charged to nobody — no per-process limit, no pressure signal, no OOM killer
/// (`issues/isolation/`) — so without a cap a client that opens sockets
/// in a loop walks the machine into exhaustion, and netd has no way to tell
/// that from ordinary use. Delete this in favour of a kernel memory limit, not
/// in favour of a bigger number.
fn max_piped_connections(total_mem: u64) -> usize {
    let budget = total_mem / PIPE_BUDGET_SHARE;
    (budget / PIPED_CONNECTION_BYTES).clamp(1, MAX_PIPED_SLOTS) as usize
}

/// Total physical memory, as the kernel reports it.
fn total_memory() -> u64 {
    let mut buf = [0u8; toyos::system::SYSINFO_HEADER_SIZE];
    let n = toyos::system::sysinfo(&mut buf);
    assert!(n >= toyos::system::SYSINFO_HEADER_SIZE, "sysinfo returned {n} bytes");
    toyos_abi::syscall::SysinfoHeader::decode(&buf).memory_total
}

/// Where this netd's socket ids start: at random, and never 0.
///
/// **A client holds a socket id across netd being replaced** (`toyos-swap`): it
/// learns the old netd is gone when a request fails, and closing what it held
/// is its first reaction. Every netd counting from 1 made that stale number
/// another client's live socket in the new one. A random start makes two
/// instances' ranges overlap only by a chance the size of their lengths over
/// 2^32, which bounds the harm and does not remove it: an id is a number any
/// client can name (`issues/isolation/netd-socket-ids-are-ambient.md`).
fn first_socket_id() -> u32 {
    random_u32().max(1)
}

/// A word from the kernel's random source, which every DHCP transaction id is
/// drawn from too. netd ends by name if the source refuses.
fn random_u32() -> u32 {
    let mut bytes = [0u8; 4];
    toyos_abi::syscall::random(&mut bytes)
        .unwrap_or_else(|e| panic!("netd: the kernel's random source refused netd: {e:?}"));
    u32::from_le_bytes(bytes)
}

/// An IPv4 address a request named, where it names one.
fn specified(addr: [u8; 4]) -> Option<ZonedAddr<SpecifiedAddr<Ipv4Addr>, netstack3_core::device::DeviceId<stack::Bindings>>> {
    SpecifiedAddr::new(Ipv4Addr::new(addr)).map(ZonedAddr::Unzoned)
}

/// The port a bound socket holds.
fn bound_port(info: SocketInfo<Ipv4Addr, netstack3_core::device::WeakDeviceId<stack::Bindings>>) -> u16 {
    match info {
        SocketInfo::Listener(ListenerInfo { local_identifier, .. }) => local_identifier.get(),
        SocketInfo::Connected(c) => c.local_identifier.get(),
        SocketInfo::Unbound => unreachable!("netd: a socket it bound reads as unbound"),
    }
}

struct NetDaemon {
    sockets: HashMap<u32, SocketKind>,
    next_id: u32,
    pending_udp_recvs: Vec<PendingUdpRecv>,
    resolver: resolve::Resolver<Client, fn() -> u16>,
    piped_connections: Vec<PipedConnection>,
    /// Connections their clients let go of, which the stack is finishing.
    closing: Vec<Closing>,
    piped_listeners: HashMap<u32, PipedListener>,
    pending_piped_connects: Vec<PendingPipedConnect>,
    udp_sockets: HashMap<u32, UdpSocket>,
    max_piped_connections: usize,
}

impl NetDaemon {
    fn new(max_piped_connections: usize) -> Self {
        Self {
            sockets: HashMap::new(),
            next_id: first_socket_id(),
            pending_udp_recvs: Vec::new(),
            resolver: resolve::Resolver::new(Instant::now(), resolve::random_u16),
            piped_connections: Vec::new(),
            closing: Vec::new(),
            piped_listeners: HashMap::new(),
            pending_piped_connects: Vec::new(),
            udp_sockets: HashMap::new(),
            max_piped_connections,
        }
    }

    /// Is there room for one more piped connection?
    ///
    /// Counts the connects still waiting for their SYN-ACK: they each already
    /// name a pair of pipes, so leaving them out would let a burst of
    /// `TCP_CONNECT_PIPED` overshoot the cap by the whole burst.
    fn piped_room(&self) -> bool {
        self.piped_live() < self.max_piped_connections
    }

    /// Connections the cap is counting: those whose client still holds a
    /// pipe, and the connects waiting for their SYN-ACK.
    ///
    /// **A connection its client has let go of is not counted**: it holds no
    /// pipe and no watch, which are what the cap is made of, and a client
    /// that closed first would otherwise be refused for the length of every
    /// TIME-WAIT and every orphan it left behind. Those have a bound of
    /// their own, [`Self::max_closing`].
    fn piped_live(&self) -> usize {
        self.piped_connections.len() + self.pending_piped_connects.len()
    }

    /// How many closing connections netd lets the stack keep for its clients:
    /// as many as live ones. Each costs a local port and, while it owes its
    /// peer bytes, its send buffer; past this the newest are given up
    /// (`stream::past_bound`) rather than letting clients that come and go
    /// fill memory.
    fn max_closing(&self) -> usize {
        self.max_piped_connections
    }

    /// The socket table's size, as `inspect` reads it: counts, and no
    /// endpoint, because every client holding `netd` can ask.
    ///
    /// `sockets.time_wait` is every connection in TIME-WAIT, `piped.orphans`
    /// every connection whose client has gone, and `udp.waiting` every receive
    /// held for a datagram: moments no event announces to anyone but netd.
    fn inspect(&self, snap: &mut Snapshot, net: &mut Net) {
        let (mut streams, mut listeners, mut udp) = (0u32, 0u32, 0u32);
        for kind in self.sockets.values() {
            match kind {
                SocketKind::TcpStream => streams += 1,
                SocketKind::TcpListener => listeners += 1,
                SocketKind::Udp => udp += 1,
            }
        }
        let held = stream::census(net);
        let time_wait = held.values().filter(|s| **s == TcpSocketState::TimeWait).count();
        // Every socket the stack holds that no table of netd's names: one that
        // outlived its entry moves this and no other count. A connection a
        // listener holds for its owner to accept is one.
        let cookie = |id: &TcpId| id.socket_cookie().export_value();
        let tabled: std::collections::HashSet<u64> = (self.piped_connections.iter().map(|c| cookie(&c.id)))
            .chain(self.pending_piped_connects.iter().map(|c| cookie(&c.id)))
            .chain(self.piped_listeners.values().map(|l| cookie(&l.id)))
            .chain(self.closing.iter().map(|c| c.cookie))
            .collect();
        let tcp_untabled = held.keys().filter(|c| !tabled.contains(c)).count();
        let udp_held = net.api().udp::<Ipv4>().collect_all_sockets().len();
        let udp_tabled = self.udp_sockets.len() + self.resolver.sockets() + MDNS_SOCKETS;
        let udp_untabled = udp_held
            .checked_sub(udp_tabled)
            .expect("netd: a table of its names a UDP socket the stack does not hold");
        let untabled = tcp_untabled + udp_untabled;
        snap.put("sockets.untabled", untabled);
        snap.put("sockets.tcp", streams);
        snap.put("sockets.listeners", listeners);
        snap.put("sockets.udp", udp);
        snap.put("sockets.time_wait", time_wait);
        snap.put("piped.live", self.piped_live());
        snap.put("piped.max", self.max_piped_connections);
        snap.put("piped.closing", self.closing.len());
        snap.put("piped.max_closing", self.max_closing());
        let orphans = self.piped_connections.iter().filter(|c| c.orphaned_at().is_some()).count();
        snap.put("piped.orphans", orphans + self.closing.len());
        snap.put("udp.waiting", self.pending_udp_recvs.len());
        snap.put("udp.max_waiting", MAX_PENDING_RECVS);
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = match self.next_id.wrapping_add(1) {
            0 => 1,
            next => next,
        };
        id
    }

    /// Dispatch one whole request.
    ///
    /// A synchronous handler answers and lets the connection close where it
    /// stands; an asynchronous one moves the [`Client`] into its pending list
    /// and answers when what it started finishes.
    fn handle_message(&mut self, req: Request, net: &mut Net) {
        match MsgType::from_u32(req.msg_type) {
            Some(MsgType::TcpClose) => self.handle_tcp_close(&req, net),
            Some(MsgType::TcpShutdown) => self.handle_tcp_shutdown(&req),
            Some(MsgType::UdpBind) => self.handle_udp_bind(&req, net),
            Some(MsgType::UdpSendTo) => self.handle_udp_send_to(&req, net),
            Some(MsgType::UdpRecvFrom) => self.handle_udp_recv_from(req, net),
            Some(MsgType::UdpClose) => self.handle_udp_close(&req, net),
            Some(MsgType::DnsLookup) => self.handle_dns_lookup(req, net),
            Some(MsgType::TcpSetOption) => self.handle_tcp_set_option(&req, net),
            Some(MsgType::TcpGetOption) => self.handle_tcp_get_option(&req, net),
            Some(MsgType::TcpConnectPiped) => self.handle_tcp_connect_piped(req, net),
            Some(MsgType::TcpBindPiped) => self.handle_tcp_bind_piped(&req, net),
            Some(MsgType::TcpAcceptPiped) => self.handle_tcp_accept_piped(&req, net),
            None => {
                say!("netd: unknown message type {}", req.msg_type);
                req.client.error(ERR_INVALID_INPUT);
            }
        }
    }

    fn handle_tcp_close(&mut self, msg: &Request, net: &mut Net) {
        let Ok(req) = ipc::decode_payload::<SocketCloseRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        match self.sockets.remove(&req.socket_id) {
            // A connected stream is only let go of: it lives as long as its
            // pipes, and ends as they do.
            Some(SocketKind::TcpStream) => {
                if let Some(conn) = self.piped_connections.iter_mut().find(|c| c.socket_id == Some(req.socket_id)) {
                    conn.socket_id = None;
                } else if let Some(pos) = self.pending_piped_connects.iter().position(|c| c.socket_id == req.socket_id) {
                    // A connect still waiting for its SYN-ACK, named by
                    // somebody else: its own client is answered.
                    let pc = self.pending_piped_connects.swap_remove(pos);
                    net.api().tcp::<Ipv4>().close(pc.id);
                    pc.client.error(ERR_CONNECTION_REFUSED);
                } else {
                    unreachable!("netd: stream {} is neither connected nor connecting", req.socket_id);
                }
            }
            Some(SocketKind::TcpListener) => {
                let listener = self.piped_listeners.remove(&req.socket_id).expect("netd: a listener it tabled");
                net.api().tcp::<Ipv4>().close(listener.id);
            }
            Some(SocketKind::Udp) => self.close_udp(req.socket_id, net),
            None => {}
        }
        msg.client.done();
    }

    fn handle_tcp_shutdown(&mut self, msg: &Request) {
        let Ok(req) = ipc::decode_payload::<TcpShutdownRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(conn) = self.piped_connections.iter_mut().find(|c| c.socket_id == Some(req.socket_id)) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        // **Not a shutdown of the stack's here**: the bytes the client wrote
        // before asking are still in its send pipe, and a FIN asked for now
        // would go out ahead of them and end the stream short. The bridge asks
        // once the pipe is empty. A read shutdown is the client's own to keep.
        match req.how {
            0 => {}
            1 | 2 => conn.fin_after_drain = true,
            _ => {
                msg.client.error(ERR_INVALID_INPUT);
                return;
            }
        }
        msg.client.done();
    }

    fn handle_udp_bind(&mut self, msg: &Request, net: &mut Net) {
        let Ok(req) = ipc::decode_payload::<UdpBindRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(pipes) = DataPipes::take(&msg.client) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let api = net.api();
        let mut udp = api.udp::<Ipv4>();
        let id = udp.create_with(Inbox::default());
        // The unspecified address binds as no address at all: every address.
        if let Err(e) = udp.listen(&id, specified(req.addr), NonZeroU16::new(req.port)) {
            stack::removed(udp.close(id));
            msg.client.error(match e.right() {
                Some(LocalAddressError::AddressInUse | LocalAddressError::FailedToAllocateLocalPort) => ERR_ADDR_IN_USE,
                _ => ERR_INVALID_INPUT,
            });
            return;
        }
        // A datagram to the broadcast address leaves, as it always has from
        // this machine: no client can ask for `SO_BROADCAST`.
        udp.set_broadcast(&id, true);
        let bound_port = bound_port(udp.get_info(&id));
        let socket_id = self.alloc_id();
        self.sockets.insert(socket_id, SocketKind::Udp);
        self.udp_sockets.insert(socket_id, UdpSocket { id, tx_read: pipes.from_client, rx_write: pipes.to_client });
        msg.client.result(&UdpBindResponse { socket_id, bound_port, _pad: 0 });
    }

    fn handle_udp_send_to(&mut self, msg: &Request, net: &mut Net) {
        let Ok(req) = ipc::decode_payload::<UdpSendToRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(socket) = self.udp_sockets.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let mut buf = vec![0u8; req.len as usize];
        let n = match toyos_abi::syscall::read_nonblock(socket.tx_read.as_handle(), &mut buf) {
            Ok(n) => n,
            // The client writes the datagram into the pipe and *then* sends this
            // request, so an empty pipe is a client naming bytes it never put
            // there. A blocking read here waits for a second write that a
            // conforming client never makes.
            Err(toyos_abi::syscall::SyscallError::WouldBlock) => {
                msg.client.error(ERR_INVALID_INPUT);
                return;
            }
            Err(_) => {
                msg.client.error(ERR_OTHER);
                return;
            }
        };
        let (Some(to), Some(port)) = (specified(req.addr), NonZeroU16::new(req.port)) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        buf.truncate(n);
        match net.api().udp::<Ipv4>().send_to(&socket.id, Some(to), UdpRemotePort::Set(port), Buf::new(buf, ..), ()) {
            Ok(()) => msg.client.result(&(n as u32)),
            Err(e) => {
                say!("netd: a datagram to {}:{} could not leave: {e:?}", net::show(req.addr), req.port);
                msg.client.error(ERR_OTHER);
            }
        }
    }

    /// Take one waiting datagram off `socket_id` for `client`, or hand the
    /// client back when none has arrived.
    ///
    /// **A datagram goes into the client's pipe whole, or its socket ends.**
    /// The answer names a length, and a write takes as much as the pipe has
    /// room for and cannot be taken back: a client reading that length out of
    /// a pipe holding part of this datagram would splice the next one onto it.
    /// So a pipe that will not take one whole — full, gone, or not a pipe netd
    /// can write — ends the socket by name and answers its client a reset, and
    /// nothing can follow the part it did take.
    fn deliver_datagram(&mut self, client: Client, socket_id: u32, max_len: u32, net: &mut Net) -> Option<Client> {
        let Some(socket) = self.udp_sockets.get(&socket_id) else {
            client.error(ERR_NOT_CONNECTED);
            return None;
        };
        let Some(datagram) = socket.id.external_data().take() else {
            return Some(client);
        };
        // `max_len` is the client's number: a datagram longer than it is cut
        // to it, as a short receive buffer cuts one.
        let n = datagram.bytes.len().min(max_len as usize);
        let wrote = toyos_abi::syscall::write_nonblock(socket.rx_write.as_handle(), &datagram.bytes[..n]);
        if wrote == Ok(n) {
            client.result(&UdpRecvResponse { addr: datagram.from, port: datagram.port, len: n as u16 });
            return None;
        }
        match wrote {
            Ok(took) => say!("netd: ending UDP socket {socket_id} — its receive pipe took {took} of a {n}-byte datagram"),
            Err(e) => say!("netd: ending UDP socket {socket_id} — its receive pipe refused a {n}-byte datagram: {e:?}"),
        }
        self.sockets.remove(&socket_id);
        self.close_udp(socket_id, net);
        client.error(ERR_CONNECTION_RESET);
        None
    }

    fn handle_udp_recv_from(&mut self, msg: Request, net: &mut Net) {
        let Ok(req) = ipc::decode_payload::<UdpRecvFromRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Some(client) = self.deliver_datagram(msg.client, req.socket_id, req.max_len, net) {
            if self.pending_udp_recvs.len() >= MAX_PENDING_RECVS as usize {
                say!("netd: refusing a UDP receive, {MAX_PENDING_RECVS} are already waiting");
                client.error(ERR_RESOURCE_EXHAUSTED);
                return;
            }
            // Nothing has arrived yet: keep the connection open until one does.
            self.pending_udp_recvs.push(PendingUdpRecv { client, socket_id: req.socket_id, max_len: req.max_len });
        }
    }

    fn handle_udp_close(&mut self, msg: &Request, net: &mut Net) {
        let Ok(req) = ipc::decode_payload::<SocketCloseRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Some(SocketKind::Udp) = self.sockets.get(&req.socket_id) {
            self.sockets.remove(&req.socket_id);
            self.close_udp(req.socket_id, net);
        }
        msg.client.done();
    }

    fn close_udp(&mut self, socket_id: u32, net: &mut Net) {
        let socket = self.udp_sockets.remove(&socket_id).expect("netd: a UDP socket it tabled");
        stack::removed(net.api().udp::<Ipv4>().close(socket.id));
    }

    /// Start resolving the name `msg` carries, or answer at once where there
    /// is nothing to ask: an address written as one, or a name no server can
    /// be asked for.
    fn handle_dns_lookup(&mut self, msg: Request, net: &mut Net) {
        let Ok(hostname) = std::str::from_utf8(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Ok(ip) = hostname.parse::<std::net::Ipv4Addr>() {
            answer_lookup(&msg.client, &[ip.octets()]);
            return;
        }
        let Ok(name) = toyos_dns::Name::parse(hostname) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Err((client, why)) = self.resolver.start(msg.client, name, net, Instant::now()) {
            client.error(why.code());
        }
    }

    /// The stream a client names by `socket_id`, connected or connecting.
    fn tcp_id(&self, socket_id: u32) -> Option<TcpId> {
        self.piped_connections
            .iter()
            .find(|c| c.socket_id == Some(socket_id))
            .map(|c| c.id.clone())
            .or_else(|| self.pending_piped_connects.iter().find(|c| c.socket_id == socket_id).map(|c| c.id.clone()))
    }

    fn handle_tcp_set_option(&mut self, msg: &Request, net: &mut Net) {
        let Ok(req) = ipc::decode_payload::<SocketOptionRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(id) = self.tcp_id(req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        match req.option {
            OPT_NODELAY => {
                net.api().tcp::<Ipv4>().with_socket_options_mut(&id, |o| o.nagle_enabled = req.value == 0);
                msg.client.done();
            }
            _ => msg.client.error(ERR_INVALID_INPUT),
        }
    }

    fn handle_tcp_get_option(&mut self, msg: &Request, net: &mut Net) {
        let Ok(req) = ipc::decode_payload::<SocketOptionRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(id) = self.tcp_id(req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        match req.option {
            OPT_NODELAY => {
                let nagle = net.api().tcp::<Ipv4>().with_socket_options(&id, |o| o.nagle_enabled);
                msg.client.result(&SocketOptionResponse { value: u32::from(!nagle) });
            }
            _ => msg.client.error(ERR_INVALID_INPUT),
        }
    }

    // --- Piped socket handlers ---

    fn handle_tcp_connect_piped(&mut self, msg: Request, net: &mut Net) {
        let Ok(req) = ipc::decode_payload::<TcpConnectPipedRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        // Refused before the socket exists, so a refusal leaves nothing to
        // unwind and no SYN on the wire. An error return, never a panic: the
        // request is a client's and asking for one connection too many is not
        // a bug in netd.
        //
        // Not `ERR_CONNECTION_REFUSED`, which is the peer's answer: on one
        // code a client cannot tell "this machine is full, back off" from
        // "that peer says no, give up".
        if !self.piped_room() {
            say!(
                "netd: refusing connect, {} piped connections already (max {})",
                self.piped_live(),
                self.max_piped_connections,
            );
            msg.client.error(ERR_RESOURCE_EXHAUSTED);
            return;
        }
        // Taken before the socket exists, for the same reason the capacity
        // check is: a missing pair leaves nothing to unwind and no SYN on the
        // wire.
        let Some(pipes) = StreamPipes::take(&msg.client) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let (Some(remote), Some(port)) = (specified(req.addr), NonZeroU16::new(req.port)) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        // **This machine holding no address is not a peer's refusal.** Before
        // the lease there is no source for a SYN, which clears when the lease
        // lands.
        if net.address().is_none() {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        }
        let extra = SocketExtra::new();
        let shared = extra.0.clone();
        let api = net.api();
        let mut tcp = api.tcp::<Ipv4>();
        let id = tcp.create(extra);
        if let Err(e) = tcp.connect(&id, Some(remote), port) {
            tcp.close(id);
            msg.client.error(match e {
                ConnectError::NoPort => {
                    say!("netd: refusing connect, every dynamic port is held");
                    ERR_RESOURCE_EXHAUSTED
                }
                _ => ERR_CONNECTION_REFUSED,
            });
            return;
        }
        let socket_id = self.alloc_id();
        self.sockets.insert(socket_id, SocketKind::TcpStream);
        let deadline = (req.timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(req.timeout_ms.into()));
        // Async — hold the connection until the handshake completes.
        self.pending_piped_connects.push(PendingPipedConnect { client: msg.client, socket_id, id, shared, pipes, deadline });
    }

    fn handle_tcp_bind_piped(&mut self, msg: &Request, net: &mut Net) {
        let Ok(req) = ipc::decode_payload::<TcpBindPipedRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        // Take the pipe before the socket exists: a missing one then has no
        // half-built socket to unwind.
        let Some([notify]) = msg.client.conn.recv_handles_exact::<{ NOTIFY_HANDLES }>() else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let notify_write = unsafe { Pipe::from_raw(notify) };
        let extra = SocketExtra::new();
        let shared = extra.0.clone();
        let api = net.api();
        let mut tcp = api.tcp::<Ipv4>();
        let id = tcp.create(extra);
        let bound = tcp.bind(&id, None, NonZeroU16::new(req.port)).map_err(|e| match e {
            BindError::LocalAddressError(_) => ERR_ADDR_IN_USE,
            _ => ERR_INVALID_INPUT,
        });
        let listening = bound.and_then(|()| tcp.listen(&id, LISTEN_BACKLOG).map_err(|_| ERR_ADDR_IN_USE));
        if let Err(code) = listening {
            tcp.close(id);
            msg.client.error(code);
            return;
        }
        let bound_port = match tcp.get_info(&id) {
            netstack3_core::tcp::SocketInfo::Bound(b) => b.port.get(),
            other => unreachable!("netd: a socket listening reads as {other:?}"),
        };
        let socket_id = self.alloc_id();
        self.sockets.insert(socket_id, SocketKind::TcpListener);
        self.piped_listeners.insert(socket_id, PipedListener { id, shared, notify_write, notified: false });
        msg.client.result(&TcpBindResponse { socket_id, bound_port, _pad: 0 });
    }

    fn handle_tcp_accept_piped(&mut self, msg: &Request, net: &mut Net) {
        let Ok(req) = ipc::decode_payload::<TcpAcceptPipedRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if !self.piped_room() {
            say!(
                "netd: refusing accept, {} piped connections already (max {})",
                self.piped_live(),
                self.max_piped_connections,
            );
            msg.client.error(ERR_RESOURCE_EXHAUSTED);
            return;
        }
        let Some(pipes) = StreamPipes::take(&msg.client) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(listener) = self.piped_listeners.get_mut(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let (id, remote, shared) = match net.api().tcp::<Ipv4>().accept(&listener.id) {
            Ok(accepted) => accepted,
            Err(AcceptError::WouldBlock) => {
                msg.client.error(ERR_NOT_CONNECTED);
                return;
            }
            Err(e) => unreachable!("netd: a listener refused an accept: {e:?}"),
        };
        listener.notified = false;
        let local_port = match net.api().tcp::<Ipv4>().get_info(&id) {
            netstack3_core::tcp::SocketInfo::Connection(c) => c.local_addr.port.get(),
            other => unreachable!("netd: an accepted connection reads as {other:?}"),
        };
        let remote_addr = remote.ip.addr().ipv4_bytes();
        let stream_id = self.alloc_id();
        self.sockets.insert(stream_id, SocketKind::TcpStream);
        self.piped_connections.push(PipedConnection::new(id, shared, stream_id, pipes.to_client, pipes.from_client));
        msg.client.result(&TcpAcceptPipedResponse {
            socket_id: stream_id,
            remote_addr,
            remote_port: remote.port.get(),
            local_port,
        });
    }

    /// Bridge bytes between the stack's connections and the clients' pipes,
    /// both directions as far as the other side takes — when a pipe is full,
    /// bytes stay in the connection's buffer and the TCP window shrinks — and
    /// hand the stack each connection whose client is done with it.
    fn bridge_piped(&mut self, net: &mut Net, now: Instant) {
        let mut i = 0;
        while i < self.piped_connections.len() {
            let conn = &mut self.piped_connections[i];
            conn.receive(net);
            conn.send(net);
            conn.tend(now);
            if !conn.client_done() {
                i += 1;
                continue;
            }
            let conn = self.piped_connections.swap_remove(i);
            if let Some(id) = conn.socket_id {
                self.sockets.remove(&id);
            }
            if let Some(closing) = conn.finish(net, now) {
                self.closing.push(closing);
            }
        }
        if self.closing.is_empty() {
            return;
        }
        let held = stream::census(net);
        self.closing.retain(|c| held.contains_key(&c.cookie));
        let over = stream::past_bound(self.closing.iter().map(|c| c.since).enumerate(), self.max_closing());
        for &i in &over {
            say!("netd: giving up a closing connection, {} are already closing", self.max_closing());
            self.closing[i].give_up(net);
        }
        let held = stream::census(net);
        self.closing.retain(|c| held.contains_key(&c.cookie));
    }

    /// Tell each piped listener's owner about a connection it can accept, and
    /// close every listener whose notify pipe refuses netd.
    ///
    /// **One write a pass, and any refusal ends the listener.** A listener
    /// owed a wake writes its byte; the rest write zero bytes, which move
    /// nothing and are still refused by name once the owner has gone. A wake
    /// the pipe will not take is one the owner never gets, so its `accept`
    /// would wait forever on a listener netd still held; closing the listener
    /// is what tells it instead — its notify pipe reads EOF. A full pipe is
    /// that refusal too: it is an owner that has left a whole pipe of wakes
    /// unread.
    fn serve_piped_listeners(&mut self, net: &mut Net) {
        use toyos_abi::syscall::SyscallError;
        let mut dead = Vec::new();
        for (&socket_id, listener) in &mut self.piped_listeners {
            let owed = listener.shared.incoming() > 0 && !listener.notified;
            let wake: &[u8] = if owed { &[1] } else { &[] };
            match toyos_abi::syscall::write_nonblock(listener.notify_write.as_handle(), wake) {
                Ok(_) => listener.notified |= owed,
                Err(SyscallError::WouldBlock) if !owed => {}
                // Its owner has gone, which is the ordinary end of a listener.
                Err(SyscallError::Gone) => dead.push(socket_id),
                Err(e) => {
                    say!("netd: closing listener {socket_id} — its notify pipe refused netd: {e:?}");
                    dead.push(socket_id);
                }
            }
        }
        for socket_id in dead {
            let listener = self.piped_listeners.remove(&socket_id).expect("netd: a listener it found");
            self.sockets.remove(&socket_id);
            net.api().tcp::<Ipv4>().close(listener.id);
        }
    }

    /// Process pending async operations (UDP recvs, lookups, piped connects).
    fn process_pending(&mut self, net: &mut Net) {
        let now = Instant::now();

        self.let_go_of_udp(net);

        for pr in std::mem::take(&mut self.pending_udp_recvs) {
            if let Some(client) = self.deliver_datagram(pr.client, pr.socket_id, pr.max_len, net) {
                self.pending_udp_recvs.push(PendingUdpRecv { client, ..pr });
            }
        }

        for (client, name, ended) in self.resolver.pass(net, now) {
            use resolve::Ended;
            use toyos_dns::Failure;
            match ended {
                Ok(addrs) => answer_lookup(&client, &addrs),
                // The protocol's one answer for a name with no address,
                // whether the name or only its address is missing.
                Err(Ended::Failed(Failure::NoSuchName | Failure::NoAddress)) => answer_lookup(&client, &[]),
                Err(Ended::Failed(Failure::TimedOut)) => client.error(ERR_TIMED_OUT),
                Err(Ended::Failed(why @ (Failure::Truncated | Failure::ServerFailed(_) | Failure::TooManyAliases))) => {
                    say!("netd: a lookup of {name} ended without an answer: {why:?}");
                    client.error(ERR_OTHER);
                }
                Err(Ended::NoPort) => {
                    say!("netd: a lookup of {name} ended with no port left for its next query");
                    client.error(ERR_RESOURCE_EXHAUSTED);
                }
            }
        }

        // Pending piped connects.
        let mut i = 0;
        while i < self.pending_piped_connects.len() {
            let pc = &self.pending_piped_connects[i];
            if pc.shared.established() {
                let pc = self.pending_piped_connects.swap_remove(i);
                let local_port = match net.api().tcp::<Ipv4>().get_info(&pc.id) {
                    netstack3_core::tcp::SocketInfo::Connection(c) => c.local_addr.port.get(),
                    other => unreachable!("netd: an established connection reads as {other:?}"),
                };
                pc.client.result(&TcpConnectResponse { socket_id: pc.socket_id, local_port, _pad: 0 });
                self.piped_connections.push(PipedConnection::new(
                    pc.id,
                    pc.shared,
                    pc.socket_id,
                    pc.pipes.to_client,
                    pc.pipes.from_client,
                ));
                continue;
            }
            if stream::state(net, &pc.id) == TcpSocketState::Close {
                let pc = self.pending_piped_connects.swap_remove(i);
                let code = match net.api().tcp::<Ipv4>().get_socket_error(&pc.id) {
                    Some(ConnectionError::TimedOut) => ERR_TIMED_OUT,
                    _ => ERR_CONNECTION_REFUSED,
                };
                pc.client.error(code);
                self.sockets.remove(&pc.socket_id);
                net.api().tcp::<Ipv4>().close(pc.id);
                continue;
            }
            if pc.deadline.is_some_and(|d| now >= d) {
                let pc = self.pending_piped_connects.swap_remove(i);
                pc.client.error(ERR_TIMED_OUT);
                self.sockets.remove(&pc.socket_id);
                net.api().tcp::<Ipv4>().close(pc.id);
                continue;
            }
            i += 1;
        }
    }

    /// How long until a pass is owed that no event brings: a pending
    /// connect's deadline.
    fn wake_in(&self, now: Instant) -> Option<Duration> {
        self.pending_piped_connects
            .iter()
            .filter_map(|pc| pc.deadline)
            .min()
            .map(|at| at.saturating_duration_since(now))
    }

    /// The clients waiting on a lookup, a connect or a UDP receive. Each
    /// one's connection is watched: hanging up makes it readable.
    fn waiting_clients(&self) -> impl Iterator<Item = &Client> {
        let connects = self.pending_piped_connects.iter().map(|pc| &pc.client);
        let receives = self.pending_udp_recvs.iter().map(|pr| &pr.client);
        self.resolver.clients().chain(connects).chain(receives)
    }

    /// Let go of every lookup, pending connect and UDP receive whose client
    /// `left` says has hung up: its sockets and its slot are another
    /// client's, now rather than when its servers or its handshake are done
    /// with it.
    fn let_go(&mut self, net: &mut Net, left: impl Fn(&Client) -> bool) {
        self.resolver.let_go(net, &left);
        let mut i = 0;
        while i < self.pending_piped_connects.len() {
            if !left(&self.pending_piped_connects[i].client) {
                i += 1;
                continue;
            }
            let pc = self.pending_piped_connects.swap_remove(i);
            self.sockets.remove(&pc.socket_id);
            net.api().tcp::<Ipv4>().close(pc.id);
        }
        self.pending_udp_recvs.retain(|pr| !left(&pr.client));
    }

    /// Close every UDP socket whose client has gone. A zero-byte write is
    /// refused by name once the receive pipe has no reader; nothing wakes netd
    /// for that, so it is asked on every pass, and a client that leaves
    /// between passes is let go of on the next.
    fn let_go_of_udp(&mut self, net: &mut Net) {
        use toyos_abi::syscall::SyscallError;
        let gone: Vec<u32> = self
            .udp_sockets
            .iter()
            .filter(|(_, s)| toyos_abi::syscall::write_nonblock(s.rx_write.as_handle(), &[]) == Err(SyscallError::Gone))
            .map(|(&id, _)| id)
            .collect();
        for id in gone {
            self.sockets.remove(&id);
            self.close_udp(id, net);
        }
    }
}

/// The most addresses one lookup's answer carries: what
/// `toyos::net::dns_lookup`'s 256-byte buffer holds, a count byte and five
/// bytes an address. A resolver may answer with a subset of a name's
/// addresses, and these are the ones the server put first.
const MAX_ANSWERED: usize = (256 - 1) / 5;

/// A lookup's answer: a count, then each address behind the family tag 4.
fn answer_lookup(client: &Client, addrs: &[[u8; 4]]) {
    let addrs = &addrs[..addrs.len().min(MAX_ANSWERED)];
    let mut answer = vec![addrs.len() as u8];
    for addr in addrs {
        answer.push(4);
        answer.extend_from_slice(addr);
    }
    client.result_bytes(&answer);
}

/// Answer `inspect` with what this pass knows, in one non-blocking write, and
/// let the connection close as every other answer does.
///
/// Here and not in [`NetDaemon::handle_message`] because the device and the
/// lease are the loop's and not the socket table's.
fn answer_inspect(request: &Request, daemon: &NetDaemon, card: &Card, net: &mut Net, dhcp: &dhcp::Dhcp, transmit: &Transmit, passes: &Passes) {
    // The request is a bare header, and anything riding on one is not this
    // protocol.
    if request.payload_len != 0 {
        request.client.error(ERR_INVALID_INPUT);
        return;
    }
    let mut snap = Snapshot::new(toyos_inspect::NET);
    card.inspect(&mut snap);
    snap.put("tx.waited", transmit.waited);
    snap.put("tx.most_waiting", net.bindings.egress.most);
    snap.put("tx.max_waiting", egress::LIMIT);
    snap.put("tx.dropped", net.bindings.egress.dropped);
    snap.put("passes.spun", passes.spun);
    snap.put("stack.deferred", net.bindings.deferred());
    dhcp.inspect(&mut snap);
    daemon.inspect(&mut snap, net);
    let encoded = snap.encode().unwrap_or_else(|why| panic!("netd: its snapshot: {why}"));
    request.client.snapshot(&encoded);
}

/// What the loop counts about its own passes, which `inspect` reports.
#[derive(Default)]
struct Passes {
    /// Passes that asked to wait for nothing while frames waited for the ring
    /// and none was left unread: a pass then has nothing to do before the
    /// ring's interrupt or a deadline, so each is a spin.
    spun: u64,
}

const _: () = assert!(
    toyos_inspect::MAX_SNAPSHOT_BYTES == ipc::MAX_FRAME_LEN as usize,
    "a snapshot is one frame"
);

/// [`EXIT_WITH_LEASE`]'s last two lines, and the exit they announce. `held` is
/// whether a leased address is held now, at the end of the window — a lease
/// that landed and was then lost inside it is not one.
///
/// **The card is taken and dropped before the exit**, which runs no destructor:
/// dropping the driver is what lets the function go.
fn end_the_lease_probe(report: &report::Report, card: Card, held: bool) -> ! {
    let nic = card.intel_driver();
    report.say(Event::Counts(nic.counts()));
    let verdict = if held {
        Verdict::Leased
    } else {
        Verdict::NotLeased(toyos_i219::phy::Outcome::of(nic.brought_up().phy, nic.link()))
    };
    drop(card);
    let code = verdict.exit_code();
    report.say(Event::Exit { code });
    std::process::exit(code)
}

/// Write what the DHCP client decided into the stack and the resolver, and
/// say so.
fn apply_lease(change: dhcp::Change, dhcp: &dhcp::Dhcp, net: &mut Net, resolver: &mut resolve::Resolver<Client, fn() -> u16>, report: Option<&report::Report>, now: Instant) {
    let was_held = net.address().is_some();
    match change {
        dhcp::Change::Leased(lease) => {
            dhcp.say_leased(&lease, now);
            let address = net::Address { addr: lease.address, prefix: lease.prefix, router: lease.router };
            match net.apply(Some(address)) {
                Ok(()) => resolver.set_servers(&lease.dns),
                Err(net::Unusable(why)) => {
                    say!("netd: DHCP: the lease is unusable, this machine has no address: {why}");
                    resolver.set_servers(&[]);
                    return;
                }
            }
            if let Some(report) = report {
                report.say(Event::Leased {
                    address: lease.address.into(),
                    prefix: lease.prefix,
                    server: lease.server.into(),
                    router: lease.router.map(Into::into),
                });
            }
        }
        dhcp::Change::Lost => {
            say!("netd: DHCP: the lease is gone; this machine has no address");
            net.apply(None).expect("netd: clearing the address cannot be refused");
            resolver.set_servers(&[]);
            if let (Some(report), true) = (report, was_held) {
                report.say(Event::Lost);
            }
        }
    }
}

fn main() {
    let started = Instant::now();
    // **The order this used to have was load-bearing and is now moot.** The
    // `netd` port exists before either process does, a client's connection is
    // queued on it whether or not this program ever reaches `accept`, and if
    // netd exits the queued client sees `Gone` rather than silence.
    let Some((open, claim)) = CARDS
        .iter()
        .find_map(|(id, open)| endow::pci_function::<toyos::PciDev>(*id).map(|c| (*open, c)))
    else {
        say!("netd: no NIC on this machine, exiting");
        return;
    };
    let acceptor = endow::acceptor("netd")
        .expect("the manifest declares this program serves `netd`");
    // Before the card is opened, so a boot config that arms both is refused
    // before either acts.
    let asked_for: Vec<&str> = ACTUATORS.into_iter().filter(|actuator| armed(actuator)).collect();
    if asked_for.len() > 1 {
        panic!(
            "netd: {asked_for:?} cannot share a boot: a probe ends this process before the \
             point another of them acts at"
        );
    }
    let card = open(claim, armed(PROVOKE_MESSAGE));
    let report = armed(EXIT_WITH_LEASE).then(|| {
        let report = report::Report::open(started);
        let intel = card.intel_driver();
        for words in i219::brought_up_words(intel.brought_up()) {
            report.say(Event::BroughtUp(&words));
        }
        report.say(Event::Link(intel.link()));
        report
    });
    // The link as the card came up with it, which the first change a pass
    // reports is measured against. Virtio reports no link changes at all.
    let mut link_up = match &card {
        Card::Intel(intel) => intel.link().is_up(),
        Card::Virtio(_) => true,
    };
    let mac = card.mac();
    say!(
        "netd: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    let mut net = Net::new(mac, started);
    let mut dhcp = dhcp::Dhcp::new(mac, Instant::now(), random_u32);
    let mut mdns = mdns::Responder::new(dhcp::HOSTNAME, &mut net);
    let mut transmit = Transmit::default();
    // Groups the stack joined whose link addresses the card's filter passes.
    let mut accepted = 0;

    let total_mem = total_memory();
    let max_piped = max_piped_connections(total_mem);
    let mut daemon = NetDaemon::new(max_piped);

    // Sized for the slot ceiling rather than for `max_piped`: the batch
    // between two `wait` calls is the two fixed registrations, at most two per live
    // piped connection, one per pending connection, one per lookup and one per
    // pending receive, and the ceiling is what that can never exceed.
    let poller = Poller::new(
        FIXED_POLL_HANDLES
            + POLL_HANDLES_PER_PIPED * MAX_PIPED_SLOTS as u32
            + MAX_PENDING_CONNS
            + LOOKUP_POLL_HANDLES
            + MAX_PENDING_RECVS,
    );
    const TOKEN_LISTENER: u64 = 0;
    const TOKEN_NIC: u64 = 1;
    const TOKEN_TX_PIPE_BASE: u64 = 0x1000;
    const TOKEN_RX_PIPE_BASE: u64 = 0x8000;
    // Clear of the tx- and rx-pipe ranges by more than `MAX_PIPED_SLOTS`, and of a
    // connection's own handle by more than `MAX_HANDLES` (4096,
    // `kernel/src/object/handle.rs`).
    const TOKEN_PENDING_BASE: u64 = 0x1_0000;
    // Clear of the pending range by the same margin.
    const TOKEN_WAITING_BASE: u64 = 0x2_0000;

    let mut pending: Vec<PendingConn> = Vec::new();
    let mut passes = Passes::default();

    loop {
        // First, because it is what makes the interrupt taken and what gives a
        // driver with a per-pass receive budget that budget back.
        if let Some(link) = card.begin_pass() {
            if let Some(report) = &report {
                report.say(Event::Link(link));
            }
            // Down to up only, and only with no lease held: a speed change is
            // no new network, and a bound lease is kept across a flap.
            if link.is_up() && !link_up && !dhcp.leased() {
                dhcp.client.restart(Instant::now());
            }
            link_up = link.is_up();
        }
        // A turn of frames, each the DHCP client's or the stack's. A turn read
        // whole may have left frames unread, and the pass after this one is
        // owed now.
        let mut read = 0;
        let mut changes = Vec::new();
        while read < READ_A_TURN
            && card.receive(|frame| {
                if dhcp::Client::takes(frame) {
                    changes.extend(dhcp.client.on_frame(frame, Instant::now()));
                } else {
                    net.receive(frame);
                }
            })
        {
            read += 1;
        }
        let unread = read == READ_A_TURN;
        net.fire_timers();
        card.report();

        // **After the frames and before anything is served.** The lease is
        // what gives this machine an address, a route and its resolvers, so a
        // client answered before it was applied would be answered on a
        // machine that is on no network.
        let now = Instant::now();
        changes.extend(dhcp.client.on_time(now));
        for frame in dhcp.client.take_frames() {
            net.bindings.egress.push(frame);
        }
        for change in changes {
            apply_lease(change, &dhcp, &mut net, &mut daemon.resolver, report.as_ref(), now);
        }
        if dhcp.settle(now) {
            say!(
                "netd: ready, at most {max_piped} piped connections \
                 ({} MiB each of {} MiB total)",
                PIPED_CONNECTION_BYTES / (1024 * 1024),
                total_mem / (1024 * 1024),
            );
        }
        if let Card::Intel(nic) = &card {
            for group in &net.bindings.joined[accepted..] {
                nic.accept_multicast(*group);
            }
        }
        accepted = net.bindings.joined.len();

        mdns.pass(&mut net, Instant::now());
        daemon.bridge_piped(&mut net, Instant::now());
        daemon.serve_piped_listeners(&mut net);
        daemon.process_pending(&mut net);
        transmit.flush(&card, &mut net);
        let backlogged = !net.bindings.egress.is_empty();

        // The stack's own next deadline — a retransmission, a delayed ACK, a
        // TIME-WAIT's end — and every wake of netd's own. A piped connection
        // needs nothing else: its peer's bytes wake the NIC, and its client's
        // bytes and room wake the watches below. Frames waiting for the ring
        // are woken for by the ring's interrupt.
        let now = Instant::now();
        let until = |at: Instant| at.saturating_duration_since(now);
        let timeout = [
            net.next_timer().map(until),
            Some(until(dhcp.client.wake_at())),
            dhcp.settle_at().map(until),
            daemon.wake_in(now),
            mdns.wake_in(now),
            daemon.resolver.wake_in(now),
            // The probe's window is a wake of its own: an idle machine would
            // otherwise sleep through the moment it owes its answer.
            report.as_ref().map(|_| LEASE_WINDOW.saturating_sub(started.elapsed())),
            // A client that connects and then says nothing wakes nothing, so
            // the deadline that removes it has to be a wake in its own right.
            (!pending.is_empty()).then_some(HANDSHAKE_TIMEOUT),
            unread.then_some(Duration::ZERO),
        ]
        .into_iter()
        .flatten()
        .min()
        .map_or(u64::MAX, |d| d.as_nanos() as u64);
        if let Some(report) = &report {
            if started.elapsed() >= LEASE_WINDOW {
                end_the_lease_probe(report, card, dhcp.leased());
            }
        }

        poller.watch(&acceptor, READABLE, TOKEN_LISTENER);
        poller.watch(card.claim(), READABLE, TOKEN_NIC);

        // The client's bytes to send, and room in a receive pipe that is
        // holding the peer's back: either is a pass's worth of work.
        for (i, conn) in daemon.piped_connections.iter().enumerate() {
            // Only while the connection can take them: a pipe holding bytes is
            // readable until read, so its watch would complete on every pass
            // while the peer's window is shut. The ACK that makes room wakes
            // the NIC. Once the connection sends nothing more, the pipe is
            // watched for its client leaving, and the pass that reads it
            // closes it.
            if let Some(pipe) = &conn.tx_read {
                let sending = matches!(
                    stream::state(&mut net, &conn.id),
                    TcpSocketState::Established | TcpSocketState::CloseWait
                );
                if conn.send_room(&mut net) || !sending {
                    poller.watch(pipe, READABLE, TOKEN_TX_PIPE_BASE + i as u64);
                }
            }
            if let (true, Some(pipe)) = (conn.held, &conn.rx_write) {
                poller.watch(pipe, WRITABLE, TOKEN_RX_PIPE_BASE + i as u64);
            }
        }

        for p in pending.iter() {
            poller.watch(&p.conn, READABLE, TOKEN_PENDING_BASE + p.conn.as_handle().0 as u64);
        }

        // A client waiting on a lookup, a connect or a UDP receive hangs up by
        // closing its connection, which makes it readable.
        for client in daemon.waiting_clients() {
            poller.watch(&client.conn, READABLE, TOKEN_WAITING_BASE + client.conn.as_handle().0 as u64);
        }

        let mut ready: Vec<u64> = Vec::new();
        passes.spun += (timeout == 0 && backlogged && !unread) as u64;
        poller.wait(1, timeout, |token| ready.push(token));

        // A handshake that never completes is why this deadline exists, and the
        // sweep has to happen on a pass that found nothing ready too —
        // otherwise a silent client is only ever timed out by some *other*
        // client's traffic.
        let now_wall = Instant::now();
        for p in pending.iter().filter(|p| now_wall.duration_since(p.since) >= HANDSHAKE_TIMEOUT) {
            say!(
                "netd: dropping client {} — it never finished its request",
                p.conn.as_handle().0
            );
        }
        pending.retain(|p| now_wall.duration_since(p.since) < HANDSHAKE_TIMEOUT);

        // A lookup, a connect or a receive whose client has left ends now: its
        // sockets and its slot are another client's.
        let spoke = |c: &Client| ready.contains(&(TOKEN_WAITING_BASE + c.conn.as_handle().0 as u64));
        daemon.let_go(&mut net, |c| spoke(c) && c.gone());

        // Accept and the request are two events. Nothing is read here: a client
        // that connects and then says nothing costs a slot and a deadline, not
        // the network stack.
        if ready.contains(&TOKEN_LISTENER) {
            let conn = acceptor.accept().expect("accept failed");
            if pending.len() >= MAX_PENDING_CONNS as usize {
                say!(
                    "netd: refusing client {} — {MAX_PENDING_CONNS} connections are already \
                     waiting to say what they want",
                    conn.as_handle().0
                );
            } else {
                pending.push(PendingConn { conn, rx: ClientRx::new(), since: Instant::now() });
            }
        }

        // `remove` rather than `swap_remove`: the entries after `i` shift down,
        // so leaving `i` alone visits each connection exactly once. At
        // `MAX_PENDING_CONNS` entries the shift is not worth a subtler loop.
        let mut requests: Vec<Request> = Vec::new();
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
                // Unlogged, and the only removal here that is: a client may
                // connect to find out whether netd exists and hang up, which is
                // its business. The two below are the client getting something
                // wrong, and those netd names.
                RxStep::Eof => {
                    pending.remove(i);
                }
                RxStep::Malformed => {
                    say!(
                        "netd: dropping client {} — it sent a frame this protocol cannot \
                         describe",
                        pending[i].conn.as_handle().0
                    );
                    pending.remove(i);
                }
                RxStep::Frame { msg_type, payload_len } => {
                    let mut payload = [0u8; MAX_KEPT_REQUEST];
                    payload[..payload_len].copy_from_slice(pending[i].rx.payload(payload_len));
                    let p = pending.remove(i);
                    requests.push(Request {
                        client: Client { conn: p.conn },
                        msg_type,
                        payload,
                        payload_len,
                    });
                }
            }
        }

        for request in requests {
            if request.msg_type == toyos_inspect::MSG_INSPECT {
                answer_inspect(&request, &daemon, &card, &mut net, &dhcp, &transmit, &passes);
                continue;
            }
            daemon.handle_message(request, &mut net);
        }
    }
}
