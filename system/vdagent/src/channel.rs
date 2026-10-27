//! The virtio-serial function QEMU's clipboard channel is on, driven here
//! over its claim: a multiport console device (virtio 1.2 §5.3) whose port
//! [`toyos_vdagent::PORT_ID`] carries the vdagent stream.
//!
//! **The transport and the rings are `toyos-virtio`'s**, over
//! `toyos-pci-claim`'s register window and grant. What is this file's is the
//! console device: its six queues (§5.3.2) — port 0's two, which nothing uses,
//! the control pair and the agent's port's pair — on the one MSI-X entry the
//! kernel programs, and which buffer goes with which head.
//!
//! **Every receive buffer is posted at its own head**, so a completion's head
//! *is* the buffer it filled and nothing the device wrote is an index. **A
//! control receive buffer is posted again as soon as it is read**, before its
//! message is handled: QEMU 11.1.1 drops a control event that finds none
//! posted, measured.
//!
//! **A used ring the device is not believed on ends this program** by the
//! refusal's own name: every message after it is one that silently never
//! arrives.

use std::time::Instant;

use toyos::shm::SharedMemory;
use toyos::{DmaRegion, PciDev};
use toyos_abi::syscall::SyscallError;
use toyos_device_memory::DmaBuffers;
use toyos_pci_claim::virtio::{negotiate, Negotiated, Refusal};
use toyos_pci_claim::{Bar, Grant};
use toyos_vdagent::control::{self, Control};
use toyos_vdagent::{CONTROL_BUFFERS, LARGEST_TRANSMIT, MAX_PORTS};
use toyos_virtio::pci::Live;
use toyos_virtio::queue::{Buffer, Parts, Used, Virtqueue};

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
/// The control messages the agent can have in flight at once: at most a
/// `DEVICE_READY`, a `PORT_READY` and two `PORT_OPEN`s between two reads of
/// the used ring.
const CONTROL_TX_SIZE: u16 = 4;
const PORT_RX_SIZE: u16 = 16;
/// One transmit, of every message owed, in one element.
const PORT_TX_SIZE: u16 = 1;

/// A control buffer holds a message and a `PORT_NAME`'s name, which is not
/// read: a longer name is cut where the buffer ends.
const CONTROL_RX_BYTES: usize = 64;
const PORT_RX_BYTES: usize = 4096;

/// Each queue's parts, one after another on the descriptor table's alignment.
const fn after(parts: Parts, size: u16) -> usize {
    (parts.end(size) + 15) & !15
}
const Q_PORT0_RX: Parts = Parts::contiguous(0, PORT0_SIZE);
const Q_PORT0_TX: Parts = Parts::contiguous(after(Q_PORT0_RX, PORT0_SIZE), PORT0_SIZE);
const Q_CONTROL_RX: Parts = Parts::contiguous(after(Q_PORT0_TX, PORT0_SIZE), CONTROL_RX_SIZE);
const Q_CONTROL_TX: Parts = Parts::contiguous(after(Q_CONTROL_RX, CONTROL_RX_SIZE), CONTROL_TX_SIZE);
const Q_PORT_RX: Parts = Parts::contiguous(after(Q_CONTROL_TX, CONTROL_TX_SIZE), PORT_RX_SIZE);
const Q_PORT_TX: Parts = Parts::contiguous(after(Q_PORT_RX, PORT_RX_SIZE), PORT_TX_SIZE);

const PAGE: usize = 0x1000;
const OFF_CONTROL_RX: usize = (Q_PORT_TX.end(PORT_TX_SIZE) + PAGE - 1) & !(PAGE - 1);
const OFF_CONTROL_TX: usize = OFF_CONTROL_RX + CONTROL_RX_SIZE as usize * CONTROL_RX_BYTES;
const OFF_PORT_RX: usize = (OFF_CONTROL_TX + CONTROL_TX_SIZE as usize * control::BYTES + PAGE - 1) & !(PAGE - 1);
const OFF_PORT_TX: usize = OFF_PORT_RX + PORT_RX_SIZE as usize * PORT_RX_BYTES;
const GRANT_BYTES: u64 = (OFF_PORT_TX + LARGEST_TRANSMIT) as u64;

/// Why the function is not the agent's to drive.
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
    rings
        .poll_used()
        .unwrap_or_else(|why| panic!("vdagent: this device cannot be driven on — {why}"))
}

/// The function, brought up and driving.
pub struct Channel {
    dev: PciDev,
    device: Live<Bar>,
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
    port_tx: Virtqueue<Grant>,
    /// When the transmit in flight was made available.
    sent: Option<Instant>,
}

impl Channel {
    /// Bring the function up — §3.1.1's order, every queue before
    /// `DRIVER_OK` — post every receive buffer, and send `DEVICE_READY`.
    pub fn open(dev: PciDev) -> Result<Self, NotOpened> {
        let Negotiated { setup, mapping, features } = negotiate(&dev, VIRTIO_CONSOLE_F_MULTIPORT)?;
        toyos::say!("vdagent: {features}");
        if setup.features() & VIRTIO_CONSOLE_F_MULTIPORT == 0 {
            return Err(NotOpened::NotMultiport);
        }
        let (setup, ports) = setup.device_read32(MAX_NR_PORTS)?;
        if ports != MAX_PORTS {
            return Err(NotOpened::Ports(ports));
        }

        let (grant, region) = Grant::alloc(&dev, GRANT_BYTES)?;
        let port0 = [
            Virtqueue::new(grant, PORT0_RX, PORT0_SIZE, Q_PORT0_RX),
            Virtqueue::new(grant, PORT0_TX, PORT0_SIZE, Q_PORT0_TX),
        ];
        let control_rx = Virtqueue::new(grant, CONTROL_RX, CONTROL_RX_SIZE, Q_CONTROL_RX);
        let control_tx = Virtqueue::new(grant, CONTROL_TX, CONTROL_TX_SIZE, Q_CONTROL_TX);
        let port_rx = Virtqueue::new(grant, PORT_RX, PORT_RX_SIZE, Q_PORT_RX);
        let port_tx = Virtqueue::new(grant, PORT_TX, PORT_TX_SIZE, Q_PORT_TX);
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
            port_tx,
            sent: None,
        };
        for head in 0..CONTROL_RX_SIZE {
            channel.post_control(head);
        }
        for head in 0..PORT_RX_SIZE {
            channel.post_port(head);
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
            Err(why) => panic!("vdagent: the claim refused its interrupt record: {why:?}"),
        }
    }

    fn post_control(&mut self, head: u16) {
        let at = OFF_CONTROL_RX + head as usize * CONTROL_RX_BYTES;
        let buffer = Buffer::writable(self.grant.device_addr(at), CONTROL_RX_BYTES as u32);
        let published = self.control_rx.publish(head, &[buffer]);
        self.device.notify(published);
    }

    fn post_port(&mut self, head: u16) {
        let at = OFF_PORT_RX + head as usize * PORT_RX_BYTES;
        let buffer = Buffer::writable(self.grant.device_addr(at), PORT_RX_BYTES as u32);
        let published = self.port_rx.publish(head, &[buffer]);
        self.device.notify(published);
    }

    /// The next control message the host sent, its buffer posted again
    /// before it is answered.
    pub fn control(&mut self) -> Option<Result<Control, control::Refused>> {
        let Used { head, written } = finished(&mut self.control_rx)?;
        let at = OFF_CONTROL_RX + head as usize * CONTROL_RX_BYTES;
        let mut bytes = [0u8; control::BYTES];
        let len = (written as usize).min(control::BYTES);
        for (i, byte) in bytes[..len].iter_mut().enumerate() {
            *byte = self.grant.window().read(at + i);
        }
        self.post_control(head);
        Some(control::decode(&bytes[..len], MAX_PORTS))
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
            .unwrap_or_else(|| panic!("vdagent: the device took back none of {CONTROL_TX_SIZE} control messages"));
        let at = OFF_CONTROL_TX + head as usize * control::BYTES;
        for (i, byte) in control::encode(message).into_iter().enumerate() {
            self.grant.window().write(at + i, byte);
        }
        let frame = Buffer::readable(self.grant.device_addr(at), control::BYTES as u32);
        let published = self.control_tx.publish(head, &[frame]);
        self.device.notify(published);
    }

    /// Hand every buffer the host filled on the agent's port to `each`, and
    /// post it again.
    pub fn received(&mut self, mut each: impl FnMut(&[u8])) {
        while let Some(Used { head, written }) = finished(&mut self.port_rx) {
            let window = self.grant.window().sub(OFF_PORT_RX + head as usize * PORT_RX_BYTES, written as usize);
            // SAFETY: the window is inside the grant, which lives as long as
            // `self`; the device has finished with this buffer — its used
            // element said so, and `written` is at most the buffer's length —
            // and it is not posted again until `each` has returned. virtio is
            // the hypervisor's, inside the trusted base, so its protocol is
            // believed; an external or hot-plug device's driver must copy instead.
            each(unsafe { std::slice::from_raw_parts(window.as_ptr() as *const u8, written as usize) });
            self.post_port(head);
        }
    }

    /// Whether the device has taken the last transmit back, so a new one can
    /// be made.
    pub fn transmit_free(&mut self) -> bool {
        if finished(&mut self.port_tx).is_some() {
            self.sent = None;
        }
        self.sent.is_none()
    }

    /// When the transmit in flight was made, if one is.
    pub fn sent(&self) -> Option<Instant> {
        self.sent
    }

    /// One transmit on the agent's port, of the bytes `fill` writes into a
    /// [`LARGEST_TRANSMIT`] buffer, and `fill` answers how many. Nothing is
    /// sent for 0.
    ///
    /// # Panics
    /// With a transmit in flight: the caller asks [`Self::transmit_free`].
    pub fn transmit(&mut self, fill: impl FnOnce(&mut [u8]) -> usize) {
        assert!(self.sent.is_none(), "vdagent: a transmit was made over the one in flight");
        let window = self.grant.window().sub(OFF_PORT_TX, LARGEST_TRANSMIT);
        // SAFETY: the window is inside the grant, which lives as long as
        // `self`; the device is not reading it, because no transmit is in
        // flight and the buffer is published only after `fill` returns. virtio
        // is the hypervisor's, inside the trusted base, so its protocol is
        // believed; an external or hot-plug device's driver must copy instead.
        let len = fill(unsafe { std::slice::from_raw_parts_mut(window.as_ptr(), LARGEST_TRANSMIT) });
        if len == 0 {
            return;
        }
        let frame = Buffer::readable(self.grant.device_addr(OFF_PORT_TX), len as u32);
        let published = self.port_tx.publish(0, &[frame]);
        self.device.notify(published);
        self.sent = Some(Instant::now());
    }
}
