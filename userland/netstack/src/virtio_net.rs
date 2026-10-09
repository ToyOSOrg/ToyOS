//! The virtio-net driver. It runs here, in netstack, not in the kernel.
//!
//! What the kernel keeps is the *claim*: config space, the interrupt vector it
//! programmed into this function's MSI-X table, and the address space the
//! function translates through. Everything below that is this file and
//! `toyos-virtio`, and none of it is authority: an address written into a
//! descriptor is one `PciDev::dma_alloc` answered, and one this driver invents
//! instead is refused at the unit and recorded against this process.
//!
//! **The transport and the rings are `toyos-virtio`'s**, where every word the
//! device writes is bounded and every refusal is host-tested, over
//! `device.rs`'s register window and grant. What is this file's is the
//! network device (§5.1): which buffer goes with which head, the header in
//! front of every frame, and what becomes of a refusal.
//!
//! **A used ring the device is not believed on ends this program**, by the
//! refusal's own name ([`finished`]): no conforming device writes one, the
//! refused element spent one a chain in flight was owed, and every frame
//! after it is one that silently never arrives.
//!
//! Virtio 1.2 throughout: §4.1.4 for the PCI capability layout, §5.1 for the
//! network device.

use std::cell::RefCell;

use toyos::shm::SharedMemory;
use toyos::volatile::Window;
use toyos::{DmaRegion, PciDev};
use toyos_abi::syscall::{RegWidth, SyscallError};
use toyos_device_memory::DmaBuffers;
use toyos_virtio::pci::{Layout, Live, Offer, VendorCap};
use toyos_virtio::queue::{avail_bytes, desc_bytes, Buffer, Parts, Published, Used, Virtqueue};

use crate::device::{Bar, Grant, KernelRefused};

/// PCI's own vendor-specific capability id; virtio's config structures are all
/// published under it (§4.1.4).
const CAP_ID_VENDOR: u8 = 0x09;

/// Where the capability list starts, and how far a walk may follow it. The
/// pointer is the *device's*, so a malformed or cyclic chain ends the walk
/// rather than running for ever.
const CAPABILITIES_PTR: u32 = 0x34;
const MAX_CAPABILITIES: usize = 48;

/// §5.1.3: the device has a MAC address of its own to read.
const VIRTIO_NET_F_MAC: u64 = 1 << 5;
/// §6, for the feature line: the bit `toyos-virtio` accepts wherever it is
/// offered, and a gate reads back.
const VIRTIO_F_ACCESS_PLATFORM: u64 = toyos_virtio::pci::VIRTIO_F_ACCESS_PLATFORM;

/// The one MSI-X table entry the kernel programs.
const MSIX_ENTRY: u16 = 0;

const RX_QUEUE: u16 = 0;
const TX_QUEUE: u16 = 1;
/// One descriptor per receive buffer, and the two counts are one number: buffer
/// `i` is posted at head `i` and nowhere else, so a completion's head *is* the
/// buffer it filled and nothing the device wrote is used as an index.
const RX_QUEUE_SIZE: u16 = 256;
pub const RX_BUF_COUNT: usize = RX_QUEUE_SIZE as usize;
pub const RX_BUF_SIZE: usize = 4096;
/// One descriptor per transmit buffer, and the two counts are one number for
/// the same reason the receive side's are: buffer `i` is published at head `i`
/// and nowhere else, so a head this driver holds names a buffer nothing else
/// is writing. Sixteen heads over one buffer would be sixteen aliases — smoltcp
/// emits several frames per poll, and the device reads a descriptor whenever it
/// likes.
const TX_QUEUE_SIZE: u16 = 16;
pub const TX_BUF_COUNT: usize = TX_QUEUE_SIZE as usize;
pub const TX_BUF_SIZE: usize = 4096;
/// The header virtio 1.0 puts in front of every frame, both directions: always
/// twelve bytes with `VERSION_1`, `num_buffers` included (§5.1.6).
pub const NET_HDR_SIZE: usize = 12;

/// The grant's layout: the receive queue's three parts on a page each, the
/// transmit queue's three in one page, then the frames. One grant, because
/// every byte of it is this process's own — what a wrong offset here corrupts
/// is this driver's memory and nobody else's.
const RX_PARTS: Parts = Parts { desc: 0x0000, avail: 0x1000, used: 0x2000 };
const TX_PARTS: Parts = Parts::contiguous(0x3000, TX_QUEUE_SIZE);
const OFF_RX_BUFS: usize = 0x4000;
const OFF_TX_BUFS: usize = OFF_RX_BUFS + RX_BUF_COUNT * RX_BUF_SIZE;
const GRANT_BYTES: u64 = (OFF_TX_BUFS + TX_BUF_COUNT * TX_BUF_SIZE) as u64;

const _: () = {
    assert!(desc_bytes(RX_QUEUE_SIZE) <= RX_PARTS.avail - RX_PARTS.desc);
    assert!(avail_bytes(RX_QUEUE_SIZE) <= RX_PARTS.used - RX_PARTS.avail);
    assert!(RX_PARTS.end(RX_QUEUE_SIZE) <= TX_PARTS.desc);
    assert!(TX_PARTS.end(TX_QUEUE_SIZE) <= OFF_RX_BUFS);
    assert!(NET_HDR_SIZE < RX_BUF_SIZE && NET_HDR_SIZE < TX_BUF_SIZE);
};

/// Why the device was not brought up. Each keeps its own word: a machine with
/// no such device and one that refused a feature set ask different things of a
/// caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// What the device published or answered, as the transport refused it.
    Device(toyos_virtio::pci::Refusal),
    Kernel(KernelRefused),
    /// The claim answered a configuration read it had to refuse.
    Unbounded(&'static str, u32),
}

impl From<toyos_virtio::pci::Refusal> for Refusal {
    fn from(why: toyos_virtio::pci::Refusal) -> Self {
        Self::Device(why)
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Device(why) => write!(f, "{why}"),
            Self::Kernel(why) => write!(f, "{why}"),
            Self::Unbounded(what, at) => write!(
                f,
                "the claim answered a {what} at {at:#x}, so it is not a claim on one \
                 function's own configuration space"
            ),
        }
    }
}

/// The next chain the device has finished with on `rings`, or `None`.
///
/// **Every way the used ring is not believed ends the device's use**: a head
/// past the table, a head with no chain in flight, more bytes than a chain's
/// writable ones, and an index past what was made available. Nothing drives
/// this NIC from there, and this dies where it can be read.
fn finished(rings: &mut Virtqueue<Grant>) -> Option<Used> {
    rings
        .poll_used()
        .unwrap_or_else(|why| panic!("netstack: this NIC cannot be driven on — {why}"))
}

/// The bound the capability walk rests on, asked once before the walk.
///
/// **The walk below indexes configuration space by numbers the *device* wrote**
/// — the capability pointer and every `next` link in the chain — and it is safe
/// only because a claim answers its own function's 4 KiB and nothing else. That
/// is the kernel's contract, so this is where the driver that depends on it
/// checks it: a read past the end and one not aligned for its own width are
/// both refused, and the first byte is not. An aligned read that straddles the
/// end cannot be written — 4096 is a multiple of every width — and one whose
/// offset wraps cannot be expressed, `PciDev::config_read` taking a `u32`; both
/// are answered where the arithmetic lives, in `toyos-dma`'s host tests.
fn config_space_is_bounded(dev: &PciDev) -> Result<(), Refusal> {
    const CONFIG_BYTES: u32 = 4096;
    for (what, at, width) in [
        ("read past its configuration space", CONFIG_BYTES, RegWidth::U8),
        ("misaligned read", 1, RegWidth::U16),
    ] {
        if dev.config_read(at, width).is_ok() {
            return Err(Refusal::Unbounded(what, at));
        }
    }
    // And the bound is a bound rather than a wall: the vendor id is still there.
    dev.config_read(0, RegWidth::U16).map_err(KernelRefused::on("its vendor id")).map_err(Refusal::Kernel)?;
    crate::say!(
        "netstack: this claim answers {CONFIG_BYTES} bytes of configuration space and refuses \
         every access outside them"
    );
    Ok(())
}

/// The virtio-net function, brought up and driving.
pub struct VirtioNet {
    dev: PciDev,
    device: Live<Bar>,
    /// Held for their mappings' lives: the register window points into the
    /// first and the grant's into the second.
    _bar: SharedMemory,
    _region: DmaRegion,
    grant: Grant,
    rx: RefCell<Virtqueue<Grant>>,
    tx: RefCell<TxQueue>,
    mac: [u8; 6],
}

impl VirtioNet {
    /// Bring the function up: the capability chain, then the reset, the
    /// feature negotiation, both queues and their vectors in the order
    /// `toyos-virtio`'s types fix, which is virtio 1.2 §3.1.1's.
    pub fn open(dev: PciDev) -> Result<Self, Refusal> {
        let info = dev.describe().map_err(KernelRefused::on("the claim's description")).map_err(Refusal::Kernel)?;

        config_space_is_bounded(&dev)?;
        let layout = Layout::of(&vendor_caps(&dev))?;
        // The kernel hands out a BAR at a time, and reports 0 bytes for one it
        // keeps back.
        let bar = layout.bar();
        let bar_bytes = *info
            .bar_bytes
            .get(bar as usize)
            .filter(|bytes| **bytes > 0)
            .ok_or(toyos_virtio::pci::Refusal::MissingCap("a BAR this claim may map"))?;
        let mapped = dev
            .map_bar(bar as u32, bar_bytes)
            .map_err(KernelRefused::on("the register window"))
            .map_err(Refusal::Kernel)?;
        // SAFETY: the mapping is `bar_bytes` long and lives as long as
        // `mapped`, which this struct holds for its own life.
        let window = unsafe { Window::new(mapped.as_ptr(), bar_bytes as usize) };

        let offer = Offer::acknowledge(Bar::over(window), &layout)?;
        let offered = offer.features();
        let setup = offer.accept(VIRTIO_NET_F_MAC)?;
        let features = setup.features();
        // The line the kernel's virtio drivers print, in the same shape:
        // `iommu_virtio_platform` reads it back for every virtio function the
        // machine creates.
        crate::say!(
            "netstack: VirtIO: PCI {:02x}:{:02x}.{} features device={offered:#x} \
             negotiated={features:#x} access_platform={}",
            info.bus,
            info.dev,
            info.func,
            if features & VIRTIO_F_ACCESS_PLATFORM != 0 { 'y' } else { 'n' },
        );

        let region = dev
            .dma_alloc(GRANT_BYTES)
            .map_err(KernelRefused::on("a DMA grant"))
            .map_err(Refusal::Kernel)?;
        // SAFETY: the grant covers at least `GRANT_BYTES` — the kernel rounds
        // the request up to whole pages, never down — and lives as long as
        // `region`, which this struct holds.
        let window = unsafe { Window::new(region.memory.as_ptr(), GRANT_BYTES as usize) };
        window.zero();
        let grant = Grant::over(window, region.device_addr);

        let rx = Virtqueue::new(grant, RX_QUEUE, RX_QUEUE_SIZE, RX_PARTS);
        let tx = TxQueue::new(grant);
        // The vector the kernel already put in the table, named to the device
        // for each of its sources.
        let mut setup = setup
            .config_vector(MSIX_ENTRY)?
            .enable(&rx, MSIX_ENTRY)?
            .enable(&tx.rings, MSIX_ENTRY)?;

        let mut mac = [0u8; 6];
        for (at, byte) in mac.iter_mut().enumerate() {
            (setup, *byte) = setup.device_read8(at)?;
        }

        let nic = Self {
            dev,
            device: setup.driver_ok(),
            _bar: mapped,
            _region: region,
            grant,
            rx: RefCell::new(rx),
            tx: RefCell::new(tx),
            mac,
        };

        // Every receive buffer posted before a frame can arrive.
        for index in 0..RX_BUF_COUNT {
            nic.post_rx(index);
        }
        Ok(nic)
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Drain the interrupt record, so the claim stops reading ready.
    ///
    /// The count is not acted on — what a message meant is in the rings — but
    /// it has to be consumed, or the poller reports the same interrupt for ever.
    /// `WouldBlock` is the ordinary "nothing since the last read". Any other
    /// refusal is handed up as the kernel worded it: what it means is
    /// `Card::begin_pass`'s call, made once for both drivers.
    pub fn take_interrupt(&self) -> Result<u32, SyscallError> {
        match self.dev.irq() {
            Ok(record) => Ok(record.count),
            Err(SyscallError::WouldBlock) => Ok(0),
            Err(why) => Err(why),
        }
    }

    /// Post receive buffer `index` on its own head.
    fn post_rx(&self, index: usize) {
        let at = OFF_RX_BUFS + index * RX_BUF_SIZE;
        // The header is zeroed before the buffer is published: what the device
        // writes there is its own, and what it leaves behind is this driver's.
        self.grant.window().sub(at, NET_HDR_SIZE).zero();
        let buffer = Buffer::writable(self.grant.device_addr(at), RX_BUF_SIZE as u32);
        let published = self.rx.borrow_mut().publish(index as u16, &[buffer]);
        self.device.notify(published);
    }

    /// The next received frame as `(buffer index, frame bytes)`, the virtio
    /// header excluded, or `None`.
    pub fn poll_rx(&self) -> Option<(usize, usize)> {
        loop {
            // The head is below this queue's size, which is the buffer count,
            // and `written` no more than the one buffer its chain is.
            let Used { head, written } = finished(&mut self.rx.borrow_mut())?;
            let (index, total) = (head as usize, written as usize);
            if total <= NET_HDR_SIZE {
                // Shorter than its own header is nothing to hand up, and the
                // buffer goes straight back.
                self.post_rx(index);
                continue;
            }
            return Some((index, total - NET_HDR_SIZE));
        }
    }

    /// Give buffer `index` back to the device, after its frame has been read.
    ///
    /// A buffer never given back is a receive slot lost for the boot: 256 of
    /// them and this NIC stops receiving.
    pub fn rx_done(&self, index: usize) {
        assert!(index < RX_BUF_COUNT, "netstack: buffer {index} is not one this NIC has");
        self.post_rx(index);
    }

    /// Where a received frame's bytes are, past the virtio header.
    pub fn rx_frame(&self, index: usize, len: usize) -> &[u8] {
        let window = self.grant.window().sub(OFF_RX_BUFS + index * RX_BUF_SIZE + NET_HDR_SIZE, len);
        // SAFETY: the window is inside the grant, which lives as long as
        // `self`; the device has finished with this buffer — its used-ring
        // element is what said so — and it is not posted again until
        // `rx_done`, which the caller makes after this borrow ends.
        unsafe { std::slice::from_raw_parts(window.as_ptr() as *const u8, len) }
    }

    /// How many frames the transmit queue takes now ([`TxQueue::room`]). A
    /// caller answered 0 sleeps on the claim.
    pub fn tx_room(&self) -> usize {
        self.tx.borrow_mut().room()
    }

    /// Fill a transmit buffer with a `len`-byte frame and hand it to the
    /// device ([`TxQueue::send`]).
    pub fn tx<R>(&self, len: usize, fill: impl FnOnce(&mut [u8]) -> R) -> R {
        let (result, published) = self.tx.borrow_mut().send(len, fill);
        self.device.notify(published);
        result
    }

    /// The claim, for the poller: readable means an interrupt has landed.
    pub fn claim(&self) -> &PciDev {
        &self.dev
    }
}

/// The transmit queue: its rings, the heads nothing is in flight on, and the
/// buffer each head owns. Everything in it is memory, so the host drives it
/// with a plain allocation for the grant.
struct TxQueue {
    rings: Virtqueue<Grant>,
    /// Transmit heads nothing is in flight on.
    free: Vec<u16>,
    grant: Grant,
}

impl TxQueue {
    fn new(grant: Grant) -> Self {
        Self {
            rings: Virtqueue::new(grant, TX_QUEUE, TX_QUEUE_SIZE, TX_PARTS),
            free: (0..TX_QUEUE_SIZE).rev().collect(),
            grant,
        }
    }

    /// How many frames the queue takes now, every head the device has
    /// finished with taken back first.
    ///
    /// **Room returning is an interrupt already**: `toyos-virtio` accepts no
    /// `VIRTIO_F_EVENT_IDX` and leaves the transmit queue's `avail.flags` 0,
    /// and §2.7.7 has the device notify for every buffer it uses on such a
    /// queue.
    fn room(&mut self) -> usize {
        while let Some(done) = finished(&mut self.rings) {
            self.free.push(done.head);
        }
        self.free.len()
    }

    /// Fill a transmit buffer with a `len`-byte frame and make it available:
    /// the device is told of it by whoever holds the transport, with what this
    /// answers.
    ///
    /// **The buffer is the head's own and the two are taken together**, so
    /// nothing is written into a buffer the device is reading: a head leaves
    /// `free` only here and comes back only in [`Self::room`], which the
    /// device's used ring is what drives.
    ///
    /// Non-blocking, and for a caller [`Self::room`] answered: a frame
    /// offered with no head free is a caller that did not ask.
    fn send<R>(&mut self, len: usize, fill: impl FnOnce(&mut [u8]) -> R) -> (R, Published) {
        assert!(
            NET_HDR_SIZE + len <= TX_BUF_SIZE,
            "netstack: a {len}-byte frame does not fit this NIC's transmit buffer"
        );
        let head = self
            .free
            .pop()
            .expect("netstack: a frame was offered to a transmit queue that had said it has no room");
        let at = OFF_TX_BUFS + head as usize * TX_BUF_SIZE;
        // The header is this driver's and zeroed before the frame goes in.
        self.grant.window().sub(at, NET_HDR_SIZE).zero();
        let window = self.grant.window().sub(at + NET_HDR_SIZE, len);
        // SAFETY: the window is inside the grant, which lives as long as the
        // driver that holds this queue; the device is not reading it, because
        // this head is out of `free` and its descriptor is published only
        // after `fill` returns.
        let result = fill(unsafe { std::slice::from_raw_parts_mut(window.as_ptr(), len) });
        let frame = Buffer::readable(self.grant.device_addr(at), (NET_HDR_SIZE + len) as u32);
        (result, self.rings.publish(head, &[frame]))
    }
}

/// The vendor capabilities a function published, in its list's order, walked
/// once.
fn vendor_caps(dev: &PciDev) -> Vec<VendorCap> {
    let mut found = Vec::new();
    let mut seen = 0usize;
    let Ok(first) = dev.config_read(CAPABILITIES_PTR, RegWidth::U8) else {
        return found;
    };
    let mut next = first;
    // The pointer is the device's: a chain that does not terminate, or one
    // pointing outside the header, ends the walk rather than running off
    // the window or for ever.
    while next >= 0x40 && next < 0x100 && seen < MAX_CAPABILITIES {
        seen += 1;
        let Ok(id) = dev.config_read(next, RegWidth::U8) else { break };
        if id as u8 == CAP_ID_VENDOR {
            let read = |at: u32, width| dev.config_read(next + at, width).unwrap_or(0);
            found.push(VendorCap {
                cfg_type: read(3, RegWidth::U8) as u8,
                bar: read(4, RegWidth::U8) as u8,
                offset: read(8, RegWidth::U32),
                length: read(12, RegWidth::U32),
                notify_off_multiplier: read(16, RegWidth::U32),
            });
        }
        let Ok(link) = dev.config_read(next + 1, RegWidth::U8) else { break };
        next = link;
    }
    found
}

/// The transmit queue's room, over a plain allocation for the grant. What a
/// used-ring element must satisfy is `toyos-virtio`'s, and tested there.
#[cfg(test)]
mod tests {
    use toyos_virtio::queue::{AVAIL_ENTRY_BYTES, DESC_BYTES, RING_ENTRIES, USED_ELEM_BYTES};

    use super::*;

    const DEVICE_BASE: u64 = 0x4000_0000;

    /// A transmit queue over a plain allocation the size of the grant.
    fn queue() -> TxQueue {
        let backing = vec![0u64; GRANT_BYTES as usize / 8].leak();
        // SAFETY: `leak` gives the allocation the `'static` lifetime the
        // window needs, and nothing but this queue and the test reaches it.
        let window = unsafe { Window::new(backing.as_mut_ptr().cast(), GRANT_BYTES as usize) };
        TxQueue::new(Grant::over(window, DEVICE_BASE))
    }

    /// The head in available-ring entry `nth` (§2.7.6).
    fn made_available(queue: &TxQueue, nth: u16) -> u16 {
        let entry = (nth % TX_QUEUE_SIZE) as usize;
        queue.grant.window().read(TX_PARTS.avail + RING_ENTRIES + entry * AVAIL_ENTRY_BYTES)
    }

    /// The device, finishing with the `count` oldest chains it was given and
    /// answering their heads (§2.7.8).
    fn device_uses(queue: &TxQueue, count: u16) -> Vec<u16> {
        let ring = queue.grant.window();
        let used_idx: u16 = ring.read(TX_PARTS.used + 2);
        (0..count)
            .map(|nth| {
                let at = used_idx.wrapping_add(nth);
                let head = made_available(queue, at);
                let element =
                    TX_PARTS.used + RING_ENTRIES + (at % TX_QUEUE_SIZE) as usize * USED_ELEM_BYTES;
                ring.write(element, head as u32);
                ring.write(element + 4, 0u32);
                ring.write(TX_PARTS.used + 2, at.wrapping_add(1));
                head
            })
            .collect()
    }

    /// The queue says how many frames it takes before one is offered: every
    /// head in flight is no room, and room returns by exactly the heads the
    /// device has finished with. Nothing is dropped on the way: each frame
    /// published is in its own buffer behind a zeroed header.
    #[test]
    fn a_full_transmit_queue_has_no_room_until_the_device_gives_heads_back() {
        let mut queue = queue();
        let mut published = 0u8;
        while queue.room() > 0 {
            assert!(published < TX_QUEUE_SIZE as u8 * 2, "the transmit queue never filled");
            let ((), told) = queue.send(60, |frame| frame.fill(published + 1));
            assert_eq!(told.queue(), TX_QUEUE);
            published += 1;
        }
        assert_eq!(published as u16, TX_QUEUE_SIZE);
        let ring = queue.grant.window();
        assert_eq!(ring.read::<u16>(TX_PARTS.avail + 2), TX_QUEUE_SIZE);
        for nth in 0..TX_QUEUE_SIZE {
            let chain = TX_PARTS.desc + made_available(&queue, nth) as usize * DESC_BYTES;
            let (addr, len): (u64, u32) = (ring.read(chain), ring.read(chain + 8));
            assert_eq!(len as usize, NET_HDR_SIZE + 60);
            let buffer = ring.sub((addr - DEVICE_BASE) as usize, len as usize);
            let bytes: Vec<u8> = (0..len as usize).map(|at| buffer.read::<u8>(at)).collect();
            assert_eq!(bytes[..NET_HDR_SIZE], [0; NET_HDR_SIZE]);
            assert_eq!(bytes[NET_HDR_SIZE..], [nth as u8 + 1; 60]);
        }

        let back = device_uses(&queue, 3);
        assert_eq!(queue.room(), 3);
        // The next frame goes out on a head the device gave back, and on no
        // head still in flight.
        let _ = queue.send(60, |frame| frame.fill(0xEE));
        assert!(back.contains(&made_available(&queue, TX_QUEUE_SIZE)));
        assert_eq!(queue.room(), 2);
    }

    /// A frame offered to a queue that said it had no room is netstack's own
    /// bug, and dies by name instead of being written somewhere and dropped.
    #[test]
    #[should_panic(expected = "had said it has no room")]
    fn a_frame_offered_to_a_full_transmit_queue_is_not_taken() {
        let mut queue = queue();
        while queue.room() > 0 {
            let _ = queue.send(60, |frame| frame.fill(1));
        }
        let _ = queue.send(60, |frame| frame.fill(2));
    }

    /// A queue with two frames in flight, on heads 0 and 1, whose device then
    /// writes one used element, `id` and `len`, and counts `used_idx` of them.
    fn after_the_device_wrote(id: u32, len: u32, used_idx: u16) -> TxQueue {
        let mut queue = queue();
        let _ = queue.send(60, |frame| frame.fill(1));
        let _ = queue.send(60, |frame| frame.fill(2));
        assert_eq!([made_available(&queue, 0), made_available(&queue, 1)], [0, 1]);
        let ring = queue.grant.window();
        ring.write(TX_PARTS.used + RING_ENTRIES, id);
        ring.write(TX_PARTS.used + RING_ENTRIES + 4, len);
        ring.write(TX_PARTS.used + 2, used_idx);
        queue
    }

    /// Each way the used ring is not believed ends the program by that
    /// refusal's own name, where it is read: none is counted and passed over.
    #[test]
    #[should_panic(expected = "this NIC cannot be driven on — a used element's head")]
    fn a_used_head_past_the_table_ends_the_driver() {
        after_the_device_wrote(0xFFFF, 0, 1).room();
    }

    #[test]
    #[should_panic(
        expected = "this NIC cannot be driven on — a used element names head 5, where no chain \
                    is in flight"
    )]
    fn a_used_head_with_no_chain_in_flight_ends_the_driver() {
        after_the_device_wrote(5, 0, 1).room();
    }

    /// A transmit chain is the device's to read and has no byte it may write.
    #[test]
    #[should_panic(
        expected = "this NIC cannot be driven on — the used element for head 0 claims more bytes \
                    than its chain may be written"
    )]
    fn a_used_length_past_the_chains_writable_bytes_ends_the_driver() {
        after_the_device_wrote(0, 1, 1).room();
    }

    #[test]
    #[should_panic(expected = "this NIC cannot be driven on — the used index reads 3")]
    fn a_used_index_past_every_frame_offered_ends_the_driver() {
        after_the_device_wrote(0, 0, 3).room();
    }
}
