//! The Intel HDA stub: bring-up, one output stream, and the allow-list.
//!
//! The kernel touches only registers whose value is an address, indexes a
//! kernel structure, or acknowledges the interrupt; everything else a driver
//! reaches goes through [`reg_write`] and [`reg_read`], gated by a positive
//! list and refused by name. Which controller carries the stream, and every
//! codec and pin decision, belong to soundserver, never to this file: [`init`]
//! lists every controller it can bind and [`bind`] binds the one it is told.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use toyos_abi::audio::AudioCompletionRecord;
use alloc::vec::Vec;

use toyos_abi::hda::{HdaController, HdaInfo, Reg, MAX_CONTROLLERS};
use toyos_abi::syscall::{RegWidth, SyscallError};
use toyos_hda::stream;

use super::pci::PciDevice;
use crate::device::ClaimError;
use crate::log;
use crate::mm::policy::{CachePolicy, MmioPolicy};
use crate::mm::Mmio;
use crate::object::shm::Region;
use crate::sync::Lock;
use crate::time::{Delay, Duration};

const CLASS_MULTIMEDIA: u8 = 0x04;
const SUBCLASS_HDA: u8 = 0x03;

const HEADER_COMMAND: u64 = 0x04;
const HEADER_BAR0: u64 = 0x10;
const COMMAND_MEMORY_SPACE: u16 = 1 << 1;

const CAP_POWER_MANAGEMENT: u8 = 0x01;
const PM_CONTROL_STATUS: u64 = 0x04;
const PM_STATE_MASK: u16 = 0x3;
/// The mandated D3hot-to-D0 recovery time; the device may not be asked anything during it.
const PM_D3HOT_RECOVERY: Delay = Delay::from_spec(
    Duration::from_millis(10),
    "PCI Power Management: the mandated D3hot-to-D0 recovery time",
);

const GCAP: u64 = 0x00;
const VMIN: u64 = 0x02;
const VMAJ: u64 = 0x03;
const GCTL: u64 = 0x08;
const STATESTS: u64 = 0x0E;
const INTCTL: u64 = 0x20;
const IMMEDIATE_COMMAND: u64 = 0x60;
const IMMEDIATE_RESPONSE: u64 = 0x64;
const IMMEDIATE_STATUS: u64 = 0x68;

const GCTL_CRST: u32 = 1 << 0;
const INTCTL_GIE: u32 = 1 << 31;

/// The first stream descriptor's offset; which one is the first output depends on `GCAP.ISS`.
const STREAM_BASE: u64 = 0x80;
const STREAM_STRIDE: u64 = 0x20;

const SD_CTL: u64 = 0x00;
/// The byte carrying the stream tag; the codec's converter must be told the same number.
const SD_CTL_TAG: u64 = 0x02;
const SD_STS: u64 = 0x03;
const SD_LPIB: u64 = 0x04;
const SD_CBL: u64 = 0x08;
const SD_LVI: u64 = 0x0C;
const SD_FMT: u64 = 0x12;
const SD_BDPL: u64 = 0x18;
const SD_BDPU: u64 = 0x1C;

const SD_CTL_SRST: u8 = 1 << 0;
const SD_CTL_RUN: u8 = 1 << 1;
const SD_CTL_IOCE: u8 = 1 << 2;
const SD_CTL_FEIE: u8 = 1 << 3;
const SD_CTL_DEIE: u8 = 1 << 4;

const SD_STS_BCIS: u8 = 1 << 2;
const SD_STS_FIFOE: u8 = 1 << 3;
const SD_STS_DESE: u8 = 1 << 4;
const SD_STS_WRITE_CLEAR: u8 = SD_STS_BCIS | SD_STS_FIFOE | SD_STS_DESE;

const PERIODS: usize = 8;
const PERIOD_BYTES: usize = 512;

/// The tag this stream carries; [`HdaInfo::stream_tag`] is how soundserver's verb names the same number.
const STREAM_TAG: u8 = 1;

/// The smallest register window that can hold a stream descriptor; a BAR under this is refused.
const MIN_BAR_BYTES: u64 = 0x1000;

/// How long a register bit is given to settle; well past the spec's microsecond figures, so an
/// expiry means the device stopped.
const SETTLE_NS: u64 = 100_000_000;

/// The delay between releasing `CRST` and believing `STATESTS`: 25 frames at 48 kHz.
const CODEC_DETECT: Delay = Delay::from_spec(
    Duration::from_millis(1),
    "HD Audio: 25 frames at 48kHz between releasing CRST and believing STATESTS",
);

/// How many refused register accesses are named in the log before the call still fails silently.
const MAX_NAMED_REFUSALS: usize = 16;


// No lock: the interrupt handler can run on a CPU already holding `CONTROLLER`.
struct StreamIsr {
    stream: UnsafeCell<Option<Mmio>>,
    /// Written by the handler while the stream runs, and by [`reg_write`] on the edge that starts
    /// it — never both, since a stopped stream raises no interrupt.
    last: AtomicUsize,
    /// Accumulating rather than a ring: an interrupt carries nothing a later one does not, so
    /// there is no queue for a slow reader to overflow.
    mask: AtomicU32,
    /// The newest interrupt's timestamp: the DLL measures a batch against its last grid point.
    timestamp: AtomicU64,
    /// Counted here and named once from the drain path: a handler that logs adds work for the
    /// thing that failed.
    errors: AtomicU32,
    named_error: AtomicBool,
}

// SAFETY: `stream` is written once, by the bind that left `Listed`, before its controller can
// raise the vector, and is read-only afterwards; every other field is atomic.
unsafe impl Sync for StreamIsr {}

static ISR: StreamIsr = StreamIsr {
    stream: UnsafeCell::new(None),
    last: AtomicUsize::new(0),
    mask: AtomicU32::new(0),
    timestamp: AtomicU64::new(0),
    errors: AtomicU32::new(0),
    named_error: AtomicBool::new(false),
};

fn isr_stream() -> Option<Mmio> {
    // SAFETY: sole writer is `bind_listed`, before any controller can raise the vector.
    unsafe { *ISR.stream.get() }
}

/// Acknowledge one stream interrupt and record which periods it covered.
pub fn isr_complete() {
    let timestamp = crate::clock::nanos_since_boot();
    let Some(stream) = isr_stream() else { return };

    let status = stream.read_u8(SD_STS);
    if status & SD_STS_WRITE_CLEAR == 0 {
        return;
    }
    // Cleared before SDnLPIB is read, so a period completing in between raises a fresh interrupt.
    stream.write_u8(SD_STS, status & SD_STS_WRITE_CLEAR);
    if status & (SD_STS_FIFOE | SD_STS_DESE) != 0 {
        ISR.errors.fetch_add(1, Ordering::Relaxed);
    }
    if status & SD_STS_BCIS == 0 {
        return;
    }

    let position = stream.read_u32(SD_LPIB);
    let last = ISR.last.load(Ordering::Relaxed);
    // A position the device cannot have yields no completion, rather than one masked to fit.
    let Some((mask, current)) = stream::completed(last, position, PERIOD_BYTES as u32, PERIODS)
    else {
        return;
    };
    ISR.last.store(current, Ordering::Relaxed);
    if mask == 0 {
        return;
    }
    ISR.timestamp.store(timestamp, Ordering::Relaxed);
    ISR.mask.fetch_or(mask, Ordering::Release);
    super::AUDIO_WATCH.post_in_place();
    crate::preempt::set_need_resched();
}

/// Are completions pending? Lock-free.
pub fn has_pending() -> bool {
    ISR.mask.load(Ordering::Acquire) != 0
}

/// Take the pending completions, or `None`.
fn take_completions() -> Option<AudioCompletionRecord> {
    let mask = ISR.mask.swap(0, Ordering::AcqRel);
    if mask == 0 {
        return None;
    }
    Some(AudioCompletionRecord {
        mask,
        _pad: 0,
        // Read after the mask, so never older than the newest period in it.
        timestamp_nanos: ISR.timestamp.load(Ordering::Acquire),
    })
}

/// Copy one completion record into `buf`, and name a device error the first
/// time one has been counted.
pub fn drain_completed(buf: &mut crate::user_ptr::UserBytesMut) -> usize {
    let errors = ISR.errors.load(Ordering::Relaxed);
    if errors != 0 && !ISR.named_error.swap(true, Ordering::Relaxed) {
        log!("hda: the stream reported a FIFO or descriptor error ({errors} so far)");
    }
    let Some(record) = take_completions() else { return 0 };
    let mut bytes = [0u8; AudioCompletionRecord::SIZE];
    bytes[0..4].copy_from_slice(&record.mask.to_le_bytes());
    bytes[8..16].copy_from_slice(&record.timestamp_nanos.to_le_bytes());
    buf.write_at(0, &bytes);
    AudioCompletionRecord::SIZE
}


/// One controller [`init`] can bind: out of reset with its link up, its message armed at the HDA
/// vector, and bus mastering off and `INTCTL` clear, so it raises nothing.
struct Listed {
    pci: PciDevice,
    regs: Mmio,
    /// Its output stream descriptor's own window.
    stream: Mmio,
    stream_index: u8,
    stream_offset: u32,
    statests: u16,
    message: Message,
}

/// Which of the function's two message capabilities [`arm_interrupt`] armed, so the one that is
/// turned off is the one that is on.
#[derive(Clone, Copy)]
enum Message {
    MsiX,
    Msi,
}

/// The stream's memory, taken once at boot and kept for it: its shape is the driver's constants,
/// the same for every controller, and every claim's PCM object maps the PCM pool.
#[derive(Clone)]
struct StreamMemory {
    space: crate::iommu::DeviceSpace,
    /// The descriptor list's device address, already written.
    bdl: u64,
    pcm: Region,
}

/// What every call on the HDA claim is decided by. One-shot: `Listed` → `Binding` → `Bound` or
/// `Refused`, and nothing returns to `Listed`.
enum State {
    /// Every controller the kernel can bind, in PCI order. The register calls reach `ICS`, `ICW`
    /// and `IRR` on any of them.
    Listed(Vec<Listed>),
    /// One bind owns every listed controller; every other call is refused, and nothing waits.
    Binding,
    /// The stream runs on this controller, which the claim lists alone, at index 0.
    Bound(Listed),
    /// The bind failed on the device; the log line names how.
    Refused,
}

struct Hda {
    state: State,
    memory: StreamMemory,
}

/// No device wait is made under this lock: [`bind`] moves the list out of it and runs its waits
/// with nothing held.
static CONTROLLER: Lock<Option<Hda>> = Lock::new(None);
static REFUSALS: AtomicUsize = AtomicUsize::new(0);

/// What an `hda-audio` claim describes: the stream's shape and the controllers it lists, read
/// from the one state every call is decided by.
pub fn describe() -> Result<(HdaInfo, Region), ClaimError> {
    let guard = CONTROLLER.lock();
    let hda = guard.as_ref().ok_or(ClaimError::Absent)?;
    let listed = match &hda.state {
        State::Listed(list) => list.as_slice(),
        State::Bound(controller) => core::slice::from_ref(controller),
        // The binder's frame holds the claim until the bind returns.
        State::Binding => return Err(ClaimError::Owned),
        State::Refused => return Err(ClaimError::Unusable),
    };
    let mut controllers = [HdaController {
        stream_offset: 0,
        statests: 0,
        bus: 0,
        dev: 0,
        func: 0,
        _pad: [0; 3],
    }; MAX_CONTROLLERS];
    for (slot, controller) in controllers.iter_mut().zip(listed) {
        *slot = HdaController {
            stream_offset: controller.stream_offset,
            statests: controller.statests,
            bus: controller.pci.bus,
            dev: controller.pci.dev,
            func: controller.pci.func,
            _pad: [0; 3],
        };
    }
    let info = HdaInfo {
        pcm: toyos_abi::HANDLE_INVALID,
        period_bytes: PERIOD_BYTES as u32,
        stream_tag: STREAM_TAG,
        periods: PERIODS as u8,
        count: listed.len() as u8,
        _pad: 0,
        controllers,
    };
    Ok((info, hda.memory.pcm.clone()))
}

/// A register is on the list only if its value is not an address and indexes nothing the kernel
/// allocated; `SDnCBL` and `SDnLVI` are absent because the kernel writes them itself. `stream` is
/// the bound controller's descriptor offset, and `None` before a bind: an unbound controller has
/// no stream for a driver to touch.
fn write_permit(stream: Option<u64>, offset: u64, width: RegWidth) -> Result<&'static str, ()> {
    let sd = |field: u64| stream.map(|at| at + field);
    match (offset, width) {
        (IMMEDIATE_COMMAND, RegWidth::U32) => Ok("ICW: a codec verb, which names no memory"),
        (IMMEDIATE_STATUS, RegWidth::U16) => Ok("ICS: the immediate-command busy and valid bits"),
        (o, RegWidth::U8) if Some(o) == sd(SD_CTL) => Ok("SDnCTL: run and interrupt enables"),
        (o, RegWidth::U8) if Some(o) == sd(SD_CTL_TAG) => Ok("SDnCTL: the stream tag"),
        (o, RegWidth::U16) if Some(o) == sd(SD_FMT) => Ok("SDnFMT: the format word"),
        _ => Err(()),
    }
}

/// The two registers a driver has to poll; everything else is either in [`HdaInfo`] or the
/// kernel's own.
fn read_permit(offset: u64, width: RegWidth) -> Result<&'static str, ()> {
    match (offset, width) {
        (IMMEDIATE_STATUS, RegWidth::U16) => Ok("ICS"),
        (IMMEDIATE_RESPONSE, RegWidth::U32) => Ok("IRR"),
        _ => Err(()),
    }
}

fn refuse(what: &str, reg: Reg, width: RegWidth) -> SyscallError {
    if REFUSALS.fetch_add(1, Ordering::Relaxed) < MAX_NAMED_REFUSALS {
        log!(
            "hda: refused a {width:?} {what} of {:#x} on controller {} — not on the allow-list",
            reg.offset,
            reg.controller
        );
    }
    SyscallError::PermissionDenied
}

/// `f` over the controller `reg` names in the claim's current list, and whether the stream is
/// bound to it, for one register access under [`CONTROLLER`].
fn on_controller<R>(
    reg: Reg,
    f: impl FnOnce(&Listed, bool) -> Result<R, SyscallError>,
) -> Result<R, SyscallError> {
    let guard = CONTROLLER.lock();
    let hda = guard.as_ref().ok_or(SyscallError::NotFound)?;
    match &hda.state {
        State::Listed(list) => f(list.get(reg.controller as usize).ok_or(SyscallError::InvalidArgument)?, false),
        State::Bound(controller) if reg.controller == 0 => f(controller, true),
        State::Bound(_) => Err(SyscallError::InvalidArgument),
        State::Binding => Err(SyscallError::WouldBlock),
        State::Refused => Err(SyscallError::Io),
    }
}

pub fn reg_read(reg: Reg, width: RegWidth) -> Result<u32, SyscallError> {
    if read_permit(reg.offset as u64, width).is_err() {
        return Err(refuse("read", reg, width));
    }
    on_controller(reg, |controller, _| {
        let offset = reg.offset as u64;
        Ok(match width {
            RegWidth::U8 => controller.regs.read_u8(offset) as u32,
            RegWidth::U16 => controller.regs.read_u16(offset) as u32,
            RegWidth::U32 => controller.regs.read_u32(offset),
        })
    })
}

pub fn reg_write(reg: Reg, width: RegWidth, value: u32) -> Result<(), SyscallError> {
    if value > width.max_value() {
        return Err(SyscallError::InvalidArgument);
    }
    on_controller(reg, |controller, bound| {
        let offset = reg.offset as u64;
        let stream = bound.then_some(controller.stream_offset as u64);
        if write_permit(stream, offset, width).is_err() {
            return Err(refuse("write", reg, width));
        }
        if Some(offset) == stream.map(|at| at + SD_CTL) {
            return start_stop(controller, value as u8);
        }
        match width {
            RegWidth::U8 => controller.regs.write_u8(offset, value as u8),
            RegWidth::U16 => controller.regs.write_u16(offset, value as u16),
            RegWidth::U32 => controller.regs.write_u32(offset, value),
        }
        Ok(())
    })
}

/// `SDnCTL`'s first byte, where the driver starts and stops the engine.
fn start_stop(controller: &Listed, value: u8) -> Result<(), SyscallError> {
    // Refused: stream reset clears the descriptor's address and length registers, which the
    // driver cannot write back, so `RUN` would point the DMA engine at physical address zero.
    if value & SD_CTL_SRST != 0 {
        if REFUSALS.fetch_add(1, Ordering::Relaxed) < MAX_NAMED_REFUSALS {
            log!(
                "hda: refused SDnCTL {value:#04x} — stream reset clears the buffer descriptor \
                 address, which is the kernel's"
            );
        }
        return Err(SyscallError::PermissionDenied);
    }
    // Re-anchored on the start edge: SDnLPIB does not restart at zero when the stream restarts.
    let running = controller.stream.read_u8(SD_CTL) & SD_CTL_RUN != 0;
    if value & SD_CTL_RUN != 0 && !running {
        let position = controller.stream.read_u32(SD_LPIB);
        ISR.last.store(position as usize / PERIOD_BYTES % PERIODS, Ordering::Relaxed);
        ISR.mask.store(0, Ordering::Relaxed);
    }
    controller.stream.write_u8(SD_CTL, value);
    Ok(())
}

/// Bind the stream to the listed controller at `index`, and put every other one back into reset.
///
/// Takes no memory: the domain, the pools and the descriptor list are [`init`]'s, so nothing here
/// can be refused for want of it. The waits run with no lock held, on the caller's thread.
pub fn bind(index: u8) -> Result<(), SyscallError> {
    let (list, memory) = {
        let mut guard = CONTROLLER.lock();
        let hda = guard.as_mut().ok_or(SyscallError::NotFound)?;
        match &hda.state {
            State::Listed(list) if (index as usize) < list.len() => {}
            State::Listed(_) => return Err(SyscallError::InvalidArgument),
            State::Binding => return Err(SyscallError::WouldBlock),
            State::Bound(_) if index == 0 => return Ok(()),
            State::Bound(_) => return Err(SyscallError::InvalidArgument),
            State::Refused => return Err(SyscallError::Io),
        }
        let State::Listed(list) = core::mem::replace(&mut hda.state, State::Binding) else {
            unreachable!("matched as listed under the same guard")
        };
        (list, hda.memory.clone())
    };
    let outcome = bind_listed(list, index as usize, &memory);
    let bound = matches!(outcome, State::Bound(_));
    CONTROLLER.lock().as_mut().expect("a bind leaves the state it took").state = outcome;
    if bound { Ok(()) } else { Err(SyscallError::Io) }
}

/// The bind's body, over the list it took out of [`CONTROLLER`]; answers the state it leaves.
fn bind_listed(mut list: Vec<Listed>, index: usize, memory: &StreamMemory) -> State {
    let chosen = list.remove(index);
    let pci = chosen.pci;
    // Before `SD_BDPL` tells the device an address, and after both pools' mappings.
    memory.space.attach(pci.bus, pci.dev, pci.func);

    if !reset_stream(chosen.stream) {
        log!(
            "hda: {:02x}:{:02x}.{} stream {} never left reset — refused, and every listed \
             controller goes back into reset",
            pci.bus,
            pci.dev,
            pci.func,
            chosen.stream_index
        );
        silence(&chosen);
        for other in &list {
            silence(other);
        }
        return State::Refused;
    }

    let stream = chosen.stream;
    stream.write_u32(SD_BDPL, memory.bdl as u32);
    stream.write_u32(SD_BDPU, (memory.bdl >> 32) as u32);
    stream.write_u32(
        SD_CBL,
        stream::cyclic_length(PERIOD_BYTES as u32, PERIODS).expect("fits a u32"),
    );
    stream.write_u16(SD_LVI, stream::last_valid_index(PERIODS).expect("a ring of eight") as u16);
    stream.write_u8(SD_CTL_TAG, STREAM_TAG << 4);
    stream.write_u8(SD_CTL, SD_CTL_IOCE | SD_CTL_FEIE | SD_CTL_DEIE);
    stream.write_u8(SD_STS, SD_STS_WRITE_CLEAR);

    // SAFETY: the bind that left `Listed` is this field's one writer, and no state returns there;
    // no listed controller raises the vector before the bus mastering and `INTCTL` below.
    unsafe {
        *ISR.stream.get() = Some(stream);
    }
    pci.start_bus_mastering();
    chosen.regs.write_u32(INTCTL, INTCTL_GIE | (1 << chosen.stream_index));

    for other in &list {
        silence(other);
    }
    log!(
        "hda: {:02x}:{:02x}.{} bound, statests={:#06x}, output stream {} at {:#x}, tag {STREAM_TAG}, \
         {PERIODS} periods of {PERIOD_BYTES} bytes; {} other listed controller(s) back into reset",
        pci.bus,
        pci.dev,
        pci.func,
        chosen.statests,
        chosen.stream_index,
        chosen.stream_offset,
        list.len()
    );
    State::Bound(chosen)
}

/// Turn a listed controller's message off and put it back into reset. Nothing reaches it again
/// either way, so one that never reads back is named and left.
fn silence(controller: &Listed) {
    let pci = controller.pci;
    match controller.message {
        Message::MsiX => pci.disable_msix(),
        Message::Msi => pci.disable_msi(),
    }
    let regs = controller.regs;
    regs.write_u32(GCTL, 0);
    if !crate::clock::settles(SETTLE_NS, || regs.read_u32(GCTL) & GCTL_CRST == 0) {
        log!(
            "hda: {:02x}:{:02x}.{} never went back into reset (gctl={:#010x})",
            pci.bus,
            pci.dev,
            pci.func,
            regs.read_u32(GCTL)
        );
    }
}

/// List every controller this machine has a codec behind and take the stream's memory once;
/// bind none. Which one carries the audio is the driver's to choose, by walking their codecs.
pub fn init(devices: &[PciDevice]) {
    let controllers: Vec<&PciDevice> = devices
        .iter()
        .filter(|d| d.matches_class(CLASS_MULTIMEDIA, SUBCLASS_HDA, None))
        .collect();
    if controllers.is_empty() {
        return;
    }

    let mut live: Vec<(&PciDevice, Mmio, u16, u16)> = Vec::new();
    for pci in &controllers {
        if let Some((regs, gcap, statests)) = probe(pci) {
            live.push((pci, regs, gcap, statests));
        }
    }
    if live.is_empty() {
        log!(
            "hda: {} class 0403 function(s) and no codec on any link — no HDA audio on this \
             machine",
            controllers.len()
        );
        return;
    }
    if live.len() > MAX_CONTROLLERS {
        for (pci, _, _, statests) in &live {
            log!(
                "hda: {:02x}:{:02x}.{} has a live link (statests={statests:#06x})",
                pci.bus,
                pci.dev,
                pci.func
            );
        }
        log!(
            "hda: {} controllers answer on this machine and a claim lists at most {MAX_CONTROLLERS} \
             — refused by name, no HDA audio",
            live.len()
        );
        return;
    }

    let mut list: Vec<Listed> = Vec::new();
    for (pci, regs, gcap, statests) in live {
        let output_streams = (gcap >> 12) & 0xF;
        let input_streams = (gcap >> 8) & 0xF;
        if output_streams == 0 {
            log!(
                "hda: {:02x}:{:02x}.{} reports no output stream descriptor (gcap={gcap:#06x}) — \
                 refused",
                pci.bus,
                pci.dev,
                pci.func
            );
            continue;
        }
        let Some(message) = arm_interrupt(pci) else { continue };
        // Its registers are the kernel's to serve from boot, bound or not.
        crate::pcidev::note_kernel_driver(pci);
        let stream_index = input_streams as u8;
        let stream_offset = STREAM_BASE + stream_index as u64 * STREAM_STRIDE;
        log!(
            "hda: {:02x}:{:02x}.{} listed as controller {}, statests={statests:#06x}, output \
             stream {stream_index} at {stream_offset:#x}",
            pci.bus,
            pci.dev,
            pci.func,
            list.len()
        );
        list.push(Listed {
            pci: *pci,
            regs,
            stream: regs.subregion(stream_offset, STREAM_STRIDE),
            stream_index,
            stream_offset: stream_offset as u32,
            statests,
            message,
        });
    }
    if list.is_empty() {
        return;
    }

    let space = crate::iommu::DeviceSpace::create();
    let bdl = super::DmaPool::alloc_in(PERIODS * 16, space).leak();
    let pcm = super::DmaPool::alloc_in(PERIODS * PERIOD_BYTES, space).leak();
    // Unaligned: the descriptor layout is the HDA specification's (§3.6.2), not a Rust one.
    let bdl_view = bdl.unaligned();
    // Nothing races these writes: no controller is told this address until a bind's `SD_BDPL`.
    bdl_view.zero();
    pcm.zero();
    let entries = stream::build_bdl(pcm.device_addr(), PERIOD_BYTES as u32, PERIODS)
        .expect("hda: the pipeline's own shape builds a descriptor list");
    for (i, entry) in entries.iter().enumerate() {
        // `build_bdl` returns exactly `PERIODS` entries, matching the pool's `PERIODS * 16` bytes.
        bdl_view.write::<u64>(i * 16, entry.address);
        bdl_view.write::<u32>(i * 16 + 8, entry.length);
        bdl_view.write::<u32>(i * 16 + 12, u32::from(entry.interrupt_on_completion));
    }
    let pcm_region = Region {
        phys: crate::DirectMap::from_phys(pcm.host_phys()),
        size: crate::mm::PAGE_2M,
        cache: CachePolicy::Normal,
        pages: None,
    };

    log!(
        "hda: {} controller(s) listed, none bound; the stream's descriptor list and {PERIODS} \
         periods of {PERIOD_BYTES} bytes taken once",
        list.len()
    );
    *CONTROLLER.lock() = Some(Hda {
        state: State::Listed(list),
        memory: StreamMemory { space, bdl: bdl.device_addr(), pcm: pcm_region },
    });
}

/// Take one controller out of reset and ask whether anything is on its link; `None` for every way
/// a function can fail to be one, each named in the log.
fn probe(pci: &PciDevice) -> Option<(Mmio, u16, u16)> {
    let low = pci.read_config_u32(HEADER_BAR0);
    if low & 1 != 0 {
        log!("hda: {:02x}:{:02x}.{} bar0 is an I/O BAR — refused", pci.bus, pci.dev, pci.func);
        return None;
    }
    let wide = (low >> 1) & 0x3 == 2;
    let high = if wide { pci.read_config_u32(HEADER_BAR0 + 4) } else { 0 };
    let base = ((high as u64) << 32) | (low as u64 & !0xF);
    if base == 0 {
        log!(
            "hda: {:02x}:{:02x}.{} bar0 is unassigned — firmware gave it no register window",
            pci.bus,
            pci.dev,
            pci.func
        );
        return None;
    }

    power_up(pci);
    let command = pci.read_config_u16(HEADER_COMMAND);
    pci.write_config_u16(HEADER_COMMAND, command | COMMAND_MEMORY_SPACE);

    let regs = crate::mm::paging::map_mmio(
        base,
        MIN_BAR_BYTES * 4,
        MmioPolicy::Uncacheable,
    );

    let gcap = regs.read_u16(GCAP);
    if gcap == u16::MAX {
        log!(
            "hda: {:02x}:{:02x}.{} register window at {base:#x} answers all ones",
            pci.bus,
            pci.dev,
            pci.func
        );
        return None;
    }

    if !reset_controller(regs) {
        log!(
            "hda: {:02x}:{:02x}.{} never left reset (gctl={:#010x}) — refused",
            pci.bus,
            pci.dev,
            pci.func,
            regs.read_u32(GCTL)
        );
        return None;
    }

    let statests = regs.read_u16(STATESTS);
    log!(
        "hda: {:02x}:{:02x}.{} {:04x}:{:04x} version {}.{} gcap={gcap:#06x} statests={statests:#06x}",
        pci.bus,
        pci.dev,
        pci.func,
        pci.vendor_id(),
        pci.device_id(),
        regs.read_u8(VMAJ),
        regs.read_u8(VMIN),
    );
    if statests == 0 {
        log!(
            "hda: {:02x}:{:02x}.{} statests=0x0000 — nothing on the legacy link",
            pci.bus,
            pci.dev,
            pci.func
        );
        return None;
    }
    Some((regs, gcap, statests))
}

/// Put the function in D0 if firmware left it lower; D3hot reads all ones, indistinguishable from
/// an absent controller.
fn power_up(pci: &PciDevice) {
    let Ok(cap) = pci.capability(CAP_POWER_MANAGEMENT) else {
        return;
    };
    let pmcsr = cap.read_u16(PM_CONTROL_STATUS);
    if pmcsr & PM_STATE_MASK == 0 {
        return;
    }
    cap.write_u16(PM_CONTROL_STATUS, pmcsr & !PM_STATE_MASK);
    spin_ns(PM_D3HOT_RECOVERY.nanos());
}

/// Hold the controller in reset and release it, waiting for both edges: a write that never reads
/// back is a controller that is not there.
fn reset_controller(regs: Mmio) -> bool {
    regs.write_u32(GCTL, 0);
    if !crate::clock::settles(SETTLE_NS, || regs.read_u32(GCTL) & GCTL_CRST == 0) {
        return false;
    }
    regs.write_u32(GCTL, GCTL_CRST);
    if !crate::clock::settles(SETTLE_NS, || regs.read_u32(GCTL) & GCTL_CRST != 0) {
        return false;
    }
    spin_ns(CODEC_DETECT.nanos());
    true
}

/// The descriptor's own reset, clearing its address and length registers before the kernel
/// writes them.
fn reset_stream(stream: Mmio) -> bool {
    stream.write_u8(SD_CTL, SD_CTL_SRST);
    if !crate::clock::settles(SETTLE_NS, || stream.read_u8(SD_CTL) & SD_CTL_SRST != 0) {
        return false;
    }
    stream.write_u8(SD_CTL, 0);
    crate::clock::settles(SETTLE_NS, || stream.read_u8(SD_CTL) & SD_CTL_SRST == 0)
}

/// Arm the completion interrupt, or say why this controller cannot be listed; a refusal, never a
/// panic, over a peripheral.
fn arm_interrupt(pci: &PciDevice) -> Option<Message> {
    let vector = crate::arch::trap::HDA_VECTOR;
    if pci.enable_msix(vector).is_some() {
        return Some(Message::MsiX);
    }
    if pci.enable_msi(vector) {
        return Some(Message::Msi);
    }
    log!(
        "hda: {:02x}:{:02x}.{} has no interrupt this driver can arm — the line above says whether \
         a capability was missing or a message was refused — not listed",
        pci.bus,
        pci.dev,
        pci.func
    );
    None
}

fn spin_ns(duration: u64) {
    let deadline = crate::clock::nanos_since_boot() + duration;
    while crate::clock::nanos_since_boot() < deadline {
        core::hint::spin_loop();
    }
}
