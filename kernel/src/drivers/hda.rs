//! The Intel HDA stub: bring-up, one output stream, and the allow-list.
//!
//! The kernel touches only registers whose value is an address, indexes a
//! kernel structure, or acknowledges the interrupt; everything else a driver
//! reaches goes through [`reg_write`] and [`reg_read`], gated by a positive
//! list and refused by name. Codec and pin decisions belong to soundd, never
//! to this file.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use toyos_abi::audio::AudioCompletionRecord;
use toyos_abi::hda::HdaInfo;
use toyos_abi::syscall::{RegWidth, SyscallError};
use toyos_hda::stream;

use super::pci::PciDevice;
use crate::log;
use crate::mm::paging::{CachePolicy, MmioPolicy};
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

/// The pipeline shape; soundd's mix loop, its client ring depth and gate A's recorded counters are
/// sized against it.
const PERIODS: usize = 8;
const PERIOD_BYTES: usize = 512;

/// The tag this stream carries; [`HdaInfo::stream_tag`] is how soundd's verb names the same number.
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

// SAFETY: `stream` is written once at init before the vector is armed and is
// read-only afterwards; every other field is atomic.
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
    // SAFETY: sole writer is `init`, before the vector is armed.
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
    crate::irq_ring::isr_publish(crate::irq_ring::IrqSource::Audio, timestamp);
    crate::preempt::set_need_resched();
}

/// Are completions pending? Lock-free.
pub fn has_pending() -> bool {
    ISR.mask.load(Ordering::Acquire) != 0
}

static INBOX_WATCHERS: Lock<alloc::vec::Vec<crate::inbox::InboxId>> =
    Lock::new(alloc::vec::Vec::new());

pub fn add_inbox_watcher(id: crate::inbox::InboxId) {
    let mut watchers = INBOX_WATCHERS.lock();
    if !watchers.contains(&id) {
        watchers.push(id);
    }
}

pub fn remove_inbox_watcher(id: crate::inbox::InboxId) {
    INBOX_WATCHERS.lock().retain(|&x| x != id);
}

pub fn inbox_watchers() -> alloc::vec::Vec<crate::inbox::InboxId> {
    INBOX_WATCHERS.lock().clone()
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


struct HdaController {
    regs: Mmio,
    stream: Mmio,
    /// Kept so the pages outlive the mappings that name them, and so a refused controller gives
    /// them back.
    _bdl: super::DmaPool,
    _pcm: super::DmaPool,
}

static CONTROLLER: Lock<Option<HdaController>> = Lock::new(None);
static INFO: Lock<Option<(HdaInfo, Region)>> = Lock::new(None);
static REFUSALS: AtomicUsize = AtomicUsize::new(0);

pub fn info() -> Option<(HdaInfo, Region)> {
    INFO.lock().clone()
}


/// A register is on the list only if its value is not an address and indexes nothing the kernel
/// allocated; `SDnCBL` and `SDnLVI` are absent because the kernel writes them itself.
fn write_permit(stream_offset: u64, offset: u64, width: RegWidth) -> Result<&'static str, ()> {
    let sd = |field: u64| stream_offset + field;
    match (offset, width) {
        (IMMEDIATE_COMMAND, RegWidth::U32) => Ok("ICW: a codec verb, which names no memory"),
        (IMMEDIATE_STATUS, RegWidth::U16) => Ok("ICS: the immediate-command busy and valid bits"),
        (o, RegWidth::U8) if o == sd(SD_CTL) => Ok("SDnCTL: run and interrupt enables"),
        (o, RegWidth::U8) if o == sd(SD_CTL_TAG) => Ok("SDnCTL: the stream tag"),
        (o, RegWidth::U16) if o == sd(SD_FMT) => Ok("SDnFMT: the format word"),
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

fn refuse(what: &str, offset: u64, width: RegWidth) -> SyscallError {
    if REFUSALS.fetch_add(1, Ordering::Relaxed) < MAX_NAMED_REFUSALS {
        log!("hda: refused a {width:?} {what} of {offset:#x} — not on the allow-list");
    }
    SyscallError::PermissionDenied
}

pub fn reg_read(offset: u64, width: RegWidth) -> Result<u32, SyscallError> {
    if read_permit(offset, width).is_err() {
        return Err(refuse("read", offset, width));
    }
    let guard = CONTROLLER.lock();
    let controller = guard.as_ref().ok_or(SyscallError::NotFound)?;
    Ok(match width {
        RegWidth::U8 => controller.regs.read_u8(offset) as u32,
        RegWidth::U16 => controller.regs.read_u16(offset) as u32,
        RegWidth::U32 => controller.regs.read_u32(offset),
    })
}

pub fn reg_write(offset: u64, width: RegWidth, value: u32) -> Result<(), SyscallError> {
    let stream_offset = info().ok_or(SyscallError::NotFound)?.0.stream_offset as u64;
    if write_permit(stream_offset, offset, width).is_err() {
        return Err(refuse("write", offset, width));
    }
    if value > width.max_value() {
        return Err(SyscallError::InvalidArgument);
    }

    let guard = CONTROLLER.lock();
    let controller = guard.as_ref().ok_or(SyscallError::NotFound)?;

    if offset == stream_offset + SD_CTL {
        return start_stop(controller, value as u8);
    }
    match width {
        RegWidth::U8 => controller.regs.write_u8(offset, value as u8),
        RegWidth::U16 => controller.regs.write_u16(offset, value as u16),
        RegWidth::U32 => controller.regs.write_u32(offset, value),
    }
    Ok(())
}

/// `SDnCTL`'s first byte, where the driver starts and stops the engine.
fn start_stop(controller: &HdaController, value: u8) -> Result<(), SyscallError> {
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


/// Bring up the one HDA controller this machine has a codec behind; more than one live link is a
/// refusal naming every controller found, never a first match.
pub fn init(devices: &[PciDevice]) {
    let controllers: alloc::vec::Vec<&PciDevice> = devices
        .iter()
        .filter(|d| d.matches_class(CLASS_MULTIMEDIA, SUBCLASS_HDA, None))
        .collect();
    if controllers.is_empty() {
        return;
    }

    let mut live: alloc::vec::Vec<(&PciDevice, Mmio, u16, u16)> = alloc::vec::Vec::new();
    for pci in &controllers {
        if let Some((regs, gcap, statests)) = probe(pci) {
            live.push((pci, regs, gcap, statests));
        }
    }

    let (pci, regs, gcap, statests) = match live.len() {
        0 => {
            log!(
                "hda: {} class 0403 function(s) and no codec on any link — no HDA audio on this \
                 machine",
                controllers.len()
            );
            return;
        }
        1 => live.remove(0),
        n => {
            for (pci, _, _, statests) in &live {
                log!(
                    "hda: {:02x}:{:02x}.{} has a live link (statests={statests:#06x})",
                    pci.bus,
                    pci.dev,
                    pci.func
                );
            }
            log!(
                "hda: {n} controllers answer on this machine and choosing between them means \
                 walking their codec graphs, which is the driver's — refused by name, no HDA audio"
            );
            return;
        }
    };

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
        return;
    }

    let stream_index = input_streams as u8;
    let stream_offset = STREAM_BASE + stream_index as u64 * STREAM_STRIDE;
    let stream = regs.subregion(stream_offset, STREAM_STRIDE);

    let bdl = super::DmaPool::alloc(PERIODS * 16);
    let pcm = super::DmaPool::alloc(PERIODS * PERIOD_BYTES);
    // Unaligned: the descriptor layout is the HDA specification's (§3.6.2), not a Rust one.
    let bdl_view = bdl.view().unaligned();
    let pcm_view = pcm.view();
    // Nothing races these writes: the controller is not told this address until `SD_BDPL`, below.
    bdl_view.zero();
    pcm_view.zero();

    let entries = stream::build_bdl(pcm_view.device_addr(), PERIOD_BYTES as u32, PERIODS)
        .expect("hda: the pipeline's own shape builds a descriptor list");
    for (i, entry) in entries.iter().enumerate() {
        // `build_bdl` returns exactly `PERIODS` entries, matching the pool's `PERIODS * 16` bytes.
        bdl_view.write::<u64>(i * 16, entry.address);
        bdl_view.write::<u32>(i * 16 + 8, entry.length);
        bdl_view.write::<u32>(i * 16 + 12, u32::from(entry.interrupt_on_completion));
    }

    if !reset_stream(stream) {
        log!(
            "hda: {:02x}:{:02x}.{} stream {stream_index} never left reset — refused",
            pci.bus,
            pci.dev,
            pci.func
        );
        return;
    }

    stream.write_u32(SD_BDPL, bdl_view.device_addr() as u32);
    stream.write_u32(SD_BDPU, (bdl_view.device_addr() >> 32) as u32);
    stream.write_u32(
        SD_CBL,
        stream::cyclic_length(PERIOD_BYTES as u32, PERIODS).expect("fits a u32"),
    );
    stream.write_u16(SD_LVI, stream::last_valid_index(PERIODS).expect("a ring of eight") as u16);
    stream.write_u8(SD_CTL_TAG, STREAM_TAG << 4);
    stream.write_u8(SD_CTL, SD_CTL_IOCE | SD_CTL_FEIE | SD_CTL_DEIE);
    stream.write_u8(SD_STS, SD_STS_WRITE_CLEAR);

    // SAFETY: no vector is armed yet, so nothing can observe a half-written
    // Option.
    unsafe {
        *ISR.stream.get() = Some(stream);
    }

    pci.enable_bus_master();
    if !arm_interrupt(pci) {
        return;
    }
    regs.write_u32(INTCTL, INTCTL_GIE | (1 << stream_index));

    let pcm_region = Region {
        phys: crate::DirectMap::from_phys(pcm_view.host_phys()),
        size: crate::mm::PAGE_2M,
        cache: CachePolicy::DeferToMtrr,
        pages: None,
    };

    *CONTROLLER.lock() = Some(HdaController { regs, stream, _bdl: bdl, _pcm: pcm });
    *INFO.lock() = Some((HdaInfo {
        pcm: toyos_abi::HANDLE_INVALID,
        period_bytes: PERIOD_BYTES as u32,
        stream_offset: stream_offset as u32,
        statests,
        stream_tag: STREAM_TAG,
        periods: PERIODS as u8,
    }, pcm_region));

    log!(
        "hda: {:02x}:{:02x}.{} bound, statests={statests:#06x}, output stream {stream_index} at \
         {stream_offset:#x}, tag {STREAM_TAG}, {PERIODS} periods of {PERIOD_BYTES} bytes",
        pci.bus,
        pci.dev,
        pci.func
    );

    #[cfg(feature = "boot-actuators")]
    if crate::actuator::hda_allowlist_selftest() {
        allowlist_selftest(stream_offset);
    }
}

/// Every arm of [`write_permit`] and [`read_permit`], run against the bound controller and
/// reported by name.
#[cfg(feature = "boot-actuators")]
fn allowlist_selftest(stream_offset: u64) {
    let sd = |field: u64| stream_offset + field;
    let cases: &[(&str, u64, RegWidth, u32)] = &[
        ("ICW", IMMEDIATE_COMMAND, RegWidth::U32, 0),
        ("SDnFMT", sd(SD_FMT), RegWidth::U16, 0),
        ("SDnCTL", sd(SD_CTL), RegWidth::U8, SD_CTL_IOCE as u32),
        ("SDnCTL-tag", sd(SD_CTL_TAG), RegWidth::U8, (STREAM_TAG as u32) << 4),
        ("SDnBDPL", sd(SD_BDPL), RegWidth::U32, 0),
        ("SDnBDPU", sd(SD_BDPU), RegWidth::U32, 0),
        ("SDnCBL", sd(SD_CBL), RegWidth::U32, 0),
        ("SDnLVI", sd(SD_LVI), RegWidth::U16, 0xFF),
        ("SDnSTS", sd(SD_STS), RegWidth::U8, 0),
        ("SDnCTL-srst", sd(SD_CTL), RegWidth::U8, SD_CTL_SRST as u32),
        // A 32-bit SDnCTL write also reaches SDnSTS, the kernel's own interrupt acknowledgement.
        ("SDnCTL-wide", sd(SD_CTL), RegWidth::U32, 0),
        ("INTCTL", INTCTL, RegWidth::U32, 0),
        ("GCTL", GCTL, RegWidth::U32, 0),
    ];
    for &(name, offset, width, value) in cases {
        let verdict = match reg_write(offset, width, value) {
            Ok(()) => "written",
            Err(_) => "refused",
        };
        log!("hda: selftest write {name} {verdict}");
    }
    for (name, offset, width) in [
        ("ICS", IMMEDIATE_STATUS, RegWidth::U16),
        ("IRR", IMMEDIATE_RESPONSE, RegWidth::U32),
        ("SDnLPIB", sd(SD_LPIB), RegWidth::U32),
        ("STATESTS", STATESTS, RegWidth::U16),
    ] {
        let verdict = match reg_read(offset, width) {
            Ok(_) => "read",
            Err(_) => "refused",
        };
        log!("hda: selftest read {name} {verdict}");
    }
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
    let Some(cap) = pci.capabilities().find(|c| c.id() == CAP_POWER_MANAGEMENT) else {
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

/// Arm the completion interrupt, or say why this machine has no HDA audio; a refusal, never a
/// panic, over a peripheral.
fn arm_interrupt(pci: &PciDevice) -> bool {
    let vector = crate::arch::idt::HDA_VECTOR;
    if pci.enable_msix(vector) || pci.enable_msi(vector) {
        return true;
    }
    log!(
        "hda: {:02x}:{:02x}.{} has no interrupt this driver can arm — the line above says whether \
         a capability was missing or a message was refused — NOT INITIALISED",
        pci.bus,
        pci.dev,
        pci.func
    );
    false
}

fn spin_ns(duration: u64) {
    let deadline = crate::clock::nanos_since_boot() + duration;
    while crate::clock::nanos_since_boot() < deadline {
        core::hint::spin_loop();
    }
}
