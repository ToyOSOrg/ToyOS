use std::collections::HashMap;
use std::time::{Duration, Instant};
use toyos_abi::RawHandle;
use toyos::poller::{READABLE, Poller};
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
/// reason. **The class is closed at the kernel now** — a `ConsoleObject` per
/// holder buffers a line and emits it whole under one `BackendGuard` — so what
/// this still buys is one syscall per line instead of one per fragment.
/// Exported so the driver beside this file can speak in netd's own name: the
/// console's speakers are derived from the manifest, and a line from a module
/// of this program is still this program's.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let mut line = format!($($arg)*);
        line.push('\n');
        let _ = std::io::stderr().write_all(line.as_bytes());
    }};
}

mod device;
mod dhcp;
mod i219;
mod report;
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

use toyos::net::*;

use smoltcp::iface::{Config, Interface, PollResult, SocketHandle, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::{dhcpv4, dns, tcp, udp};
use smoltcp::time::Instant as SmoltcpInstant;
use smoltcp::wire::{DnsQueryType, EthernetAddress, HardwareAddress, IpAddress, IpEndpoint};

use std::net::Ipv4Addr;

// --- smoltcp Device wrapper ---

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
    /// it. What the message meant is in the rings, which `iface.poll` reads.
    /// This is also where a driver with a per-pass budget gets it back.
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

    fn tx<R>(&self, len: usize, fill: impl FnOnce(&mut [u8]) -> R) -> R {
        match self {
            Self::Virtio(nic) => nic.tx(len, fill),
            Self::Intel(nic) => nic.tx(len, fill),
        }
    }
}

/// The driver, as smoltcp's `Device`.
///
/// A thin adapter: every token below borrows the driver rather than a claim
/// handle, because the ring the token gives back to is this process's own.
struct DmaNic {
    nic: Card,
}

impl Device for DmaNic {
    type RxToken<'a> = DmaRxToken<'a>;
    type TxToken<'a> = DmaTxToken<'a>;

    fn receive(&mut self, _timestamp: SmoltcpInstant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let token = match &self.nic {
            Card::Virtio(nic) => {
                nic.poll_rx().map(|(index, len)| DmaRxToken::Virtio { nic, index, len })
            }
            Card::Intel(nic) => nic.poll_rx().map(|frame| DmaRxToken::Intel { nic, frame }),
        }?;
        Some((token, DmaTxToken { nic: &self.nic }))
    }

    fn transmit(&mut self, _timestamp: SmoltcpInstant) -> Option<Self::TxToken<'_>> {
        Some(DmaTxToken { nic: &self.nic })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1514;
        caps.medium = Medium::Ethernet;
        caps
    }
}

/// One received frame, holding the driver it came from rather than a tag that
/// says which — so a frame and a driver that do not go together is not a state
/// this program can be in.
enum DmaRxToken<'a> {
    Virtio { nic: &'a VirtioNet, index: usize, len: usize },
    Intel { nic: &'a i219::Nic, frame: toyos_i219::Frame },
}

impl phy::RxToken for DmaRxToken<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        // The borrow ends with `f`, and the buffer goes back to the device only
        // afterwards: smoltcp's `consume` takes `FnOnce(&[u8])`, so the
        // callback cannot keep the reference past its own return.
        match self {
            Self::Virtio { nic, index, len } => {
                let result = f(nic.rx_frame(index, len));
                nic.rx_done(index);
                result
            }
            Self::Intel { nic, frame } => {
                let result = f(nic.rx_frame(&frame));
                nic.rx_done(frame);
                result
            }
        }
    }
}

struct DmaTxToken<'a> {
    nic: &'a Card,
}

impl phy::TxToken for DmaTxToken<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        // Filling and sending are one call, because the buffer belongs to the
        // descriptor the driver picks: a frame written before one was taken
        // would be written into a buffer the device may still be reading.
        self.nic.tx(len, f)
    }
}

// --- Socket tracking ---

enum SocketKind {
    TcpStream(SocketHandle),
    TcpListener(SocketHandle),
    Udp(SocketHandle),
}

struct UdpPipes {
    tx_read: Pipe,
    rx_write: Pipe,
}

struct PendingUdpRecv {
    client: Client,
    socket_id: u32,
    max_len: u32,
    deadline: Option<Instant>,
}

struct PendingDns {
    client: Client,
    query: dns::QueryHandle,
}

/// A piped TCP connection: data flows through kernel pipes instead of IPC messages.
struct PipedConnection {
    handle: SocketHandle,
    rx_write: Option<Pipe>,
    tx_read: Option<Pipe>,
}

impl PipedConnection {
    fn close_rx(&mut self) {
        self.rx_write.take();
    }

    fn close_tx(&mut self) {
        self.tx_read.take();
    }

    fn close_all(&mut self) {
        self.close_rx();
        self.close_tx();
    }

    fn is_fully_closed(&self) -> bool {
        self.rx_write.is_none() && self.tx_read.is_none()
    }
}

/// A piped TCP listener: netd writes 1 byte to notify pipe on new connection.
struct PipedListener {
    handle: SocketHandle,
    notify_write: Pipe,
    notified: bool,
}

struct PendingPipedConnect {
    client: Client,
    socket_id: u32,
    handle: SocketHandle,
    /// Held from the moment the request arrived. The ends came *with* it, so
    /// there is nothing left to open when the handshake completes and nothing
    /// to fail there — where a pipe id could still be refused after netd had
    /// already told smoltcp to connect.
    pipes: DataPipes,
    deadline: Option<Instant>,
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

fn piped_connection(handle: SocketHandle, pipes: DataPipes) -> PipedConnection {
    PipedConnection {
        handle,
        rx_write: Some(pipes.to_client),
        tx_read: Some(pipes.from_client),
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
/// until what they started finishes. The handle closes with it — which is what
/// replaced a `mem::forget` on the accepted connection and eight hand-written
/// `close` calls that had to agree with each other on every path.
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

/// Hard ceiling on live piped connections, from the poller rather than from
/// memory: netd registers every tx pipe in the same batch as the two fixed
/// registrations
/// and the pending connections, and `Poller::MAX_HANDLES` is the widest set one
/// poller can carry. The memory budget below is what binds on any machine with
/// less than 8 GiB.
const MAX_PIPED_SLOTS: u64 = (Poller::MAX_HANDLES - FIXED_POLL_HANDLES - MAX_PENDING_CONNS) as u64;

/// One client's inbound framing.
///
/// **netd never reads a client with a blocking read.** That is the whole point
/// of [`ipc::FrameRx`]: `ipc::recv_header` and `ipc::recv_payload` park the
/// caller until the peer sends the bytes it promised, and netd used to call
/// both — so one client that connected and wrote four bytes stopped the network
/// stack for everyone until it disconnected. Here a peer that stops halfway
/// through a frame costs a buffer and a deadline instead of the event loop.
type ClientRx = ipc::FrameRx<MAX_KEPT_REQUEST>;

/// A connection that has been accepted and has not yet said what it wants.
///
/// It exists because `accept` and the request frame are two events, and netd
/// used to fuse them with a blocking `recv_header` on the fresh connection.
struct PendingConn {
    conn: Connection,
    rx: ClientRx,
    since: Instant,
}

/// A whole request, off the connection and in memory.
///
/// The payload travels with the frame instead of being read off the connection
/// during
/// dispatch: the read side is finished before anything acts on a message, so no
/// handler below can park on the client that sent it.
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

/// Payload bytes a UDP socket's receive buffer holds, and therefore the longest
/// datagram netd can ever hand back — which is what bounds the buffer
/// [`NetDaemon::deliver_datagram`] sizes from a client's `max_len`.
const UDP_SOCKET_BUFFER: usize = 65536;

/// Payload bytes each direction of a TCP socket buffers inside netd, before the
/// window closes and the peer is asked to wait.
const TCP_SOCKET_BUFFER: usize = 65536;

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
    let mut bytes = [0u8; 4];
    toyos_abi::syscall::random(&mut bytes);
    u32::from_le_bytes(bytes).max(1)
}

struct NetDaemon {
    sockets: HashMap<u32, SocketKind>,
    next_id: u32,
    next_local_port: u16,
    pending_udp_recvs: Vec<PendingUdpRecv>,
    pending_dns: Vec<PendingDns>,
    dns_handle: SocketHandle,
    piped_connections: Vec<PipedConnection>,
    piped_listeners: HashMap<u32, PipedListener>,
    pending_piped_connects: Vec<PendingPipedConnect>,
    udp_pipes: HashMap<u32, UdpPipes>,
    max_piped_connections: usize,
}

impl NetDaemon {
    fn new(dns_handle: SocketHandle, max_piped_connections: usize) -> Self {
        Self {
            sockets: HashMap::new(),
            next_id: first_socket_id(),
            next_local_port: 49152,
            pending_udp_recvs: Vec::new(),
            pending_dns: Vec::new(),
            dns_handle,
            piped_connections: Vec::new(),
            piped_listeners: HashMap::new(),
            pending_piped_connects: Vec::new(),
            udp_pipes: HashMap::new(),
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

    /// Connections the cap is counting. Reported by both refusals, because
    /// `piped_connections.len()` alone reads as "0 already, max 126" when a
    /// burst of connects fills the pending list — a refusal that looks like a
    /// bug in the check rather than the check working.
    fn piped_live(&self) -> usize {
        self.piped_connections.len() + self.pending_piped_connects.len()
    }

    /// The socket table's size, as `inspect` reads it: counts, and no
    /// endpoint, because every client holding `netd` can ask.
    fn inspect(&self, snap: &mut Snapshot) {
        let (mut streams, mut listeners, mut udp) = (0u32, 0u32, 0u32);
        for kind in self.sockets.values() {
            match kind {
                SocketKind::TcpStream(_) => streams += 1,
                SocketKind::TcpListener(_) => listeners += 1,
                SocketKind::Udp(_) => udp += 1,
            }
        }
        snap.put("sockets.tcp", streams);
        snap.put("sockets.listeners", listeners);
        snap.put("sockets.udp", udp);
        snap.put("piped.live", self.piped_live());
        snap.put("piped.max", self.max_piped_connections);
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = match self.next_id.wrapping_add(1) {
            0 => 1,
            next => next,
        };
        id
    }

    fn alloc_port(&mut self) -> u16 {
        let port = self.next_local_port;
        self.next_local_port = if self.next_local_port >= 65535 { 49152 } else { self.next_local_port + 1 };
        port
    }

    /// Dispatch one whole request.
    ///
    /// A synchronous handler answers and lets the connection close where it
    /// stands; an asynchronous one moves the [`Client`] into its pending list
    /// and answers when what it started finishes.
    fn handle_message(
        &mut self,
        req: Request,
        socket_set: &mut SocketSet<'_>,
        iface: &mut Interface,
    ) {
        match MsgType::from_u32(req.msg_type) {
            Some(MsgType::TcpClose) => self.handle_tcp_close(&req, socket_set),
            Some(MsgType::TcpShutdown) => self.handle_tcp_shutdown(&req, socket_set),
            Some(MsgType::UdpBind) => self.handle_udp_bind(&req, socket_set),
            Some(MsgType::UdpSendTo) => self.handle_udp_send_to(&req, socket_set),
            Some(MsgType::UdpRecvFrom) => self.handle_udp_recv_from(req, socket_set),
            Some(MsgType::UdpClose) => self.handle_udp_close(&req, socket_set),
            Some(MsgType::DnsLookup) => self.handle_dns_lookup(req, socket_set, iface),
            Some(MsgType::TcpSetOption) => self.handle_tcp_set_option(&req, socket_set),
            Some(MsgType::TcpGetOption) => self.handle_tcp_get_option(&req, socket_set),
            Some(MsgType::TcpConnectPiped) => self.handle_tcp_connect_piped(req, socket_set, iface),
            Some(MsgType::TcpBindPiped) => self.handle_tcp_bind_piped(&req, socket_set),
            Some(MsgType::TcpAcceptPiped) => self.handle_tcp_accept_piped(&req, socket_set),
            None => {
                say!("netd: unknown message type {}", req.msg_type);
                req.client.error(ERR_INVALID_INPUT);
            }
        }
    }

    fn handle_tcp_close(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<SocketCloseRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Some(kind) = self.sockets.remove(&req.socket_id) {
            match kind {
                SocketKind::TcpStream(handle) => {
                    socket_set.get_mut::<tcp::Socket>(handle).close();
                    socket_set.remove(handle);
                    if let Some(pos) = self.piped_connections.iter().position(|c| c.handle == handle) {
                        self.piped_connections.swap_remove(pos).close_all();
                    }
                    // A connect still waiting for its SYN-ACK names the
                    // handle just removed, and the pass that would read it
                    // next is a panic; its client is answered instead.
                    if let Some(pos) =
                        self.pending_piped_connects.iter().position(|c| c.handle == handle)
                    {
                        self.pending_piped_connects.swap_remove(pos).client.error(ERR_CONNECTION_REFUSED);
                    }
                }
                SocketKind::TcpListener(handle) => {
                    socket_set.get_mut::<tcp::Socket>(handle).abort();
                    socket_set.remove(handle);
                    self.piped_listeners.remove(&req.socket_id);
                }
                SocketKind::Udp(handle) => {
                    socket_set.get_mut::<udp::Socket>(handle).close();
                    socket_set.remove(handle);
                    self.udp_pipes.remove(&req.socket_id);
                }
            }
        }
        msg.client.done();
    }

    fn handle_tcp_shutdown(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<TcpShutdownRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(SocketKind::TcpStream(handle)) = self.sockets.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let socket = socket_set.get_mut::<tcp::Socket>(*handle);
        if req.how == 1 || req.how == 2 {
            socket.close();
        }
        msg.client.done();
    }

    fn handle_udp_bind(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<UdpBindRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let port = if req.port == 0 { self.alloc_port() } else { req.port };

        let Some(pipes) = DataPipes::take(&msg.client) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let (rx_write, tx_read) = (pipes.to_client, pipes.from_client);

        let rx_buf = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; 16],
            vec![0u8; UDP_SOCKET_BUFFER],
        );
        let tx_buf = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; 16],
            vec![0u8; UDP_SOCKET_BUFFER],
        );
        let mut socket = udp::Socket::new(rx_buf, tx_buf);
        let endpoint = IpEndpoint::new(IpAddress::Ipv4(Ipv4Addr::from(req.addr)), port);
        if socket.bind(endpoint).is_err() {
            msg.client.error(ERR_ADDR_IN_USE);
            return;
        }

        let handle = socket_set.add(socket);
        let socket_id = self.alloc_id();
        self.sockets.insert(socket_id, SocketKind::Udp(handle));
        self.udp_pipes.insert(socket_id, UdpPipes { tx_read, rx_write });

        msg.client.result(&UdpBindResponse {
            socket_id,
            bound_port: port,
            _pad: 0,
        });
    }

    fn handle_udp_send_to(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<UdpSendToRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };

        let Some(SocketKind::Udp(handle)) = self.sockets.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let handle = *handle;

        let Some(pipes) = self.udp_pipes.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };

        let mut buf = vec![0u8; req.len as usize];
        let n = match toyos_abi::syscall::read_nonblock(pipes.tx_read.as_handle(), &mut buf) {
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

        let addr = Ipv4Addr::from(req.addr);
        let endpoint = IpEndpoint::new(IpAddress::Ipv4(addr), req.port);
        let socket = socket_set.get_mut::<udp::Socket>(handle);
        match socket.send_slice(&buf[..n], endpoint) {
            Ok(()) => msg.client.result(&(n as u32)),
            Err(_) => msg.client.error(ERR_OTHER),
        }
    }

    /// Hand one waiting datagram to a client, or report that none has arrived.
    ///
    /// The write into the client's own receive pipe is non-blocking, and a short
    /// one is refused rather than reported: the response names a length, and a
    /// client reading that many bytes out of a pipe holding fewer would splice
    /// the next datagram onto this one. A client that is not draining its
    /// receive pipe loses the datagram — which is what UDP does under pressure —
    /// and is told so rather than being lied to.
    fn deliver_datagram(
        client: &Client,
        socket: &mut udp::Socket,
        max_len: u32,
        rx_write: RawHandle,
    ) -> bool {
        if !socket.can_recv() {
            return false;
        }
        // `max_len` is the client's number. Clamped rather than trusted: the
        // socket's own receive buffer is 65536 bytes, so no datagram it can hand
        // back is longer, and an unclamped `vec!` here is a 4 GiB allocation any
        // client can ask netd to make.
        let mut buf = vec![0u8; (max_len as usize).min(UDP_SOCKET_BUFFER)];
        match socket.recv_slice(&mut buf) {
            Ok((n, endpoint)) => {
                let addr = match endpoint.endpoint.addr {
                    IpAddress::Ipv4(a) => a.octets(),
                };
                match toyos_abi::syscall::write_nonblock(rx_write, &buf[..n]) {
                    Ok(written) if written == n => client.result(&UdpRecvResponse {
                        addr,
                        port: endpoint.endpoint.port,
                        len: n as u16,
                    }),
                    _ => client.error(ERR_RESOURCE_EXHAUSTED),
                }
            }
            Err(_) => client.error(ERR_OTHER),
        }
        true
    }

    fn handle_udp_recv_from(&mut self, msg: Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<UdpRecvFromRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(SocketKind::Udp(handle)) = self.sockets.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let handle = *handle;
        let Some(pipes) = self.udp_pipes.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let rx_write = pipes.rx_write.as_handle();
        let socket = socket_set.get_mut::<udp::Socket>(handle);

        if Self::deliver_datagram(&msg.client, socket, req.max_len, rx_write) {
            return;
        }

        // Nothing has arrived yet: keep the connection open until one does.
        self.pending_udp_recvs.push(PendingUdpRecv {
            client: msg.client,
            socket_id: req.socket_id,
            max_len: req.max_len,
            deadline: None,
        });
    }

    fn handle_udp_close(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<SocketCloseRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Some(SocketKind::Udp(handle)) = self.sockets.remove(&req.socket_id) {
            socket_set.get_mut::<udp::Socket>(handle).close();
            socket_set.remove(handle);
            self.udp_pipes.remove(&req.socket_id);
        }
        msg.client.done();
    }

    fn handle_dns_lookup(
        &mut self,
        msg: Request,
        socket_set: &mut SocketSet<'_>,
        iface: &mut Interface,
    ) {
        let Ok(hostname) = std::str::from_utf8(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };

        if let Ok(ip) = hostname.parse::<std::net::Ipv4Addr>() {
            let octets = ip.octets();
            let mut resp = vec![1u8];
            resp.push(4);
            resp.extend_from_slice(&octets);
            msg.client.result_bytes(&resp);
            return;
        }

        let dns = socket_set.get_mut::<dns::Socket>(self.dns_handle);
        match dns.start_query(iface.context(), hostname, DnsQueryType::A) {
            // Async — hold the connection until the query resolves.
            Ok(query) => self.pending_dns.push(PendingDns { client: msg.client, query }),
            Err(_) => msg.client.error(ERR_OTHER),
        }
    }

    fn handle_tcp_set_option(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<SocketOptionRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(SocketKind::TcpStream(handle)) = self.sockets.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let socket = socket_set.get_mut::<tcp::Socket>(*handle);
        match req.option {
            OPT_NODELAY => {
                socket.set_nagle_enabled(req.value == 0);
                msg.client.done();
            }
            _ => msg.client.error(ERR_INVALID_INPUT),
        }
    }

    fn handle_tcp_get_option(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<SocketOptionRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(SocketKind::TcpStream(handle)) = self.sockets.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let socket = socket_set.get_mut::<tcp::Socket>(*handle);
        match req.option {
            OPT_NODELAY => {
                let val = if socket.nagle_enabled() { 0u32 } else { 1u32 };
                msg.client.result(&SocketOptionResponse { value: val });
            }
            _ => msg.client.error(ERR_INVALID_INPUT),
        }
    }

    // --- Piped socket handlers ---

    fn handle_tcp_connect_piped(
        &mut self,
        msg: Request,
        socket_set: &mut SocketSet<'_>,
        iface: &mut Interface,
    ) {
        let Ok(req) = ipc::decode_payload::<TcpConnectPipedRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        // Refused before the socket exists, so a refusal leaves nothing to
        // unwind and no SYN on the wire. An error return, never a panic: the
        // request is a client's and asking for one connection too many is not
        // a bug in netd.
        //
        // Not `ERR_CONNECTION_REFUSED`, which this file already uses below for
        // a pending connect whose socket reached `Closed` — the peer's answer.
        // On one code a client cannot tell "this machine is full, back off"
        // from "that peer says no, give up".
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
        let Some(pipes) = DataPipes::take(&msg.client) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let remote = IpEndpoint::new(
            IpAddress::Ipv4(Ipv4Addr::from(req.addr)),
            req.port,
        );
        if req.port == 0 || remote.addr.is_unspecified() {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        }
        // **This machine holding no address is not a peer's refusal.** Before
        // the lease there is no source for a SYN, and the socket's own
        // `Unaddressable` would reach the client as `ERR_CONNECTION_REFUSED`,
        // which says "that peer says no, give up" about a condition of this
        // machine that clears when the lease lands.
        if iface.ipv4_addr().is_none() {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        }
        let local_port = self.alloc_port();

        let rx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let tx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let mut socket = tcp::Socket::new(rx_buf, tx_buf);
        if socket.connect(iface.context(), remote, local_port).is_err() {
            msg.client.error(ERR_CONNECTION_REFUSED);
            return;
        }

        let handle = socket_set.add(socket);
        let socket_id = self.alloc_id();
        self.sockets.insert(socket_id, SocketKind::TcpStream(handle));

        let deadline = if req.timeout_ms > 0 {
            Some(Instant::now() + Duration::from_millis(req.timeout_ms as u64))
        } else {
            None
        };

        // Async — hold the connection until the handshake completes.
        self.pending_piped_connects.push(PendingPipedConnect {
            client: msg.client,
            socket_id,
            handle,
            pipes,
            deadline,
        });
    }

    fn handle_tcp_bind_piped(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<TcpBindPipedRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let port = if req.port == 0 { self.alloc_port() } else { req.port };

        // Take the pipe before the socket goes into socket_set: a missing one
        // then has no half-built socket to unwind.
        let Some([notify]) = msg.client.conn.recv_handles_exact::<{ NOTIFY_HANDLES }>() else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let notify_write = unsafe { Pipe::from_raw(notify) };

        let rx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let tx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let mut socket = tcp::Socket::new(rx_buf, tx_buf);
        if socket.listen(port).is_err() {
            msg.client.error(ERR_ADDR_IN_USE);
            return;
        }

        let handle = socket_set.add(socket);
        let socket_id = self.alloc_id();
        self.sockets.insert(socket_id, SocketKind::TcpListener(handle));

        self.piped_listeners.insert(socket_id, PipedListener {
            handle,
            notify_write,
            notified: false,
        });

        msg.client.result(&TcpBindResponse {
            socket_id,
            bound_port: port,
            _pad: 0,
        });
    }

    fn handle_tcp_accept_piped(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
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
        let Some(pipes) = DataPipes::take(&msg.client) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(listener) = self.piped_listeners.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };

        let socket = socket_set.get_mut::<tcp::Socket>(listener.handle);
        // Not `is_active`: that is already true in SynReceived, where
        // `remote_endpoint()` is still None.
        if socket.state() != tcp::State::Established {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        }

        let remote = socket.remote_endpoint().unwrap();
        let local_port = socket.local_endpoint().unwrap().port;
        let remote_addr = match remote.addr {
            IpAddress::Ipv4(a) => a.octets(),
        };

        let old_handle = listener.handle;
        let stream_id = self.alloc_id();
        self.sockets.insert(stream_id, SocketKind::TcpStream(old_handle));

        self.piped_connections.push(piped_connection(old_handle, pipes));

        // Create replacement listener
        let rx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let tx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let mut new_listener = tcp::Socket::new(rx_buf, tx_buf);
        new_listener.listen(local_port).ok();
        let new_handle = socket_set.add(new_listener);
        self.sockets.insert(req.socket_id, SocketKind::TcpListener(new_handle));

        if let Some(pl) = self.piped_listeners.get_mut(&req.socket_id) {
            pl.handle = new_handle;
            pl.notified = false;
        }

        msg.client.result(&TcpAcceptPipedResponse {
            socket_id: stream_id,
            remote_addr,
            remote_port: remote.port,
            local_port,
        });
    }

    /// Bridge data between smoltcp sockets and kernel pipes for piped connections.
    /// Drains fully in both directions — when the ring is full, data stays in smoltcp's
    /// buffer and TCP window shrinks (correct backpressure).
    fn bridge_piped(&mut self, socket_set: &mut SocketSet<'_>) {
        let mut closed = Vec::new();
        for i in 0..self.piped_connections.len() {
            let conn = &mut self.piped_connections[i];
            let socket = socket_set.get_mut::<tcp::Socket>(conn.handle);

            // smoltcp rx → pipe write via kernel (ensures reader notification)
            while socket.can_recv() {
                if let Some(ref pipe) = conn.rx_write {
                    let mut buf = [0u8; 4096];
                    match socket.recv_slice(&mut buf) {
                        Ok(n) if n > 0 => {
                            let _ = toyos_abi::syscall::write_nonblock(pipe.as_handle(), &buf[..n]);
                        }
                        _ => break,
                    };
                } else {
                    break;
                }
            }

            // pipe read → smoltcp tx. Ok(0) is the kernel's EOF — ring drained,
            // no writer — which says the client stopped writing; not the
            // forgeable closed flags.
            while socket.can_send() {
                if let Some(ref pipe) = conn.tx_read {
                    // **No more is taken out of the pipe than the socket will
                    // take from us.** `send_slice` answers how many bytes it
                    // enqueued and takes fewer when the send buffer is short of
                    // room; bytes read past that are gone, and the peer's stream
                    // is short in the middle with nothing saying so. The pipe is
                    // where the rest belongs until there is room.
                    let mut buf = [0u8; 4096];
                    let want = (socket.send_capacity() - socket.send_queue()).min(buf.len());
                    // A zero-length read answers `Ok(0)`, which the arm below
                    // reads as the client hanging up; asked for no bytes, this
                    // loop has nothing to do instead.
                    if want == 0 {
                        break;
                    }
                    match toyos_abi::syscall::read_nonblock(pipe.as_handle(), &mut buf[..want]) {
                        Ok(0) => {
                            socket.close();
                            conn.close_tx();
                            break;
                        }
                        Ok(n) => {
                            // Both refusals are bytes the pipe has already given
                            // up, so neither may be swallowed here of all places.
                            let sent = socket.send_slice(&buf[..n]).unwrap_or_else(|e| {
                                panic!("netd: a socket that could send refused {n} byte(s): {e:?}")
                            });
                            assert_eq!(sent, n, "netd: the send buffer took {sent} of {n} byte(s) it had room for");
                        }
                        _ => break,
                    }
                } else {
                    break;
                }
            }

            // Signal EOF to client when remote has closed and all data is drained
            if !socket.may_recv() && !socket.can_recv() && conn.rx_write.is_some() {
                conn.close_rx();
            }

            // **A connection that is over takes no more of the client's bytes.**
            // A peer's reset leaves the socket `Closed` and `can_send` false for
            // good, so the loop above never reads the pipe again; left open, the
            // client's writes fill a pipe nobody drains and then block, and a
            // writer that is never told its peer is gone cannot say so.
            if !socket.is_open() && conn.tx_read.is_some() {
                conn.close_tx();
            }

            // Detect client death: a zero-byte write is refused by name once
            // the pipe has no reader — the kernel's fact, not the client's.
            if let Some(ref pipe) = conn.rx_write {
                if toyos_abi::syscall::write_nonblock(pipe.as_handle(), &[])
                    == Err(toyos_abi::syscall::SyscallError::Gone)
                {
                    conn.close_rx();
                }
            }

            // Fully clean up when both sides are done
            if conn.is_fully_closed() && !socket.is_open() {
                closed.push(i);
            }
        }

        for &i in closed.iter().rev() {
            self.piped_connections.swap_remove(i);
        }
    }

    /// Check piped listeners for new established connections and notify via pipe.
    fn check_piped_listeners(&mut self, socket_set: &mut SocketSet<'_>) {
        for (_, listener) in &mut self.piped_listeners {
            let socket = socket_set.get_mut::<tcp::Socket>(listener.handle);
            // Not `is_active`: that is already true in SynReceived, before the
            // three-way handshake completes.
            if socket.state() == tcp::State::Established && !listener.notified {
                let _ = toyos_abi::syscall::write_nonblock(listener.notify_write.as_handle(), &[1]);
                listener.notified = true;
            }
        }
    }

    /// Detect dead piped listeners (owning process died, notify pipe reader closed).
    fn cleanup_dead_listeners(&mut self, socket_set: &mut SocketSet<'_>) {
        let mut dead = Vec::new();
        for (&socket_id, listener) in &self.piped_listeners {
            // As in `bridge_piped`: the kernel refuses a reader-less pipe by
            // name, and a zero-byte probe moves nothing.
            if toyos_abi::syscall::write_nonblock(listener.notify_write.as_handle(), &[])
                == Err(toyos_abi::syscall::SyscallError::Gone)
            {
                dead.push(socket_id);
            }
        }
        for socket_id in dead {
            if let Some(_listener) = self.piped_listeners.remove(&socket_id) {
                if let Some(kind) = self.sockets.remove(&socket_id) {
                    if let SocketKind::TcpListener(handle) = kind {
                        socket_set.get_mut::<tcp::Socket>(handle).abort();
                        socket_set.remove(handle);
                    }
                }
            }
        }
    }

    /// Process pending async operations (UDP recvs, DNS, piped connects).
    fn process_pending(&mut self, socket_set: &mut SocketSet<'_>) {
        let now = Instant::now();

        // Pending UDP recvs
        let mut i = 0;
        while i < self.pending_udp_recvs.len() {
            let pr = &self.pending_udp_recvs[i];
            let Some(SocketKind::Udp(handle)) = self.sockets.get(&pr.socket_id) else {
                pr.client.error(ERR_NOT_CONNECTED);
                self.pending_udp_recvs.swap_remove(i);
                continue;
            };
            let handle = *handle;
            let Some(pipes) = self.udp_pipes.get(&pr.socket_id) else {
                pr.client.error(ERR_NOT_CONNECTED);
                self.pending_udp_recvs.swap_remove(i);
                continue;
            };
            let rx_write = pipes.rx_write.as_handle();
            let max_len = pr.max_len;
            let socket = socket_set.get_mut::<udp::Socket>(handle);
            if Self::deliver_datagram(&self.pending_udp_recvs[i].client, socket, max_len, rx_write)
            {
                self.pending_udp_recvs.swap_remove(i);
                continue;
            }
            if self.pending_udp_recvs[i].deadline.is_some_and(|d| now >= d) {
                self.pending_udp_recvs[i].client.error(ERR_TIMED_OUT);
                self.pending_udp_recvs.swap_remove(i);
                continue;
            }
            i += 1;
        }

        // Pending DNS queries
        let mut i = 0;
        while i < self.pending_dns.len() {
            let pd = &self.pending_dns[i];
            let dns = socket_set.get_mut::<dns::Socket>(self.dns_handle);
            match dns.get_query_result(pd.query) {
                Ok(addrs) => {
                    let mut resp = Vec::new();
                    resp.push(addrs.len() as u8);
                    for addr in addrs.iter() {
                        match addr {
                            IpAddress::Ipv4(a) => {
                                resp.push(4);
                                resp.extend_from_slice(&a.octets());
                            }
                        }
                    }
                    pd.client.result_bytes(&resp);
                    self.pending_dns.swap_remove(i);
                    continue;
                }
                Err(dns::GetQueryResultError::Pending) => {
                    i += 1;
                    continue;
                }
                Err(_) => {
                    pd.client.error(ERR_OTHER);
                    self.pending_dns.swap_remove(i);
                    continue;
                }
            }
        }

        // Pending piped connects
        let mut i = 0;
        while i < self.pending_piped_connects.len() {
            let pc = &self.pending_piped_connects[i];
            let socket = socket_set.get_mut::<tcp::Socket>(pc.handle);
            if socket.may_send() {
                let local_port = socket.local_endpoint().map(|e| e.port).unwrap_or(0);
                let resp = TcpConnectResponse {
                    socket_id: pc.socket_id,
                    local_port,
                    _pad: 0,
                };
                pc.client.result(&resp);
                let pc = self.pending_piped_connects.swap_remove(i);
                self.piped_connections.push(piped_connection(pc.handle, pc.pipes));
                continue;
            }
            if socket.state() == tcp::State::Closed {
                pc.client.error(ERR_CONNECTION_REFUSED);
                let (socket_id, handle) = (pc.socket_id, pc.handle);
                self.sockets.remove(&socket_id);
                socket_set.remove(handle);
                self.pending_piped_connects.swap_remove(i);
                continue;
            }
            if pc.deadline.is_some_and(|d| now >= d) {
                pc.client.error(ERR_TIMED_OUT);
                socket.abort();
                let (socket_id, handle) = (pc.socket_id, pc.handle);
                self.sockets.remove(&socket_id);
                socket_set.remove(handle);
                self.pending_piped_connects.swap_remove(i);
                continue;
            }
            i += 1;
        }
    }
}

/// Answer `inspect` with what this pass knows, in one non-blocking write, and
/// let the connection close as every other answer does.
///
/// Here and not in [`NetDaemon::handle_message`] because the card and the
/// lease are the loop's and not the socket table's.
fn answer_inspect(request: &Request, daemon: &NetDaemon, card: &Card, dhcp: &dhcp::Dhcp) {
    // The request is a bare header, and anything riding on one is not this
    // protocol.
    if request.payload_len != 0 {
        request.client.error(ERR_INVALID_INPUT);
        return;
    }
    let mut snap = Snapshot::new(toyos_inspect::NET);
    card.inspect(&mut snap);
    dhcp.inspect(&mut snap);
    daemon.inspect(&mut snap);
    let encoded = snap.encode().unwrap_or_else(|why| panic!("netd: its snapshot: {why}"));
    request.client.snapshot(&encoded);
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

fn main() {
    let started = Instant::now();
    // **The order this used to have was load-bearing and is now moot.** The
    // device was claimed before the name was published, because a client that
    // connected while netd was still in `DmaNic::open` reached a listener owned
    // by a process about to return and got its request answered by nobody —
    // sshd found it, took its `panic!` arm and put a tokio backtrace across the
    // boot. There is no window left to order around: the `netd` port exists
    // before either process does, a client's connection is queued on it whether
    // or not this program ever reaches `accept`, and if netd exits the queued
    // client sees `Gone` rather than silence.
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
    let nic = open(claim, armed(PROVOKE_MESSAGE));
    let report = armed(EXIT_WITH_LEASE).then(|| {
        let report = report::Report::open(started);
        let intel = nic.intel_driver();
        for words in i219::brought_up_words(intel.brought_up()) {
            report.say(Event::BroughtUp(&words));
        }
        report.say(Event::Link(intel.link()));
        report
    });
    // The link as the card came up with it, which the first change a pass
    // reports is measured against. Virtio reports no link changes at all.
    let mut link_up = match &nic {
        Card::Intel(intel) => intel.link().is_up(),
        Card::Virtio(_) => true,
    };
    let mac = nic.mac();
    let mut device = DmaNic { nic };

    say!(
        "netd: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    let config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    let epoch = Instant::now();
    let now = SmoltcpInstant::from_millis(0);
    let mut iface = Interface::new(config, &mut device, now);

    let mut socket_set = SocketSet::new(vec![]);

    // Empty, because the lease names the resolvers and nothing else may: a
    // server written down here would answer for one network on every other.
    let dns_socket = dns::Socket::new(&[], vec![]);
    let dns_handle = socket_set.add(dns_socket);
    let dhcp_handle = socket_set.add(dhcp::socket());
    let mut dhcp = dhcp::Dhcp::new();

    let total_mem = total_memory();
    let max_piped = max_piped_connections(total_mem);
    let mut daemon = NetDaemon::new(dns_handle, max_piped);

    // Sized for the slot ceiling rather than for `max_piped`: the batch
    // between two `wait` calls is the two fixed registrations, one per live piped
    // connection and one per pending connection, and the ceiling is what that
    // can never exceed.
    let poller = Poller::new(FIXED_POLL_HANDLES + MAX_PIPED_SLOTS as u32 + MAX_PENDING_CONNS);
    const TOKEN_LISTENER: u64 = 0;
    const TOKEN_NIC: u64 = 1;
    const TOKEN_TX_PIPE_BASE: u64 = 0x1000;
    // Clear of the tx-pipe range by more than `MAX_PIPED_SLOTS`, and of a
    // connection's own handle by more than `MAX_HANDLES` (4096,
    // `kernel/src/object/handle.rs`).
    const TOKEN_PENDING_BASE: u64 = 0x1_0000;

    let mut pending: Vec<PendingConn> = Vec::new();

    loop {
        // Before `iface.poll`, because it is what makes the interrupt taken and
        // what gives a driver with a per-pass receive budget that budget back.
        if let Some(link) = device.nic.begin_pass() {
            if let Some(report) = &report {
                report.say(Event::Link(link));
            }
            // Down to up only, and only with no lease held: a speed change is
            // no new network, and a bound lease is kept across a flap rather
            // than given up — `dhcp::restart`'s own header. Before the poll
            // below, so the DISCOVER goes out on this pass.
            if link.is_up() && !link_up && !dhcp.leased() {
                dhcp::restart(socket_set.get_mut::<dhcpv4::Socket>(dhcp_handle));
            }
            link_up = link.is_up();
        }
        let now = SmoltcpInstant::from_millis(epoch.elapsed().as_millis() as i64);
        while iface.poll(now, &mut device, &mut socket_set) != PollResult::None {}

        // **After the poll and before anything is served.** The lease is what
        // gives this machine an address, a route and its resolvers, so a client
        // answered before it was applied would be answered on a machine that is
        // on no network.
        let change = dhcp::Change::of(socket_set.get_mut::<dhcpv4::Socket>(dhcp_handle));
        let held = dhcp.leased();
        if let (Some(report), Some(change)) = (&report, change.as_ref()) {
            match change.lease() {
                Some((address, router, server)) => report.say(Event::Leased {
                    address: address.address(),
                    prefix: address.prefix_len(),
                    server,
                    router,
                }),
                // Only a lease that was held is lost: the client reports the
                // same on its way to a first one.
                None if held => report.say(Event::Lost),
                None => {}
            }
        }
        if dhcp.pass(change, &mut iface, socket_set.get_mut::<dns::Socket>(dns_handle)) {
            say!(
                "netd: ready, at most {max_piped} piped connections \
                 ({} MiB each of {} MiB total)",
                PIPED_CONNECTION_BYTES / (1024 * 1024),
                total_mem / (1024 * 1024),
            );
        }

        daemon.bridge_piped(&mut socket_set);

        daemon.check_piped_listeners(&mut socket_set);

        daemon.cleanup_dead_listeners(&mut socket_set);

        daemon.process_pending(&mut socket_set);

        let delay = iface.poll_delay(now, &socket_set);

        let has_pending_async = !daemon.pending_udp_recvs.is_empty()
            || !daemon.pending_dns.is_empty()
            || !daemon.pending_piped_connects.is_empty();
        let has_piped = !daemon.piped_connections.is_empty();

        // Use 1ms polling when piped connections are active. This is the network
        // equivalent of NAPI polling — during active I/O, poll frequently to
        // bridge data between smoltcp and kernel pipes without relying solely on
        // interrupt-driven wakeups.
        let timeout_nanos = if has_pending_async || has_piped {
            Some(Duration::from_millis(1).as_nanos() as u64)
        } else {
            match delay {
                Some(d) if d.total_millis() > 0 => Some(Duration::from_millis(d.total_millis() as u64).as_nanos() as u64),
                Some(_) => Some(Duration::from_millis(1).as_nanos() as u64),
                None => None,
            }
        };

        poller.watch(&acceptor, READABLE, TOKEN_LISTENER);
        poller.watch(device.nic.claim(), READABLE, TOKEN_NIC);

        // Submit POLL_ADD for each active tx pipe (client → netd direction)
        for (i, conn) in daemon.piped_connections.iter().enumerate() {
            if let Some(ref pipe) = conn.tx_read {
                poller.watch(pipe, READABLE, TOKEN_TX_PIPE_BASE + i as u64);
            }
        }

        for p in pending.iter() {
            poller.watch(&p.conn, READABLE, TOKEN_PENDING_BASE + p.conn.as_handle().0 as u64);
        }

        let timeout = match timeout_nanos {
            None => u64::MAX,
            Some(n) => n,
        };
        // The probe's window is a wake of its own: an idle machine would
        // otherwise sleep through the moment it owes its answer.
        let timeout = match &report {
            Some(report) => {
                let left = LEASE_WINDOW.saturating_sub(started.elapsed());
                if left.is_zero() {
                    end_the_lease_probe(report, device.nic, dhcp.leased());
                }
                timeout.min(left.as_nanos() as u64)
            }
            None => timeout,
        };
        // A client that connects and then says nothing wakes nothing, so the
        // deadline that removes it has to be a wake in its own right: without
        // this netd can sit in `wait` forever with `pending` full of clients
        // whose handshake is already over its time.
        let timeout = if pending.is_empty() {
            timeout
        } else {
            timeout.min(HANDSHAKE_TIMEOUT.as_nanos() as u64)
        };

        let mut ready: Vec<u64> = Vec::new();
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
                answer_inspect(&request, &daemon, &device.nic, &dhcp);
                continue;
            }
            daemon.handle_message(request, &mut socket_set, &mut iface);
        }
    }
}
