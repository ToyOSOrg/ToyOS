//! The function, brought up and driven over its claim: a multiport console
//! device (virtio 1.2 §5.3) whose port [`toyos_virtio_serial::PORT_ID`] is
//! the one named port.
//!
//! **The transport and the rings are `toyos-virtio`'s**, over
//! `toyos-pci-claim`'s register window and grant. What is this file's is the
//! console device: its six queues (§5.3.2) — port 0's two, which nothing uses,
//! the control pair and the named port's pair — on the one MSI-X entry the
//! kernel programs, and which buffer goes with which head. The port's receive
//! buffers are posted only for a service that reads the host
//! ([`Channel::post_receives`]).
//!
//! **Every buffer is posted at its own head**, so a completion's head *is* the
//! buffer it used and nothing the device wrote is an index. **A control
//! receive buffer is posted again as soon as it is read**, before its message
//! is handled: QEMU 11.1.1 drops a control event that finds none posted,
//! measured.
//!
//! **A used ring the device is not believed on ends the program** by the
//! refusal's own name: every message after it is one that silently never
//! arrives.

use std::time::{Duration, Instant};

use toyos::shm::SharedMemory;
use toyos::{DmaRegion, PciDev};
use toyos_abi::syscall::SyscallError;
use toyos_device_memory::DmaBuffers;
use toyos_pci_claim::virtio::{negotiate, Negotiated, Refusal};
use toyos_pci_claim::{Bar, Grant};
use toyos_virtio::pci::Live;
use toyos_virtio::queue::{Buffer, Parts, Used, Virtqueue};

use toyos_virtio_serial::control::{self, Control};
use toyos_virtio_serial::{CONTROL_BUFFERS, MAX_PORTS};

/// §5.3.3: the device has control queues and more than one port.
const VIRTIO_CONSOLE_F_MULTIPORT: u64 = 1 << 1;

/// §5.3.4: `max_nr_ports`, after `cols` and `rows`.
const MAX_NR_PORTS: usize = 4;

/// The one MSI-X table entry the kernel programs.
const MSIX_ENTRY: u16 = 0;

/// §5.3.2: port 0's receive and transmit queues, the control pair, then a
/// pair per further port.
const PORT0_RX: u16 = 0;
const PORT0_TX: u16 = 1;
const CONTROL_RX: u16 = 2;
const CONTROL_TX: u16 = 3;
const PORT_RX: u16 = 4;
const PORT_TX: u16 = 5;

/// Port 0 exists and is never posted to.
const PORT0_SIZE: u16 = 1;
const CONTROL_RX_SIZE: u16 = CONTROL_BUFFERS as u16;
/// The control messages a driver can have in flight at once: at most a
/// `DEVICE_READY`, a `PORT_READY` per port and two `PORT_OPEN`s between two
/// reads of the used ring.
const CONTROL_TX_SIZE: u16 = 4;

const PAGE: usize = 0x1000;

/// The named port's receive buffers: how many, and how many bytes each.
const RECEIVE: (u16, usize) = (16, 4096);
/// The named port's transmit buffers: enough in flight that the host is
/// handed the next while it writes one out, each at least a connection's
/// widest frame (`toyos::ipc::MAX_FRAME_LEN`).
const TRANSMIT: (u16, usize) = (8, 16 * 1024);

const _: () = assert!(TRANSMIT.1 >= toyos::ipc::MAX_FRAME_LEN as usize);

/// Where everything is in the grant.
struct Layout {
    rx_size: u16,
    tx_size: u16,
    rx_bytes: usize,
    tx_bytes: usize,
    queues: [Parts; 6],
    control_rx: usize,
    control_tx: usize,
    port_rx: usize,
    port_tx: usize,
    bytes: u64,
}

impl Layout {
    fn new() -> Self {
        let (rx_size, rx_bytes) = RECEIVE;
        let (tx_size, tx_bytes) = TRANSMIT;
        let sizes = [PORT0_SIZE, PORT0_SIZE, CONTROL_RX_SIZE, CONTROL_TX_SIZE, rx_size, tx_size];
        let mut at = 0;
        let queues = sizes.map(|size| {
            let parts = Parts::contiguous(at, size);
            at = (parts.end(size) + 15) & !15;
            parts
        });
        let page = |at: usize| (at + PAGE - 1) & !(PAGE - 1);
        let control_rx = page(at);
        let control_tx = control_rx + CONTROL_RX_SIZE as usize * control::BUFFER_BYTES;
        let port_rx = page(control_tx + CONTROL_TX_SIZE as usize * control::BYTES);
        let port_tx = page(port_rx + rx_size as usize * rx_bytes);
        let bytes = (port_tx + tx_size as usize * tx_bytes) as u64;
        Self { rx_size, tx_size, rx_bytes, tx_bytes, queues, control_rx, control_tx, port_rx, port_tx, bytes }
    }
}

/// Why the function is not driven.
pub enum NotOpened {
    Bringup(Refusal),
    /// It negotiated no `VIRTIO_CONSOLE_F_MULTIPORT`, so it has no port but 0.
    NotMultiport,
    /// Its `max_nr_ports` is not the launch's `max_ports`.
    Ports(u32),
}

impl std::fmt::Display for NotOpened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bringup(why) => write!(f, "{why}"),
            Self::NotMultiport => write!(f, "it negotiated no VIRTIO_CONSOLE_F_MULTIPORT"),
            Self::Ports(n) => write!(f, "its max_nr_ports reads {n}, where the launch declares {MAX_PORTS}"),
        }
    }
}

impl From<Refusal> for NotOpened {
    fn from(why: Refusal) -> Self {
        Self::Bringup(why)
    }
}

impl From<toyos_virtio::pci::Refusal> for NotOpened {
    fn from(why: toyos_virtio::pci::Refusal) -> Self {
        Self::Bringup(why.into())
    }
}

impl From<toyos_pci_claim::KernelRefused> for NotOpened {
    fn from(why: toyos_pci_claim::KernelRefused) -> Self {
        Self::Bringup(why.into())
    }
}

/// The next chain the device has finished with on `rings`, or `None`; a used
/// ring the device is not believed on ends the program.
fn finished(rings: &mut Virtqueue<Grant>) -> Option<Used> {
    rings.poll_used().unwrap_or_else(|why| panic!("virtio-console: this device cannot be driven on — {why}"))
}

/// What [`Channel::receive`] did with the host's next buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    /// The host has filled none.
    Empty,
    /// It was taken, and is posted again.
    Taken,
    /// It was not taken, and is handed over again next time.
    Held,
}

/// One control buffer the device used, as many bytes as it wrote.
pub struct Received {
    bytes: [u8; control::BUFFER_BYTES],
    len: usize,
}

impl Received {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// The function, brought up and driving.
pub struct Channel {
    dev: PciDev,
    device: Live<Bar>,
    layout: Layout,
    /// Held for their mappings' lives: the register window points into the
    /// first and the grant's into the second.
    _bar: SharedMemory,
    _region: DmaRegion,
    grant: Grant,
    /// Held for the device's life: it has queues 0 and 1 and is never posted to.
    _port0: [Virtqueue<Grant>; 2],
    control_rx: Virtqueue<Grant>,
    control_tx: Virtqueue<Grant>,
    /// Control transmit heads nothing is in flight on.
    control_free: Vec<u16>,
    port_rx: Virtqueue<Grant>,
    /// A receive the host filled that was not taken.
    held: Option<Used>,
    port_tx: Virtqueue<Grant>,
    /// Transmit heads nothing is in flight on.
    transmit_free: Vec<u16>,
    /// Each transmit head's length and when it was made available, while it
    /// is in flight.
    in_flight: Vec<Option<(u32, Instant)>>,
}

impl Channel {
    /// Bring the function up — §3.1.1's order, every queue before
    /// `DRIVER_OK` — post every control receive buffer, and send
    /// `DEVICE_READY`.
    pub fn open(dev: PciDev) -> Result<Self, NotOpened> {
        let Negotiated { setup, mapping, features } = negotiate(&dev, VIRTIO_CONSOLE_F_MULTIPORT)?;
        toyos::say!("virtio-console: {features}");
        if setup.features() & VIRTIO_CONSOLE_F_MULTIPORT == 0 {
            return Err(NotOpened::NotMultiport);
        }
        let (setup, ports) = setup.device_read32(MAX_NR_PORTS)?;
        if ports != MAX_PORTS {
            return Err(NotOpened::Ports(ports));
        }

        let layout = Layout::new();
        let (grant, region) = Grant::alloc(&dev, layout.bytes)?;
        let [q0, q1, q2, q3, q4, q5] = layout.queues;
        let port0 = [
            Virtqueue::new(grant, PORT0_RX, PORT0_SIZE, q0),
            Virtqueue::new(grant, PORT0_TX, PORT0_SIZE, q1),
        ];
        let control_rx = Virtqueue::new(grant, CONTROL_RX, CONTROL_RX_SIZE, q2);
        let control_tx = Virtqueue::new(grant, CONTROL_TX, CONTROL_TX_SIZE, q3);
        let port_rx = Virtqueue::new(grant, PORT_RX, layout.rx_size, q4);
        let port_tx = Virtqueue::new(grant, PORT_TX, layout.tx_size, q5);
        let setup = setup
            .config_vector(MSIX_ENTRY)?
            .enable(&port0[0], MSIX_ENTRY)?
            .enable(&port0[1], MSIX_ENTRY)?
            .enable(&control_rx, MSIX_ENTRY)?
            .enable(&control_tx, MSIX_ENTRY)?
            .enable(&port_rx, MSIX_ENTRY)?
            .enable(&port_tx, MSIX_ENTRY)?;

        let mut channel = Self {
            dev,
            device: setup.driver_ok(),
            _bar: mapping,
            _region: region,
            grant,
            _port0: port0,
            control_rx,
            control_tx,
            control_free: (0..CONTROL_TX_SIZE).rev().collect(),
            port_rx,
            held: None,
            port_tx,
            transmit_free: (0..layout.tx_size).rev().collect(),
            in_flight: vec![None; layout.tx_size as usize],
            layout,
        };
        for head in 0..CONTROL_RX_SIZE {
            channel.post_control(head);
        }
        channel.send_control(Control { id: 0, event: control::Event::DeviceReady, value: 1 });
        Ok(channel)
    }

    /// The claim, for the poller: readable means an interrupt has landed.
    pub fn claim(&self) -> &PciDev {
        &self.dev
    }

    /// Drain the interrupt record, so the claim stops reading ready. What a
    /// message meant is in the rings.
    pub fn take_interrupt(&self) {
        match self.dev.irq() {
            Ok(_) | Err(SyscallError::WouldBlock) => {}
            Err(why) => panic!("virtio-console: the claim refused its interrupt record: {why:?}"),
        }
    }

    fn post_control(&mut self, head: u16) {
        let at = self.layout.control_rx + head as usize * control::BUFFER_BYTES;
        let buffer = Buffer::writable(self.grant.device_addr(at), control::BUFFER_BYTES as u32);
        let published = self.control_rx.publish(head, &[buffer]);
        self.device.notify(published);
    }

    fn post_port(&mut self, head: u16) {
        let at = self.layout.port_rx + head as usize * self.layout.rx_bytes;
        let buffer = Buffer::writable(self.grant.device_addr(at), self.layout.rx_bytes as u32);
        let published = self.port_rx.publish(head, &[buffer]);
        self.device.notify(published);
    }

    /// The next control buffer the host filled, posted again before it is
    /// answered.
    pub fn control(&mut self) -> Option<Received> {
        let Used { head, written } = finished(&mut self.control_rx)?;
        let at = self.layout.control_rx + head as usize * control::BUFFER_BYTES;
        let mut received = Received { bytes: [0; control::BUFFER_BYTES], len: written as usize };
        for (i, byte) in received.bytes[..received.len].iter_mut().enumerate() {
            *byte = self.grant.window().read(at + i);
        }
        self.post_control(head);
        Some(received)
    }

    /// Send `message` on the control queue.
    ///
    /// # Panics
    /// With every control head still in flight: the device has taken back none
    /// of [`CONTROL_TX_SIZE`] messages, which no device that is reading does.
    pub fn send_control(&mut self, message: Control) {
        while let Some(done) = finished(&mut self.control_tx) {
            self.control_free.push(done.head);
        }
        let head = self
            .control_free
            .pop()
            .unwrap_or_else(|| panic!("virtio-console: the device took back none of {CONTROL_TX_SIZE} control messages"));
        let at = self.layout.control_tx + head as usize * control::BYTES;
        for (i, byte) in control::encode(message).into_iter().enumerate() {
            self.grant.window().write(at + i, byte);
        }
        let frame = Buffer::readable(self.grant.device_addr(at), control::BYTES as u32);
        let published = self.control_tx.publish(head, &[frame]);
        self.device.notify(published);
    }

    /// Post every receive buffer of the named port, for a service that reads
    /// what the host writes; until then the host's bytes stay with it.
    pub fn post_receives(&mut self) {
        for head in 0..self.layout.rx_size {
            self.post_port(head);
        }
    }

    /// Hand the next buffer the host filled on the named port to `take`, and
    /// post it again where `take` answers that it took it.
    pub fn receive(&mut self, take: impl FnOnce(&[u8]) -> bool) -> Host {
        let Some(used) = self.held.take().or_else(|| finished(&mut self.port_rx)) else { return Host::Empty };
        let Used { head, written } = used;
        let at = self.layout.port_rx + head as usize * self.layout.rx_bytes;
        let window = self.grant.window().sub(at, written as usize);
        // SAFETY: the window is inside the grant, which lives as long as
        // `self`; the device has finished with this buffer — its used
        // element said so, and `written` is at most the buffer's length —
        // and it is not posted again until `take` has returned. virtio is
        // the hypervisor's, inside the trusted base, so its protocol is
        // believed; an external or hot-plug device's driver must copy instead.
        if take(unsafe { std::slice::from_raw_parts(window.as_ptr() as *const u8, written as usize) }) {
            self.post_port(head);
            Host::Taken
        } else {
            self.held = Some(used);
            Host::Held
        }
    }

    /// Take back every transmit the device has finished with: answers the
    /// bytes they carried.
    pub fn transmitted(&mut self) -> u64 {
        let mut bytes = 0;
        while let Some(Used { head, .. }) = finished(&mut self.port_tx) {
            let (len, _) = self.in_flight[head as usize].take().expect("a used head was published");
            bytes += u64::from(len);
            self.transmit_free.push(head);
        }
        bytes
    }

    /// Whether a transmit can be made now.
    pub fn can_transmit(&self) -> bool {
        !self.transmit_free.is_empty()
    }

    /// When the oldest transmit still in flight was made, if one is.
    pub fn oldest(&self) -> Option<Instant> {
        self.in_flight.iter().flatten().map(|(_, at)| *at).min()
    }

    /// One transmit on the named port, of the bytes `fill` writes into a
    /// transmit buffer, and `fill` answers how many. Nothing is sent for 0.
    ///
    /// # Panics
    /// With every transmit in flight: the caller asks [`Self::can_transmit`].
    pub fn transmit(&mut self, fill: impl FnOnce(&mut [u8]) -> usize) -> usize {
        let head = self.transmit_free.pop().expect("virtio-console: a transmit was made with every buffer in flight");
        let at = self.layout.port_tx + head as usize * self.layout.tx_bytes;
        let window = self.grant.window().sub(at, self.layout.tx_bytes);
        // SAFETY: the window is inside the grant, which lives as long as
        // `self`; the device is not reading it, because its head is free and
        // the buffer is published only after `fill` returns. virtio is the
        // hypervisor's, inside the trusted base, so its protocol is believed;
        // an external or hot-plug device's driver must copy instead.
        let len = fill(unsafe { std::slice::from_raw_parts_mut(window.as_ptr(), self.layout.tx_bytes) });
        if len == 0 {
            self.transmit_free.push(head);
            return 0;
        }
        assert!(len <= self.layout.tx_bytes, "virtio-console: a transmit of {len} bytes into a buffer of {}", self.layout.tx_bytes);
        let frame = Buffer::readable(self.grant.device_addr(at), len as u32);
        let published = self.port_tx.publish(head, &[frame]);
        self.device.notify(published);
        self.in_flight[head as usize] = Some((len as u32, Instant::now()));
        len
    }
}

/// How long the device may take to name its ports. Policy, far past what QEMU
/// 11.1.1 takes, measured: it adds and names them at the driver's first
/// messages.
pub const PROBE_BOUND: Duration = Duration::from_secs(5);
