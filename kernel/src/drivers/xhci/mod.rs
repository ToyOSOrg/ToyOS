mod device;
mod hid;
mod legacy;
pub mod usbd;
mod wait;

use wait::msc;

/// Everything that waits lives under [`wait`].
pub use wait::boot::{init, PORT_POLL, PORT_SETTLE_CEILING};
pub use wait::msc::{storage_flush, storage_read, storage_write};

use alloc::vec::Vec;
use core::num::NonZeroU8;
use core::sync::atomic::{fence, AtomicU64, AtomicUsize, Ordering};
use crate::mm::Mmio;
use crate::mm::Dma;
use crate::log;
use super::pci::PciDevice;
use crate::sync::Lock;
use toyos_untrusted::Untrusted;
use toyos_xhci::job::{Await, Outcome, Outstanding, Stages};
use toyos_xhci::port::{self as portmachine, GaveUp, Gone, PortState, Reset, Step};
use toyos_xhci::recovery::{self, Act, EndpointState, NeedsConfigure, Recovery};
use toyos_xhci::Protocols;
use toyos_xhci::Portsc;

use hid::HidDevice;

const CAP_CAPLENGTH:  u64 = 0x00; // u8
const CAP_HCSPARAMS1: u64 = 0x04; // u32
const CAP_HCSPARAMS2: u64 = 0x08; // u32
const CAP_HCCPARAMS1: u64 = 0x10; // u32
const CAP_DBOFF:      u64 = 0x14; // u32
const CAP_RTSOFF:     u64 = 0x18; // u32

const OP_USBCMD:   u64 = 0x00;
const OP_USBSTS:   u64 = 0x04;
const OP_PAGESIZE: u64 = 0x08;
const OP_CRCR:     u64 = 0x18; // 64-bit
const OP_DCBAAP:   u64 = 0x30; // 64-bit
const OP_CONFIG:   u64 = 0x38;
const OP_PORT_BASE: u64 = 0x400;
const PORT_REG_SIZE: u64 = 0x10;

// Raw bits for the two paths that work on a word, not a decoded register: read_portsc's actuator injections and init_one's pre-controller port power. Every decision on these bits goes through `Portsc`, never the raw consts, outside these two paths.
const PORTSC_CCS: u32 = 1 << 0;
const PORTSC_PED: u32 = 1 << 1;
const PORTSC_PR:  u32 = 1 << 4;
const PORTSC_PP:  u32 = 1 << 9;
const PORTSC_SPEED: u32 = 0xF << 10;

/// HCCPARAMS1 bit 3: Port Power Control, which decides PORTSC's PP after reset.
const HCC_PPC: u32 = 1 << 3;

const IR0_IMAN:   u64 = 0x20; // Interrupt Management (IP + IE)
const IR0_IMOD:   u64 = 0x24; // Interrupt Moderation
const IR0_ERSTSZ: u64 = 0x28;
const IR0_ERSTBA: u64 = 0x30; // 64-bit
const IR0_ERDP:   u64 = 0x38; // 64-bit

const XHCI_VECTOR: u8 = 0x21;

#[repr(C)]
#[derive(Clone, Copy)]
struct Trb {
    param: u64,
    status: u32,
    control: u32,
}

impl Trb {
    const ZERO: Self = Self { param: 0, status: 0, control: 0 };
}

const TRB_CYCLE: u32 = 1;

// TRB type field is bits [15:10]
const fn trb_type(t: u32) -> u32 { t << 10 }

const TRB_NORMAL:       u32 = trb_type(1);
const TRB_SETUP_STAGE:  u32 = trb_type(2);
const TRB_DATA_STAGE:   u32 = trb_type(3);
const TRB_STATUS_STAGE: u32 = trb_type(4);
const TRB_LINK:         u32 = trb_type(6);

const TRB_ENABLE_SLOT:    u32 = trb_type(9);
const TRB_DISABLE_SLOT:   u32 = trb_type(10);
const TRB_ADDRESS_DEVICE: u32 = trb_type(11);
const TRB_CONFIGURE_EP:   u32 = trb_type(12);
const TRB_EVALUATE_CONTEXT: u32 = trb_type(13);
const TRB_RESET_ENDPOINT: u32 = trb_type(14);
const TRB_STOP_ENDPOINT:  u32 = trb_type(15);
const TRB_SET_TR_DEQUEUE: u32 = trb_type(16);

const EVENT_TRANSFER:     u32 = 32;
const EVENT_CMD_COMPLETE: u32 = 33;
const EVENT_PORT_STATUS_CHANGE: u32 = 34;

// CC_SHORT_PACKET is success with a residue, not an error — treating it as one is the classic mass-storage bug.
const CC_SUCCESS: u32 = 1;
const CC_STALL: u32 = 6;
const CC_SHORT_PACKET: u32 = 13;

/// A completion code, named where xHCI 1.2 §Table 6-90 names it; an unnamed code still keeps its number.
#[derive(Clone, Copy)]
struct Completion(u32);

impl core::fmt::Display for Completion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let named = match self.0 {
            1 => "Success",
            2 => "Data Buffer Error",
            3 => "Babble Detected",
            4 => "USB Transaction Error",
            5 => "TRB Error",
            6 => "Stall Error",
            7 => "Resource Error",
            8 => "Bandwidth Error",
            9 => "No Slot Available",
            10 => "Invalid Stream Type",
            11 => "Slot Not Enabled",
            12 => "Endpoint Not Enabled",
            13 => "Short Packet",
            14 => "Ring Underrun",
            15 => "Ring Overrun",
            16 => "VF Event Ring Full",
            17 => "Parameter Error",
            18 => "Bandwidth Overrun",
            19 => "Context State Error",
            20 => "No Ping Response",
            21 => "Event Ring Full",
            22 => "Incompatible Device",
            23 => "Missed Service Error",
            24 => "Command Ring Stopped",
            25 => "Command Aborted",
            26 => "Stopped",
            27 => "Stopped - Length Invalid",
            28 => "Stopped - Short Packet",
            29 => "Max Exit Latency Too Large",
            31 => "Isoch Buffer Overrun",
            32 => "Event Lost",
            33 => "Undefined Error",
            34 => "Invalid Stream ID",
            35 => "Secondary Bandwidth Error",
            36 => "Split Transaction Error",
            _ => return write!(f, "code {}", self.0),
        };
        write!(f, "code {} ({named})", self.0)
    }
}

/// What the controller's answer to the one outstanding operation is for; no variant may be done by spinning inside a scheduler pass.
enum What {
    /// Disable Slot, and what stops being reachable once it completes.
    SlotGone { slot: u8, then: AfterSlot },
    /// One step of a HID endpoint's recovery; `seq` carries where it is, `issued` names the command for failure lines.
    Recovering { slot_id: u8, seq: Recovery, issued: &'static str },
    /// Enable Slot for a port that finished reset; there is no device until the controller answers.
    SlotWanted { port_idx: u8, speed: u8, packet: u16, seq: toyos_xhci::enumerate::Enumeration },
    /// One act of a device's enumeration.
    Enumerating(device::Enumerating),
}

/// What the controller said about one outstanding operation, for a refusal line.
struct Answer(Outcome);

impl core::fmt::Display for Answer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Outcome::Command { code, .. } | Outcome::Transfer { code, .. } => {
                write!(f, "{}", Completion(code))
            }
            Outcome::Silent => {
                write!(f, "no answer in {} ms", USB_TIMEOUT_NS / 1_000_000)
            }
        }
    }
}

/// Which device a line is about: the controller and the slot on it — a slot id alone is ambiguous, since a machine has more than one controller.
#[derive(Clone, Copy)]
struct Slot {
    bus: u8,
    dev: u8,
    func: u8,
    id: u8,
}

impl core::fmt::Display for Slot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:02x}:{:02x}.{} slot {}", self.bus, self.dev, self.func, self.id)
    }
}

/// Why a slot was given back, which is what decides what goes with it.
enum AfterSlot {
    /// A port's device has left the bus; the port may be enumerated again.
    Teardown(u8),
    /// Given up on while still plugged in; the port stays marked attached — see [`XhciController::let_go`].
    LetGo,
    /// Enumeration ended in refusal with the device still plugged in; the port stays attached so it is not re-enumerated every debounce.
    Refused,
}

/// The earlier of two instants something wants to be looked at again.
fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (at, None) | (None, at) => at,
    }
}

/// Where a control transfer's completions come from: addresses, not a count — [`Await::Transfer`] matches by TRB address.
#[derive(Clone, Copy)]
struct ControlTrbs {
    /// The Data Stage TRB, present only for a transfer that carries data.
    data: Option<u64>,
    /// The Status Stage TRB, every control transfer's device verdict.
    status: u64,
}

impl ControlTrbs {
    /// What the driver waits for, as [`Outstanding::submit`] wants it.
    fn awaits(self, slot: u8) -> (Await, Stages) {
        let on = |trb| Await::Transfer { slot, dci: 1, trb };
        match self.data {
            Some(data) => (on(data), Stages::DataThenStatus(on(self.status))),
            None => (on(self.status), Stages::One),
        }
    }
}

/// Put one control transfer's TRBs on an EP0 ring, and say where each completion will come from.
fn enqueue_control(
    ring: &mut TrbRing,
    bm_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
    data_buf: Option<u64>,
    data_len: u16,
) -> ControlTrbs {
    let is_in = (bm_request_type & 0x80) != 0;
    let has_data = data_len > 0 && data_buf.is_some();
    let trt = if !has_data { 0u32 } else if is_in { 3 } else { 2 };

    let mut setup = Trb::ZERO;
    setup.param = setup_packet(bm_request_type, b_request, w_value, w_index, data_len);
    setup.status = 8;
    setup.control = TRB_SETUP_STAGE | (1 << 6) | (trt << 16);
    ring.enqueue(setup);

    let data_at = data_buf.filter(|_| has_data).map(|buf| {
        let mut data = Trb::ZERO;
        data.param = buf;
        data.status = data_len as u32;
        let dir = if is_in { 1u32 << 16 } else { 0 };
        // ISP and IOC both required: without IOC the data stage produces no event, without ISP a short answer is not reported.
        data.control = TRB_DATA_STAGE | dir | (1 << 2) | (1 << 5);
        ring.enqueue(data)
    });

    let mut status = Trb::ZERO;
    let status_dir = if has_data && is_in { 0 } else { 1u32 << 16 };
    status.control = TRB_STATUS_STAGE | (1 << 5) | status_dir;
    ControlTrbs { data: data_at, status: ring.enqueue(status) }
}

/// The line for an endpoint no sequence of commands takes back to Running.
fn log_unrecoverable(slot: Slot, dci: u8, state: EndpointState) {
    log!("xHCI: {slot} endpoint {dci} is {state}; nothing short of Configure Endpoint \
         takes an endpoint out of that, and this driver does not re-configure a bound device");
}

/// How many *consecutive* transfer failures let an interrupt endpoint's device go; a delivered report clears the count.
const MAX_HID_FAILURES: u8 = 8;

/// How long the driver waits on any one command or transfer.
///
/// Not proof of a dead device: under TCG the TSC tracks host wall-clock, so a live device on an oversubscribed host can breach this once; only three breaches in one command are treated as a device fact.
///
/// USB_TIMEOUT_NS and `block::OPERATION` are the same 2 s; the two must stay equal or the budget math (`Scsi::Budget` → `BlockError::BudgetExpired`) is wrong.
const USB_TIMEOUT_NS: u64 = 2_000_000_000;

/// When a wait started now would give up; unbounded before `clock::init`, since `nanos_since_boot` stays 0.
fn deadline() -> u64 {
    crate::clock::nanos_since_boot() + USB_TIMEOUT_NS
}

/// Let a test starve one of those waits on a controller that otherwise answers perfectly; a kernel feature because QEMU cannot stage a register bit that never settles.
fn controller_answers() -> bool {
    !crate::actuator::xhci_deaf_controller()
}

fn port_answers() -> bool {
    !crate::actuator::xhci_deaf_port()
}

/// How long a mass-storage bulk transfer's completion is held back before the driver may see it.
///
/// A kernel feature: QEMU cannot stage a slow-answering drive, only a failing one.
const SLOW_TRANSFER_NS: u64 = 2_000_000;

/// The boot-time connect settle reads the same interval the per-port machine uses.
use portmachine::DEBOUNCE_NS as PORT_DEBOUNCE_NS;


/// Report an empty root hub for the first [`SLOW_CONNECT_NS`] of the boot; a kernel feature since QEMU cannot stage a port that connects late.
///
/// Replaces the register, not a verdict: the port reads exactly as unpopulated during the window.
const SLOW_CONNECT_NS: u64 = 300_000_000;

/// Report *one* root-hub port empty for the first [`SLOW_CONNECT_NS`], while every other port reads normally.
///
/// The window closes on the boot scan, not the clock: what it stages is an ordering, not a duration.
const SLOW_STORAGE_PORT: u8 = 0;

/// Whether the boot port scan has run; until it has, [`SLOW_STORAGE_PORT`] reads unpopulated.
pub(super) static BOOT_SCAN_DONE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// One bit per root-hub port; four words cover every MaxPorts a byte can express.
type PortMask = [u64; 4];

fn port_bit(mask: &PortMask, port_idx: u8) -> bool {
    mask[port_idx as usize / 64] & (1 << (port_idx % 64)) != 0
}

/// When some controller's port state machine must be stepped again, or 0 for none; neither reader may take [`XHCI`].
///
/// A CPU with nothing else to run must not sleep while this is set — nothing else would wake it for deferred port work.
static PORT_WORK_AT: AtomicU64 = AtomicU64::new(0);

/// Whether a CPU with nothing to run must stay awake for [`PORT_WORK_AT`].
pub fn port_work_pending() -> bool {
    PORT_WORK_AT.load(Ordering::Relaxed) != 0
}

/// Read every root-hub port again now, and step whatever has changed since.
///
/// For a caller with a reason of its own to keep looking: the boot scan is not a census, and [`poll_if_pending`] stops looking once it stores zero.
pub fn recheck_ports() {
    let mut wake_at: Option<u64> = None;
    for ctrl in XHCI.lock().iter_mut() {
        ctrl.ports_dirty = true;
        if let Some(at) = ctrl.poll() {
            wake_at = Some(wake_at.map_or(at, |w: u64| w.min(at)));
        }
    }
    PORT_WORK_AT.store(wake_at.unwrap_or(0), Ordering::Relaxed);
}

const RING_SIZE: usize = 256; // TRBs per ring (one page = 256 * 16)

/// Clear `len` bytes of `dma` at `off`, and answer with the region that was cleared.
pub(super) fn zero_dma<'pool>(dma: Dma<'pool>, off: usize, len: usize) -> Dma<'pool> {
    let region = dma.subview(off, len);
    region.zero();
    region
}

/// Point the DCBAA's `slot` entry at `phys`, or clear it with zero — the DCBAA's one writer. Slot 0 is the scratchpad array pointer, not a device context, which is why `init_one` is also a caller of `write_dcbaa`.
pub(super) fn write_dcbaa(dma: Dma<'_>, slot: usize, phys: u64) {
    // Volatile: the controller reads this array on any slot id, so the store may not be reordered against the command that follows.
    dma.write::<u64>(OFF_DCBAA + slot * core::mem::size_of::<u64>(), phys)
}

/// Event Ring Segment Table entry (16 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct ErstEntry {
    ring_base: u64,
    ring_size: u32,
    _reserved: u32,
}

/// `buf` and not a raw pointer: a [`Dma`] view carries the length that [`TrbRing::put`] checks each TRB against.
#[derive(Clone, Copy)]
struct TrbRing {
    buf: Dma<'static>,
    base_phys: u64,
    tail: u16,
    cycle: bool,
}

impl TrbRing {
    /// A fresh ring, zeroed with the wrap link TRB at the last slot; also the recovery primitive, paired with a Set TR Dequeue Pointer naming [`Self::dequeue`].
    fn init(buf: Dma<'static>) -> Self {
        assert!(buf.size() >= RING_SIZE * core::mem::size_of::<Trb>());
        zero_dma(buf, 0, buf.size());
        let ring = Self { buf, base_phys: buf.phys(), tail: 0, cycle: true };
        let mut link = Trb::ZERO;
        link.param = buf.phys();
        link.control = TRB_LINK | (1 << 1); // TC (Toggle Cycle)
        ring.put(RING_SIZE - 1, link);
        ring
    }

    /// One TRB at ring index `at` — the ring's one writer.
    fn put(&self, at: usize, trb: Trb) {
        // Volatile: the controller reads this ring concurrently, and the Cycle bit tells it the TRB is complete.
        self.buf.write(at * core::mem::size_of::<Trb>(), trb)
    }

    /// Where the controller should resume, with the cycle state it must expect; bit 0 carries the cycle since a TRB address is 16-byte aligned.
    fn dequeue(&self) -> u64 {
        (self.base_phys + (self.tail as u64) * 16) | (self.cycle as u64)
    }

    /// Put `trb` on the ring and answer with where it landed — the only name a Command Completion or Transfer Event carries (xHCI 1.2 §6.4.2.1–2).
    fn enqueue(&mut self, mut trb: Trb) -> u64 {
        if self.cycle {
            trb.control |= TRB_CYCLE;
        } else {
            trb.control &= !TRB_CYCLE;
        }
        let at = self.base_phys + (self.tail as u64) * 16;
        self.put(self.tail as usize, trb);
        self.tail += 1;

        if self.tail as usize >= RING_SIZE - 1 {
            let mut link = Trb::ZERO;
            link.param = self.base_phys;
            link.control = TRB_LINK | (1 << 1); // TC (Toggle Cycle)
            if self.cycle { link.control |= TRB_CYCLE; }
            self.put(self.tail as usize, link);
            self.tail = 0;
            self.cycle = !self.cycle;
        }
        at
    }
}

/// The granularity every structure below is placed at, asserted against the controller's own PAGESIZE rather than assumed.
const PAGE: usize = 0x1000;

// The pool's fixed head: one of each, since enumeration is serial — see `device::init_device`.
#[allow(clippy::erasing_op)]
const OFF_DCBAA: usize     = 0 * PAGE; // (max_slots + 1) * 8, 2 KiB at most
#[allow(clippy::identity_op)]
const OFF_CMD_RING: usize  = 1 * PAGE;
const OFF_ERST: usize      = 2 * PAGE;
const OFF_EVT_RING: usize  = 3 * PAGE;
const OFF_INPUT_CTX: usize = 4 * PAGE; // 33 contexts, so 2112 B at ctx_size 64
const OFF_DATA_BUF: usize  = 5 * PAGE;
const SHARED_SIZE: usize   = 6 * PAGE;

// One block per device; sharing any of these four regions between two devices is a silent data race.
/// The Device Context Index of the default control pipe, 1 for every device (xHCI 1.2 §4.5.1).
const EP0_DCI: u8 = 1;

const DEV_INT_RING: usize = 0;                 // 256 TRBs, exactly one page
const DEV_EP0_RING: usize = PAGE;              // likewise
const DEV_OUT_CTX: usize  = 2 * PAGE;          // 32 contexts, 2 KiB at ctx_size 64
const DEV_REPORT: usize   = 2 * PAGE + 0x800;  // 8 B, the largest boot report
const DEV_STRIDE: usize   = 3 * PAGE;

// Separate from the device block: folding this into DEV_STRIDE would cost every device 64 KiB it never touches.
const MSC_IN_RING: usize   = 0;
const MSC_OUT_RING: usize  = PAGE;
const MSC_CBW: usize       = 2 * PAGE;         // 31 B
const MSC_CSW: usize       = 2 * PAGE + 0x40;  // 13 B
const MSC_SCRATCH: usize   = 2 * PAGE + 0x80;  // INQUIRY, READ CAPACITY, sense
const MSC_SCRATCH_LEN: usize = 64;
/// The bulk data buffer, placed so it cannot cross a 64 KiB boundary — the one placement rule an xHCI Normal TRB's buffer has.
const MSC_DATA: usize      = 8 * PAGE;
const MSC_DATA_LEN: usize  = 8 * PAGE;
const MSC_STRIDE: usize    = 16 * PAGE;

/// Mass-storage devices the pool has blocks for; a third stick is refused rather than served from somebody else's block.
const MSC_BLOCKS: usize = 2;

/// The largest run of 4 KiB blocks one SCSI command moves; every caller-facing loop batches to it.
const MSC_MAX_BLOCKS: u32 = (MSC_DATA_LEN / 4096) as u32;

/// Device blocks to size the pool for before the controller's slot count is consulted; without this floor a scratchpad demand near a 2 MiB boundary can leave zero room for devices.
const MIN_DEVICE_BLOCKS: usize = 8;

/// Cap the driver at one device block, so a test can drive the slot-pool-full path; QEMU's `slots=N` cannot stage it since Enable Slot ignores MaxSlotsEn.
fn device_ceiling() -> usize {
    if crate::actuator::xhci_one_slot() {
        1
    } else {
        usize::MAX
    }
}

/// Where each structure sits in the pool, derived from what the controller reported.
#[derive(Clone, Copy)]
struct Layout {
    scratch_array: usize,
    scratch_buffers: usize,
    scratch_count: usize,
    msc_base: usize,
    dev_base: usize,
    /// Device blocks the pool holds, also the MaxSlotsEn written to CONFIG.
    dev_blocks: usize,
    pool_size: usize,
}

impl Layout {
    /// `max_scratchpad` and `max_slots` come straight off HCSPARAMS, bounded so the plain `align_2m` below cannot overflow — unlike every other caller of a size from outside the kernel, which needs `align_2m_checked`.
    fn new(max_scratchpad: usize, max_slots: u8) -> Self {
        let scratch_array = SHARED_SIZE;
        let array_bytes = (max_scratchpad * 8 + PAGE - 1) & !(PAGE - 1);
        let scratch_buffers = scratch_array + array_bytes;
        // Ahead of the device array, not behind it, so the device array still absorbs the pool's slack.
        let msc_base =
            (scratch_buffers + max_scratchpad * PAGE + MSC_STRIDE - 1) & !(MSC_STRIDE - 1);
        let dev_base = msc_base + MSC_BLOCKS * MSC_STRIDE;

        // DmaPool hands out whole 2 MiB pages; the floor decides how many pages, the slack decides how many device blocks.
        let pool_size = crate::mm::align_2m(dev_base + MIN_DEVICE_BLOCKS * DEV_STRIDE);
        let dev_blocks = ((pool_size - dev_base) / DEV_STRIDE)
            .min(max_slots as usize)
            .min(device_ceiling());

        Self {
            scratch_array,
            scratch_buffers,
            scratch_count: max_scratchpad,
            msc_base,
            dev_base,
            dev_blocks,
            pool_size,
        }
    }

    /// The block for a 1-based slot id, or `None` when the pool has no room; `slot_id` is untrusted, from the controller's own answer.
    ///
    /// `wrapping_sub` is sound only because `index` bounds the result: slot 0 wraps to `u8::MAX`, never `< dev_blocks`.
    fn device(&self, slot_id: u8) -> Option<usize> {
        let index = Untrusted::new(slot_id).map(|v| v.wrapping_sub(1)).index(self.dev_blocks).ok()?;
        Some(self.dev_base + index * DEV_STRIDE)
    }

    /// The `index`-th mass-storage block; private since only [`XhciController::claim_msc_block`] decides what `index` is.
    fn msc(&self, index: usize) -> usize {
        self.msc_base + index * MSC_STRIDE
    }
}

/// One mass-storage pool block, and whatever is holding it.
///
/// One struct and not two separate flags: a device refused between Configure Endpoint and READ CAPACITY would otherwise hold a block nothing names.
#[derive(Clone, Copy)]
struct MscBlock {
    /// The port whose device claimed this block, `None` while free; claimed before Configure Endpoint, given back only after the slot is disabled.
    port: Option<u8>,
    /// The disk, once `bring_up` produced one; `None` for a refused device, which is why `port` is what says taken.
    disk: Option<Disk>,
}

impl MscBlock {
    const FREE: Self = Self { port: None, disk: None };
}

/// A disk this controller brought up, and the number the machine knows it by.
#[derive(Clone, Copy)]
struct Disk {
    index: usize,
    dev: msc::MscDevice,
}

/// How many disks this machine has bound since boot, and the number the next bind hands out.
///
/// A counter and not a position, and never reused: `usb_storage::open` indexes by it and a mount holds it for the disk's whole life.
static DISKS_BOUND: AtomicUsize = AtomicUsize::new(0);

fn setup_packet(bm_request_type: u8, b_request: u8, w_value: u16, w_index: u16, w_length: u16) -> u64 {
    (bm_request_type as u64)
        | ((b_request as u64) << 8)
        | ((w_value as u64) << 16)
        | ((w_index as u64) << 32)
        | ((w_length as u64) << 48)
}

/// `Send` is derived, not asserted: every field is `Send` on its own, so no `unsafe impl` is owed.
pub struct XhciController {
    /// The function this controller is, so a log line can name which controller it means.
    pci: PciDevice,

    op_base: Mmio,
    db_base: Mmio,
    rt_base: Mmio,

    /// HCSPARAMS1's MaxPorts: every port register this controller has.
    max_ports: u8,

    /// When this controller's ports were powered, where [`PORT_DEBOUNCE_NS`] is measured from; kept per controller so two controllers' debounces overlap rather than add.
    powered_at: u64,

    context_size: usize, // 32 or 64
    layout: Layout,

    /// This controller's DMA, and this controller's only — a shared pool would collide two controllers' DCBAAs and command rings at the same address.
    ///
    /// Leaked late, only once `init_one`'s last refusal is behind it, so a declined controller still gives its pages back.
    ///
    /// A `DmaPool` field here plus a `TrbRing` borrowing it would be a self-reference, which is what forces the leaked `Dma<'static>` instead.
    pool: Dma<'static>,

    cmd_ring: TrbRing,

    /// The event ring as a region, not a pointer, so `next_event`'s read is bounded against the page.
    event_ring: Dma<'static>,
    event_head: u16,
    event_phase: bool,

    devices: Vec<HidDevice>,

    /// This controller's mass-storage pool blocks and their disks.
    ///
    /// Claimed before Configure Endpoint, before the disk is known, since keying off a count of *bound* disks would hand a live endpoint's memory to the next one.
    msc: [MscBlock; MSC_BLOCKS],

    /// This controller's root-hub ports, sized from HCSPARAMS1 rather than fixed at 255.
    ports: Vec<PortState>,

    /// What each port register speaks, from the controller's Supported Protocol capabilities.
    protocols: Protocols,

    /// A Port Status Change Event arrived and the ports have not been read since; consumed by [`Self::poll`].
    ports_dirty: bool,

    /// The one operation this controller has been given and has not answered.
    ///
    /// Submitted and left, not spun on: a scheduler pass may not block to [`USB_TIMEOUT_NS`] against a device with nothing to answer.
    outstanding: Outstanding<What>,

    /// Ports this driver has written PED=1 to; a kernel feature since QEMU's PED write is a no-op and cannot be staged otherwise.
    ///
    /// Replaces the register, not a verdict: the port reads PED clear for every reader until reset (§4.19.1.1.3).
    software_disabled: PortMask,

    /// The event ring slot a slow device's completion is held in, and when it was first seen. See [`SLOW_TRANSFER_NS`].
    held_event: Option<(u16, u64)>,
}

impl XhciController {
    pub(super) fn dma(&self) -> Dma<'static> {
        self.pool
    }

    fn write_dcbaa(&self, slot: usize, phys: u64) {
        write_dcbaa(self.dma(), slot, phys);
    }

    /// This controller's `slot_id` as a [`Slot`]; never construct one from a bare slot id, since the controller is half the identity.
    fn slot(&self, id: u8) -> Slot {
        Slot { bus: self.pci.bus, dev: self.pci.dev, func: self.pci.func, id }
    }

    /// Every read of a port register in this driver, so no two callers can disagree about it.
    fn read_portsc(&self, port_idx: u8) -> Portsc {
        Portsc::from_raw(self.read_portsc_raw(port_idx))
    }

    fn read_portsc_raw(&self, port_idx: u8) -> u32 {
        let raw = self.op_base.read_u32(OP_PORT_BASE + port_idx as u64 * PORT_REG_SIZE);
        if crate::actuator::xhci_slow_connect() && crate::clock::nanos_since_boot() < SLOW_CONNECT_NS
        {
            return raw & !(PORTSC_CCS | PORTSC_PED | PORTSC_SPEED);
        }
        if crate::actuator::xhci_slow_storage_connect()
            && port_idx == SLOW_STORAGE_PORT
            && !BOOT_SCAN_DONE.load(core::sync::atomic::Ordering::Relaxed)
        {
            return raw & !(PORTSC_CCS | PORTSC_PED | PORTSC_SPEED);
        }
        if crate::actuator::xhci_portsc_rw1c() && port_bit(&self.software_disabled, port_idx) {
            return raw & !PORTSC_PED;
        }
        // Also masks PED: QEMU's SuperSpeed port reads Enabled instantly, so without this the actuator stages nothing.
        if crate::actuator::xhci_deaf_port() {
            return raw & !PORTSC_PED;
        }
        raw
    }

    /// Every write of a port register; takes a typed [`toyos_xhci::portsc::Write`], which offers no way to set PED, so disabling a port the driver is enabling is unreachable rather than asserted against.
    fn write_portsc(&mut self, port_idx: u8, write: toyos_xhci::portsc::Write) {
        let value = write.raw();
        if crate::actuator::xhci_portsc_rw1c() {
            let word = port_idx as usize / 64;
            let bit = 1u64 << (port_idx % 64);
            if value & PORTSC_PED != 0 {
                self.software_disabled[word] |= bit;
            }
            if value & PORTSC_PR != 0 {
                self.software_disabled[word] &= !bit;
            }
        }
        self.op_base.write_u32(OP_PORT_BASE + port_idx as u64 * PORT_REG_SIZE, value);
    }

    /// How many ports the driver has written PED=1 to.
    fn software_disabled_ports(&self) -> u32 {
        self.software_disabled.iter().map(|w| w.count_ones()).sum()
    }

    /// The port a slot's device is on, or `None` for a slot mid-enumeration — `device::finish` is what gives a port its slot.
    fn port_of_slot(&self, slot: u8) -> Option<u8> {
        self.ports
            .iter()
            .position(|p| p.slot().map(NonZeroU8::get) == Some(slot))
            .map(|at| at as u8)
    }

    fn connected_ports(&self) -> PortMask {
        let mut mask = [0u64; 4];
        for p in 0..self.max_ports {
            if self.read_portsc(p).connected() {
                mask[p as usize / 64] |= 1 << (p % 64);
            }
        }
        mask
    }

    /// Take a free mass-storage pool block for `port_idx`, as its index and byte offset; the only way to obtain one, so nothing is unrecorded.
    fn claim_msc_block(&mut self, port_idx: u8) -> Option<(usize, usize)> {
        let index = self.msc.iter().position(|block| block.port.is_none())?;
        self.msc[index].port = Some(port_idx);
        Some((index, self.layout.msc(index)))
    }

    /// How many blocks are spoken for.
    fn msc_blocks_taken(&self) -> usize {
        self.msc.iter().filter(|block| block.port.is_some()).count()
    }

    /// Put a command on the ring and ring the doorbell, answering with the address the completion will name it by.
    fn submit_command(&mut self, trb: Trb) -> u64 {
        let at = self.cmd_ring.enqueue(trb);
        fence(Ordering::Release);
        self.db_base.write_u32(0, 0);
        at
    }

    /// One event, or `None` while the controller has not published the next; every reader goes through here since the ring is one shared queue.
    fn next_event(&mut self) -> Option<Trb> {
        // Volatile so the poll observes the Cycle bit flipping; racing the controller by design (xHCI 1.2 §4.9.2).
        let event: Trb =
            self.event_ring.read(self.event_head as usize * core::mem::size_of::<Trb>());
        if ((event.control & 1) != 0) != self.event_phase {
            return None;
        }
        if crate::actuator::usb_slow_device() && !self.slow_device_would_have_answered(&event) {
            return None;
        }
        self.advance_event_ring();
        Some(event)
    }

    /// Whether a stick this slow would have answered yet; `true` for anything that is not a bound disk's bulk completion. See [`SLOW_TRANSFER_NS`].
    ///
    /// Keyed on ring position: the head does not advance while an event is held, so a second look finds the same first-seen time.
    fn slow_device_would_have_answered(&mut self, event: &Trb) -> bool {
        let slot = ((event.control >> 24) & 0xFF) as u8;
        let dci = ((event.control >> 16) & 0x1F) as u8;
        let is_disk_bulk = (event.control >> 10) & 0x3F == EVENT_TRANSFER
            && dci >= 2
            && self.msc.iter().any(|b| b.disk.is_some_and(|d| d.dev.slot_id() == slot));
        if !is_disk_bulk {
            return true;
        }
        let now = crate::clock::nanos_since_boot();
        let since = match self.held_event {
            Some((head, at)) if head == self.event_head => at,
            _ => {
                self.held_event = Some((self.event_head, now));
                now
            }
        };
        now.saturating_sub(since) >= SLOW_TRANSFER_NS
    }

    /// Give an event to the device it names; an interrupt completion dropped here leaves the device's ring empty for the life of the boot.
    ///
    /// A code other than Success or Short Packet is recorded rather than dropped, so [`Self::recover_endpoints`] can act on it.
    fn dispatch_event(&mut self, event: Trb) {
        let trb_type = (event.control >> 10) & 0x3F;
        let code = (event.status >> 24) & 0xFF;
        let slot = ((event.control >> 24) & 0xFF) as u8;

        // The outstanding operation first: both event kinds name their TRB in the first two dwords, low 4 bits reserved and masked out (§6.4.2.1–2).
        let answers = match trb_type {
            EVENT_CMD_COMPLETE => Some((Await::Command { trb: event.param & !0xF }, slot as u32)),
            EVENT_TRANSFER => Some((
                Await::Transfer {
                    slot,
                    dci: ((event.control >> 16) & 0x1F) as u8,
                    trb: event.param & !0xF,
                },
                event.status & 0x00FF_FFFF,
            )),
            _ => None,
        };
        if answers.is_some_and(|(on, param)| self.outstanding.answered(on, code, param)) {
            return;
        }
        // The port id the event carries is not read — the register is what says what a port is, and the event is only a reason to look.
        if trb_type == EVENT_PORT_STATUS_CHANGE {
            self.ports_dirty = true;
            return;
        }
        if trb_type != EVENT_TRANSFER {
            return;
        }
        // Skip: a mid-recovery endpoint's ring is about to be rebuilt, and requeueing now would land a TRB where the dequeue pointer is not.
        if matches!(self.outstanding.what(), Some(What::Recovering { slot_id, .. }) if *slot_id == slot)
        {
            return;
        }
        let Some(at) = self.devices.iter().position(|d| d.slot_id == slot) else {
            return;
        };
        #[cfg(feature = "boot-actuators")]
        let code = self.devices[at].stage_break(code);
        let dev = &mut self.devices[at];
        if code == CC_SUCCESS || code == CC_SHORT_PACKET {
            dev.failures = 0;
            dev.dispatch_report();
            dev.requeue(&self.db_base);
            return;
        }
        dev.broke_with = Some(code);
    }

    /// Start recovering one HID endpoint a completion code broke, if the controller is free.
    ///
    /// Separate from `dispatch_event`, which runs inside another caller's wait: a recovery issued there would consume that caller's completion.
    ///
    /// One recovery at a time and never more: `Self::outstanding` is one slot, so a second device's recovery is owed until the first is answered.
    fn recover_endpoints(&mut self) {
        if self.outstanding.busy() {
            return;
        }
        let Some((slot_id, code)) =
            self.devices.iter().find_map(|d| Some((d.slot_id, d.broke_with?)))
        else {
            return;
        };
        self.recover_hid(slot_id, code);
    }

    /// One HID device's interrupt endpoint, on its way back to Running — or off the bus.
    fn recover_hid(&mut self, slot_id: u8, code: u32) {
        let Some(at) = self.devices.iter().position(|d| d.slot_id == slot_id) else {
            return;
        };
        // Stays on the list: a device not on it is one a port teardown cannot find.
        let dev = &mut self.devices[at];
        dev.broke_with = None;
        let kind = dev.kind();
        let (ep_addr, dci, port_idx, block) =
            (dev.ep_addr, dev.int_ep_dci, dev.port_idx, dev.block);

        // Disconnect wins the race: a transaction-error code from a pulled device is indistinguishable from a bad cable, only the port register tells them apart.
        //
        // CSC as well as CCS: a replug reads connected again but the transfer still died with the old device.
        let slot = self.slot(slot_id);
        let portsc = self.read_portsc(port_idx);
        if !portsc.connected() || portsc.connect_changed() {
            log!("xHCI: USB {kind} on {slot}: interrupt endpoint {ep_addr:#04x} \
                 completed with {} as its port went away; leaving it to the disconnect",
                Completion(code));
            return;
        }

        let dev = &mut self.devices[at];
        dev.failures += 1;
        let failures = dev.failures;
        log!("xHCI: USB {kind} on {slot}: interrupt endpoint {ep_addr:#04x} (dci {dci}) \
             completed with {}; failure {failures} of {MAX_HID_FAILURES}",
            Completion(code));

        if failures >= MAX_HID_FAILURES {
            self.let_go(at, format_args!("it has failed {MAX_HID_FAILURES} transfers in a row"));
            return;
        }
        let state = self.endpoint_state(block, dci);
        log!("xHCI: {slot} endpoint {dci} is {state}, recovering");
        match Recovery::begin(state) {
            Ok((seq, act)) => self.step_recovery(slot_id, seq, act),
            Err(NeedsConfigure(state)) => {
                log_unrecoverable(slot, dci, state);
                self.let_go(at, format_args!("endpoint {ep_addr:#04x} could not be restarted"));
            }
        }
    }

    /// Perform one act of a HID endpoint's recovery; nothing here waits — the completion arrives through the poll's own drain.
    fn step_recovery(&mut self, slot_id: u8, seq: Recovery, act: Act) {
        let Some(at) = self.devices.iter().position(|d| d.slot_id == slot_id) else {
            return;
        };
        let slot = self.slot(slot_id);
        let dev = &mut self.devices[at];
        let (dci, ep_addr, ring_at) = (dev.int_ep_dci, dev.ep_addr, dev.block + DEV_INT_RING);
        match act {
            Act::Running => {
                dev.requeue(&self.db_base);
                log!("xHCI: {slot} endpoint {dci} is delivering again");
            }
            Act::Command(cmd) => {
                // Copied out and back rather than borrowed: `recovery_trb` reads the pool through `self` in this window.
                let mut ring = dev.int_ring;
                let trb = self.recovery_trb(cmd, slot_id, dci, &mut ring, ring_at);
                self.devices[at].int_ring = ring;
                let on = Await::Command { trb: self.submit_command(trb) };
                let what = What::Recovering { slot_id, seq, issued: cmd.name() };
                self.outstanding.submit(what, on, Stages::One, deadline());
            }
            Act::ClearHalt => {
                let trbs = enqueue_control(
                    &mut self.devices[at].ep0_ring, 0x02, 0x01, 0, ep_addr as u16, None, 0,
                );
                self.ring_doorbell(slot_id, 1);
                let (on, stages) = trbs.awaits(slot_id);
                let what =
                    What::Recovering { slot_id, seq, issued: "CLEAR_FEATURE(ENDPOINT_HALT)" };
                self.outstanding.submit(what, on, stages, deadline());
            }
        }
    }

    /// The controller answered a recovery step; ask the sequence what is owed next, or let the device go.
    fn recovery_stepped(
        &mut self,
        slot_id: u8,
        mut seq: Recovery,
        issued: &str,
        outcome: Outcome,
    ) {
        if outcome.succeeded() {
            let act = seq.completed();
            self.step_recovery(slot_id, seq, act);
            return;
        }
        log!("xHCI: {}: {issued} failed: {}", self.slot(slot_id), Answer(outcome));
        if let Some(at) = self.devices.iter().position(|d| d.slot_id == slot_id) {
            self.let_go(at, format_args!("its interrupt endpoint could not be restarted"));
        }
    }

    /// Drop a recovery outstanding on a port whose device has gone — a transfer error there belongs to the disconnect, not the endpoint.
    fn cancel_recovery_on(&mut self, port_idx: u8) {
        let Some(What::Recovering { slot_id, .. }) = self.outstanding.what() else {
            return;
        };
        let slot_id = *slot_id;
        if !self.devices.iter().any(|d| d.slot_id == slot_id && d.port_idx == port_idx) {
            return;
        }
        self.outstanding.cancel();
        log!("xHCI: {}'s endpoint recovery is abandoned; its port has gone", self.slot(slot_id));
    }

    /// Everything a HID device the driver has given up on leaves behind.
    ///
    /// Unlike [`Self::teardown_port`], the port stays marked attached: clearing it would enumerate the same endpoint again every debounce.
    ///
    /// The port's slot is this device's: one root-hub port carries one device here, and `parse_config` gives that device one function.
    fn let_go(&mut self, at: usize, why: core::fmt::Arguments) {
        let mut dev = self.devices.remove(at);
        log!("xHCI: USB {} on {} is being let go — {why}. Unplug it and plug it in again.",
            dev.kind(), self.slot(dev.slot_id));
        dev.unbind();
        if let Some(slot) = self.ports[dev.port_idx as usize].take_slot() {
            self.submit_disable_slot(slot.get(), AfterSlot::LetGo);
        }
    }

    /// The command `cmd` names against (`slot_id`, `dci`), rebuilding the ring where the command is Set TR Dequeue.
    fn recovery_trb(
        &self,
        cmd: recovery::Command,
        slot_id: u8,
        dci: u8,
        ring: &mut TrbRing,
        ring_at: usize,
    ) -> Trb {
        let mut trb = Trb::ZERO;
        let kind = match cmd {
            recovery::Command::ResetEndpoint => TRB_RESET_ENDPOINT,
            recovery::Command::StopEndpoint => TRB_STOP_ENDPOINT,
            recovery::Command::SetDequeue => {
                *ring = TrbRing::init(self.dma().subview(ring_at, PAGE));
                trb.param = ring.dequeue();
                TRB_SET_TR_DEQUEUE
            }
        };
        trb.control = kind | ((slot_id as u32) << 24) | ((dci as u32) << 16);
        trb
    }

    /// Clear whatever change flags one port is holding, using the value the caller already read — a fresh read here could clear a flag never looked at.
    fn acknowledge_port_change(&mut self, port_idx: u8, portsc: Portsc) {
        if portsc.any_change() {
            self.write_portsc(port_idx, portsc.neutral().acknowledging(portsc));
        }
    }

    /// Read a port and clear whatever change flags it is holding.
    fn acknowledge_port_read(&mut self, port_idx: u8) {
        let portsc = self.read_portsc(port_idx);
        self.acknowledge_port_change(port_idx, portsc);
    }

    /// The same for every port this controller has.
    fn acknowledge_port_changes(&mut self) {
        for p in 0..self.max_ports {
            self.acknowledge_port_read(p);
        }
    }

    /// Record what the boot scan's enumeration left behind, so hot-plug starts from it; recorded even with no device, since a successful Enable Slot is the controller's resource regardless.
    fn port_bound(&mut self, port_idx: u8, slot: Option<u8>) {
        self.ports[port_idx as usize].adopt(slot.and_then(NonZeroU8::new));
    }

    /// Step every port that is not where the driver left it, and say when it wants to be looked at again.
    ///
    /// One step per call, no wait: the enumeration it eventually runs is the same blocking `wait::boot::configure`.
    fn service_ports(&mut self) -> Option<u64> {
        let now = crate::clock::nanos_since_boot();
        (0..self.max_ports)
            .filter_map(|p| self.service_port(p, now))
            .min()
    }

    /// One port's step, and when it next wants one.
    ///
    /// The decision is [`PortState::step`]'s; every line here is an effect, and the register is re-read after each one.
    ///
    /// The bound is not a timeout — it catches a machine that loops, since one pass issues at most one effect per state it leaves.
    fn service_port(&mut self, port_idx: u8, now: u64) -> Option<u64> {
        const MAX_EFFECTS: usize = 16;
        for _ in 0..MAX_EFFECTS {
            let portsc = self.read_portsc(port_idx);
            // CCS or CSC: a replug between two looks reads connected again, but the device that was here has still gone.
            if !portsc.connected() || portsc.connect_changed() {
                self.cancel_recovery_on(port_idx);
                device::cancel_on(self, port_idx);
            }
            // A port inside a prior pass's effect is not decided about until the controller answers.
            //
            // The `expect` is a driver bug, not a device one: the only effect that outlives a pass is the one that filled the slot.
            if self.ports[port_idx as usize].working().is_some() {
                let at = self.outstanding.wake_at().expect(
                    "a port is inside an effect the controller was never asked to perform",
                );
                return Some(at);
            }
            // Read before the machine is asked, since its own borrow of this port is live after.
            let busy = self.outstanding.wake_at();
            match self.ports[port_idx as usize].step(portsc, now) {
                Step::Idle => return None,
                Step::Wait(at) => return Some(at),
                Step::GaveUp(why) => {
                    match why {
                        GaveUp::ResetNeverFinished(kind) => log!(
                            "xHCI: port {} never finished its {} reset (PORTSC {:#010x}); \
                             skipping it",
                            port_idx + 1,
                            match kind {
                                Reset::Hot => "hot",
                                Reset::Warm => "warm",
                            },
                            portsc.raw()
                        ),
                        // §4.19.1.2 has nothing further after a warm reset — this is the port's end.
                        GaveUp::LinkNeverTrained => log!(
                            "xHCI: port {} is SuperSpeed and its link would not train, warm reset \
                             included (PORTSC {:#010x}, link {:?}); skipping it",
                            port_idx + 1,
                            portsc.raw(),
                            portsc.link_state()
                        ),
                    }
                    return None;
                }
                Step::Write(write) => self.write_portsc(port_idx, write),
                Step::Reset(kind, write) => {
                    match kind {
                        Reset::Hot => log!("xHCI: port {} connected", port_idx + 1),
                        Reset::Warm => log!(
                            "xHCI: port {} warm reset, link was {:?}",
                            port_idx + 1,
                            portsc.link_state()
                        ),
                    }
                    self.write_portsc(port_idx, write);
                }
                Step::Teardown(why, pending) => {
                    if busy.is_some() {
                        return busy;
                    }
                    pending.running();
                    match why {
                        Gone::Disconnected => log!("xHCI: port {} disconnected", port_idx + 1),
                        Gone::Replugged => log!(
                            "xHCI: port {} was unplugged and plugged back in between two looks; \
                             tearing the old device down before enumerating what is there now",
                            port_idx + 1
                        ),
                    }
                    if self.teardown_port(port_idx) {
                        self.ports[port_idx as usize].torn_down();
                    } else {
                        // The slot is outstanding; this port is inside an effect until the controller answers.
                        return self.outstanding.wake_at();
                    }
                }
                Step::Enumerate { trained, pending } => {
                    // A slot pending Disable may be handed straight back by Enable Slot below; deferring avoids zeroing the new device's DCBAA entry.
                    if busy.is_some() {
                        return busy;
                    }
                    pending.running();
                    if trained {
                        // No reset needed: a SuperSpeed link trains itself, and resetting an already-Enabled port puts it into Inactive with no way back.
                        log!("xHCI: port {} connected, link already trained", port_idx + 1);
                    }
                    device::begin(self, port_idx);
                    // Either enumeration is under way and the port waits, or it refused before spending a command.
                    return self.outstanding.wake_at();
                }
            }
        }
        log!("xHCI: port {} produced {MAX_EFFECTS} effects without settling; leaving it",
            port_idx + 1);
        None
    }

    /// Everything a device that is no longer on the bus leaves behind: input first, then the slot, then the pool block — safe only in that order, since the slot's endpoint contexts still name that memory while it lives.
    ///
    /// `true` when the port is already empty; `false` when the controller still has to answer for the slot, and [`Self::slot_gone`] finishes it.
    fn teardown_port(&mut self, port_idx: u8) -> bool {
        while let Some(at) = self.devices.iter().position(|d| d.port_idx == port_idx) {
            let mut dev = self.devices.remove(at);
            let role = dev.role;
            dev.unbind();
            match role {
                hid::HidRole::Keyboard => log!(
                    "xHCI: USB keyboard on slot {} unplugged from port {}",
                    dev.slot_id, port_idx + 1
                ),
                // The source is logged because it is the only place the button merge is visible; a leaked entry reads the same otherwise.
                hid::HidRole::Pointer(source) => log!(
                    "xHCI: USB pointer on slot {} unplugged from port {}, source {} released",
                    dev.slot_id, port_idx + 1, source.id()
                ),
            }
        }
        let Some(slot) = self.ports[port_idx as usize].take_slot() else {
            self.release_blocks(port_idx);
            return true;
        };
        self.submit_disable_slot(slot.get(), AfterSlot::Teardown(port_idx));
        false
    }

    /// The pool blocks a port's device held, back in the pool — after the slot and never before, since the slot's endpoint contexts still name this memory.
    fn release_blocks(&mut self, port_idx: u8) {
        for at in 0..MSC_BLOCKS {
            if self.msc[at].port != Some(port_idx) {
                continue;
            }
            // The disk's number does not come back: a mount holds it for the disk's whole life.
            match core::mem::replace(&mut self.msc[at], MscBlock::FREE).disk {
                Some(disk) => log!("usb-storage: disk {} unplugged from port {}; it is offline",
                    disk.index, port_idx + 1),
                None => log!("usb-storage: the device this driver refused on port {} is gone; \
                    its pool block is free again", port_idx + 1),
            }
        }
    }

    /// Ask the controller for a slot back, and record what its answer is owed.
    ///
    /// Disable Slot takes a slot out of any state (xHCI 1.2 §4.6.4) — unlike Reset Endpoint, which is why it needs no state check first.
    fn submit_disable_slot(&mut self, slot_id: u8, then: AfterSlot) {
        let mut disable = Trb::ZERO;
        disable.control = TRB_DISABLE_SLOT | ((slot_id as u32) << 24);
        let on = Await::Command { trb: self.submit_command(disable) };
        self.outstanding.submit(What::SlotGone { slot: slot_id, then }, on, Stages::One, deadline());
    }

    /// The slot is the controller's again, or it is not and there is no second question to ask about it.
    fn slot_gone(&mut self, slot: u8, then: AfterSlot, outcome: Outcome) {
        if outcome.succeeded() {
            // After the command, never before: the controller may still be writing the output context until it completes.
            self.write_dcbaa(slot as usize, 0);
            log!("xHCI: slot {slot} disabled");
        } else {
            log!("xHCI: Disable Slot failed: {}", Answer(outcome));
        }
        // Blocks go back whatever the controller said: leaving them held for the boot's life on a repeated refusal exhausts the pool.
        if let AfterSlot::Teardown(port_idx) = then {
            self.release_blocks(port_idx);
            self.ports[port_idx as usize].torn_down();
        }
    }

    /// Act on whatever the controller has answered, and issue whatever that answer owes next.
    ///
    /// Never called from inside a wait: everything below submits commands and frees memory.
    fn advance_outstanding(&mut self) {
        let now = crate::clock::nanos_since_boot();
        while let Some((what, outcome)) = self.outstanding.finished(now) {
            match what {
                What::SlotGone { slot, then } => self.slot_gone(slot, then, outcome),
                What::Recovering { slot_id, seq, issued } => {
                    self.recovery_stepped(slot_id, seq, issued, outcome)
                }
                What::SlotWanted { port_idx, speed, packet, seq } => {
                    device::slot_answered(self, port_idx, speed, packet, seq, outcome)
                }
                What::Enumerating(state) => device::stepped(self, state, outcome),
            }
        }
    }

    /// Drain the event ring and step the ports, and say when this controller wants to be polled again.
    ///
    /// Ports are read only when something says they might have moved; otherwise this costs one event-ring read.
    fn poll(&mut self) -> Option<u64> {
        while let Some(event) = self.next_event() {
            self.dispatch_event(event);
        }
        // After the drain, not inside it: an answer the drain recorded is issued where nobody else waits on this ring.
        self.advance_outstanding();
        self.recover_endpoints();

        // Nothing below reads the event ring: every step `service_ports` takes is a submit, so one advance is enough.
        let mut wake_at = None;
        if self.ports_dirty || self.ports.iter().any(PortState::outstanding) {
            self.ports_dirty = false;
            wake_at = self.service_ports();
        }
        earliest(wake_at, self.outstanding.wake_at())
    }

    /// One dword of an input context: index 0 is the control context, 1 the slot context, `dci + 1` an endpoint's.
    ///
    /// `Endpoint::dci`'s field is private and 2..=31 by construction, bounding the largest reachable index to 32 — without it a struct literal could write 12,880 bytes in.
    fn write_ctx32(&self, ctx: Dma<'static>, ctx_index: usize, dword: usize, val: u32) {
        let offset = (ctx_index * self.context_size) + (dword * 4);
        // Volatile: the controller reads the input context as soon as the command naming it is enqueued.
        ctx.write::<u32>(offset, val)
    }

    /// The Endpoint State the controller published for (`dev_block`'s device, `dci`).
    ///
    /// Output contexts index endpoints by DCI directly, unlike the input context, whose Input Control Context shifts everything by one.
    fn endpoint_state(&self, dev_block: usize, dci: u8) -> EndpointState {
        let at = dev_block + DEV_OUT_CTX + dci as usize * self.context_size;
        // Volatile: the controller writes this dword by DMA, so it must be re-read rather than cached.
        EndpointState::decode(self.dma().read::<u32>(at))
    }

    fn advance_event_ring(&mut self) {
        self.event_head = (self.event_head + 1) % RING_SIZE as u16;
        if self.event_head == 0 {
            self.event_phase = !self.event_phase;
        }
        let erdp = self.dma().phys() + OFF_EVT_RING as u64 + (self.event_head as u64) * 16;
        self.rt_base.write_u64(IR0_ERDP, erdp | (1 << 3)); // EHB clears interrupt pending
        self.rt_base.write_u32(IR0_IMAN, 3); // clear IP (W1C) + keep IE
    }

    fn ring_doorbell(&self, slot: u8, dci: u8) {
        fence(Ordering::Release);
        self.db_base.write_u32(slot as u64 * 4, dci as u32);
    }

}

/// Every xHCI controller on the machine, in PCI enumeration order.
///
/// A `Vec` and not an `Option`: the target laptop has two, and its own ports hang off the second.
static XHCI: Lock<Vec<XhciController>> = Lock::new(Vec::new());

/// Process xHCI events if this CPU has an unserviced interrupt record, or a port's state machine is due.
///
/// Every controller is polled, not just the one that interrupted, because every controller shares the same vector and `irq_ring` records only that an xHC interrupted, never which.
///
/// Thread context only, and `drain_irqs`'s alone to call: it takes `XHCI`, a ticket spinlock with preemption off for its life, and can spin on deadlines measured in seconds while holding it.
pub fn poll_if_pending() {
    let interrupted = crate::irq_ring::pending(crate::irq_ring::IrqSource::Xhci);
    if !interrupted {
        match PORT_WORK_AT.load(Ordering::Relaxed) {
            0 => return,
            at if crate::clock::nanos_since_boot() < at => return,
            _ => {}
        }
    }
    // Decline rather than queue: `lock()` here would put every CPU with work due on one ticket spinlock.
    let Some(mut guard) = XHCI.try_lock() else { return };
    // Taken only now the work will be done: taking it before a declined lock would drop a wake with nothing left to re-post it.
    crate::irq_ring::take(crate::irq_ring::IrqSource::Xhci);
    let mut wake_at: Option<u64> = None;
    for ctrl in guard.iter_mut() {
        if let Some(at) = ctrl.poll() {
            wake_at = Some(wake_at.map_or(at, |w: u64| w.min(at)));
        }
    }
    PORT_WORK_AT.store(wake_at.unwrap_or(0), Ordering::Relaxed);
}

/// Everything above the controller needs to know about one bound disk.
#[derive(Clone, Copy, Debug)]
pub struct StorageGeometry {
    /// What the device itself addresses in, straight off READ CAPACITY.
    pub logical_block_bytes: u32,
    /// The same capacity in the 4 KiB blocks `BlockDevice` is written in.
    pub blocks: u64,
}

/// How many disk numbers this machine has issued; every value below it names a disk bound at some point in this boot.
pub fn storage_count() -> usize {
    DISKS_BOUND.load(Ordering::Relaxed)
}

/// Run `f` against the machine's `index`-th disk, wherever it is — a search, since neither its controller nor its block is derivable from the machine-wide number.
fn with_disk<R>(index: usize, f: impl FnOnce(&mut XhciController, usize) -> R) -> Option<R> {
    let mut guard = XHCI.lock();
    for ctrl in guard.iter_mut() {
        if let Some(at) = ctrl
            .msc
            .iter()
            .position(|block| block.disk.is_some_and(|d| d.index == index))
        {
            return Some(f(ctrl, at));
        }
    }
    None
}

/// The geometry of the machine's `index`-th disk, `None` where there is no such disk — including, after an unplug, an index that used to name one.
pub fn storage_geometry(index: usize) -> Option<StorageGeometry> {
    with_disk(index, |ctrl, at| Some(ctrl.msc[at].disk?.dev.geometry())).flatten()
}

/// Whether the machine's `index`-th disk is still being spoken to; `Some(false)` and not `None` for an unplugged one, since the caller already holds a handle.
#[cfg(feature = "boot-actuators")]
pub fn storage_online(index: usize) -> Option<bool> {
    (index < storage_count()).then(|| {
        with_disk(index, |ctrl, at| ctrl.msc[at].disk.is_some_and(|d| d.dev.online()))
            .unwrap_or(false)
    })
}

/// Under-deliver the next READ(10) on the disk the gate is driving. See [`msc::short_read`].
#[cfg(feature = "boot-actuators")]
pub fn arm_short_read() {
    msc::short_read::arm();
}

