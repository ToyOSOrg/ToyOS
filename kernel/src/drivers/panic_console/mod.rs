//! On-screen panic console.
//!
//! Renders log records as an 8x16 text grid onto the UEFI GOP framebuffer,
//! through `LogRecord`'s `Display`, so no second formatter can drift from
//! `logd`. [`capture`] freezes the report before `panic_flush` drains it;
//! [`render`] paints it inside `halt_all_cpus`, before `panic_flush`. A
//! recovered panic must call [`discard_capture`]. virtio-gpu is
//! unsupported: its scanout needs the unbounded-poll wedge this module avoids.

mod latch;

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use toyos_abi::boot::{KernelArgs, MemoryMapEntry};
use toyos_ps2::{KeyDecoder, KeyOutcome};

use crate::log;
use crate::time::{Budget, Cadence, Duration};
use crate::mm::paging::MmioPolicy;
use crate::mm::{self, DirectMap, align_2m};

/// 1 bpp 8x16, codepoints 0x20..=0x7E, one byte per row, bit 7 leftmost.
/// `tests/common/screen.rs` decodes this same file, so decoder and renderer cannot drift.
static FONT: &[u8; 95 * 16] = include_bytes!("font8x16.bin");

const GLYPH_W: usize = 8;
const GLYPH_H: usize = 16;

/// Grid caps, chosen to bound the wrap pass's one stack array.
const MAX_COLS: usize = 320;
const MAX_ROWS: usize = 96;

/// Bytes of rendered log retained; must be at least one screenful (asserted below).
const SNAPSHOT_CAP: usize = 32 * 1024;
const _: () = assert!(SNAPSHOT_CAP >= MAX_ROWS * MAX_COLS);

/// One bit per byte `text` can hold — worst case is a message of nothing but newlines, one line per byte.
const ALERT_WORDS: usize = SNAPSHOT_CAP.div_ceil(64);

/// How long each page stays up; a [`Cadence`], not a deadline — nothing expires.
const PAGE_HOLD: Cadence = Cadence::every(
    Duration::from_secs(3),
    "a five-page report cycles inside a 15-second video",
);

/// How long Ctrl+Alt+D's report keeps the panel.
const REPORT_HOLD: Budget = Budget::of(
    Duration::from_secs(15),
    "the report stops being put back and the desktop keeps its screen",
);

/// How often the panel is checked for an overwrite while the report holds it.
const REPORT_CHECK: Cadence = Cadence::every(
    Duration::from_millis(20),
    "PROBES uncached reads per check, on the CPU that took an interrupt anyway",
);

/// Framebuffers below this are reachable before [`remap`] runs; above it, only after.
const LOW_MAP_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Copy)]
struct Fb {
    ptr: *mut u8,
    bytes: u64,
    stride_px: u32,
    width: u32,
    height: u32,
    format: u32,
}

impl Fb {
    const DETACHED: Fb = Fb {
        ptr: core::ptr::null_mut(),
        bytes: 0,
        stride_px: 0,
        width: 0,
        height: 0,
        format: 0,
    };
}

struct FbCell(UnsafeCell<Fb>);
// SAFETY: the panic path may take no lock; `FB` is published and read only
// under the `SEQ` seqlock, and `PENDING` has one writer at a time.
unsafe impl Sync for FbCell {}

/// A screenful-and-then-some of rendered log; which lines came from an `alert!` is a [`Level`] flag, never inferred from the text.
struct Rendered {
    text: [u8; SNAPSHOT_CAP],
    /// One bit per line, counted back from the last — the buffer fills from its end.
    alert: [u64; ALERT_WORDS],
    /// Bytes of `text` in use, at its end.
    len: usize,
    lines: usize,
}

impl Rendered {
    const EMPTY: Self =
        Self { text: [0; SNAPSHOT_CAP], alert: [0; ALERT_WORDS], len: 0, lines: 0 };

    /// Render the newest records stamped in `from..=to` that fit; returns the byte count. Records older than the buffer holds are dropped.
    fn render(&mut self, from: u64, to: u64) -> usize {
        self.alert = [0; ALERT_WORDS];
        self.lines = 0;
        self.len = 0;
        let mut fill = Backfill { at: SNAPSHOT_CAP, into: self };
        log::read::snapshot_committed(from, to, &mut fill);
        let at = fill.at;
        self.len = SNAPSHOT_CAP - at;
        self.len
    }

    fn view(&self) -> View<'_> {
        View {
            text: self.text.get(SNAPSHOT_CAP - self.len..).unwrap_or(&[]),
            alert: &self.alert,
            lines: self.lines,
        }
    }
}

/// A rendered log as a painter sees it.
#[derive(Clone, Copy)]
struct View<'a> {
    text: &'a [u8],
    alert: &'a [u64; ALERT_WORDS],
    lines: usize,
}

impl View<'_> {
    /// Whether line `n`, counted from the first, came from an `alert!`.
    fn is_alert(&self, n: usize) -> bool {
        let Some(from_end) = self.lines.checked_sub(n + 1) else { return false };
        self.alert.get(from_end / 64).is_some_and(|word| word & (1 << (from_end % 64)) != 0)
    }
}

/// Writes records into a [`Rendered`] back to front, so the newest survive a full buffer.
struct Backfill<'a> {
    into: &'a mut Rendered,
    /// Where the next — older — line starts.
    at: usize,
}

impl log::read::RecordSink for Backfill<'_> {
    fn put(&mut self, record: &toyos_abi::log::LogRecord) -> bool {
        let line = rendered_len(record).saturating_add(1);
        let Some(at) = self.at.checked_sub(line) else { return false };
        let Some(out) = self.into.text.get_mut(at..self.at) else { return false };
        // The same formatter measured the line, so this fills the slot exactly; a mismatch would leave zeroes, never overrun.
        let mut into = Into { out, at: 0 };
        let _ = core::fmt::write(&mut into, format_args!("{record}\n"));

        // Counted in newlines, not records: `paint` counts newlines, and a multi-line record (every panic) is more than one row.
        let lines = out.iter().filter(|&&byte| byte == b'\n').count();
        if record.level() == Some(log::Level::Alert) {
            for line in self.into.lines..self.into.lines + lines {
                if let Some(word) = self.into.alert.get_mut(line / 64) {
                    *word |= 1 << (line % 64);
                }
            }
        }
        self.into.lines += lines;
        self.at = at;
        true
    }
}

/// What one record's line costs, without rendering it anywhere.
fn rendered_len(record: &toyos_abi::log::LogRecord) -> usize {
    struct Count(usize);
    impl core::fmt::Write for Count {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            self.0 = self.0.saturating_add(s.len());
            Ok(())
        }
    }
    let mut count = Count(0);
    let _ = core::fmt::write(&mut count, format_args!("{record}"));
    count.0
}

/// A `fmt::Write` over a fixed slot that drops overflow instead of panicking — the one caller is a crash report.
struct Into<'a> {
    out: &'a mut [u8],
    at: usize,
}

impl core::fmt::Write for Into<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.at.saturating_add(s.len());
        if let Some(slot) = self.out.get_mut(self.at..end) {
            slot.copy_from_slice(s.as_bytes());
            self.at = end;
        }
        Ok(())
    }
}

struct RenderedCell(UnsafeCell<Rendered>);
// SAFETY: the panic path may take no lock; `Rendered` is too large to copy
// or swap atomically. `PAINTING`, and `CAPTURE_OWNER` over `SNAPSHOT`, serialise the three cells.
unsafe impl Sync for RenderedCell {}

/// Seqlock over `FB`; even means stable, odd means a publisher is inside.
/// Not a `Lock`: its guard drop can dispatch the scheduler, forbidden here.
/// A publisher that dies mid-update leaves this odd forever, costing the screen and nothing else.
static SEQ: AtomicU32 = AtomicU32::new(0);
static FB: FbCell = FbCell(UnsafeCell::new(Fb::DETACHED));

/// Exactly one painter at a time, taken by every painter without exception —
/// this is also what stops a fault inside the renderer from recursing past depth one.
/// [`render`] never releases it, so a later boot checkpoint cannot paint
/// over a fatal report; [`boot_checkpoint`] does release it.
static PAINTING: AtomicBool = AtomicBool::new(false);

static SNAPSHOT: RenderedCell = RenderedCell(UnsafeCell::new(Rendered::EMPTY));

/// `SNAPSHOT`'s one writer, ever; released only by [`discard_capture`], on recovery.
static CAPTURE: latch::CaptureLatch = latch::CaptureLatch::new();

/// The early branch's token: percpu is not up there, and exactly one CPU exists.
const EARLY_CAPTOR: u32 = 1;

/// True means an unrecovered panic captured this report; [`discard_capture`]
/// clears it on recovery, so a survived panic is not later painted as the cause of death.
static CAPTURED: AtomicBool = AtomicBool::new(false);

/// Scratch for readers of the live shards (a boot checkpoint, or a fatal
/// path with no panic handler run) — separate from `SNAPSHOT` so a checkpoint cannot erase a captured, unpainted report.
static LIVE: RenderedCell = RenderedCell(UnsafeCell::new(Rendered::EMPTY));

/// Ctrl+Alt+D's report, rendered once and held for [`REPORT_HOLD`] by [`hold_report`].
/// A separate buffer, not a live re-read, because it must survive a repaint after the ring has moved on.
static REPORT: RenderedCell = RenderedCell(UnsafeCell::new(Rendered::EMPTY));

/// When the report gives the panel back. 0 means it does not hold it.
static HOLD_UNTIL: AtomicU64 = AtomicU64::new(0);
/// Next `nanos_since_boot` a CPU may check the panel; a CAS picks one CPU of the eight.
static HOLD_CHECK_AT: AtomicU64 = AtomicU64::new(0);

/// Set once a process claims `DeviceType::Framebuffer`; a fatal panic ignores this and takes the screen back unconditionally.
static SCREEN_OWNED_BY_USERLAND: AtomicBool = AtomicBool::new(false);

/// When userland claimed the screen, so [`probe_due`] can wait past it. 0 means it has not.
#[cfg(feature = "boot-actuators")]
static CLAIMED_AT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Delay after the claim before the probe fires, long enough for a desktop to finish coming up.
#[cfg(feature = "boot-actuators")]
const PROBE_DELAY_NS: u64 = 5_000_000_000;

/// Whether the `metal-panic-probe` boot should panic now. Called from the
/// idle loop, whose fall-through skips recovery — the only path that never paints.
#[cfg(feature = "boot-actuators")]
pub fn probe_due() -> bool {
    if !crate::actuator::metal_panic_probe() {
        return false;
    }
    let at = CLAIMED_AT.load(Ordering::Relaxed);
    if at == 0 || crate::clock::nanos_since_boot().saturating_sub(at) < PROBE_DELAY_NS {
        return false;
    }
    // Exactly one CPU: every idle CPU reaches this in the same microsecond, and only one may fire the probe.
    static FIRED: AtomicBool = AtomicBool::new(false);
    FIRED.compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed).is_ok()
}

/// Hand the screen over, and do not return while a checkpoint is still
/// drawing on it — a claimer that returns mid-paint composes over glyphs a damage-tracked client never knows are there.
pub fn screen_claimed_by_userland() {
    SCREEN_OWNED_BY_USERLAND.store(true, Ordering::SeqCst);
    #[cfg(feature = "boot-actuators")]
    CLAIMED_AT.store(crate::clock::nanos_since_boot().max(1), Ordering::Relaxed);
    // Bounded so a CPU that died mid-paint cannot take the display down with it.
    const CHECKPOINT: Budget = Budget::of(
        Duration::from_secs(2),
        "the claimer proceeds and says so, rather than the display going down with a dead CPU",
    );
    let deadline = crate::clock::nanos_since_boot() + CHECKPOINT.nanos();
    while PAINTING.load(Ordering::SeqCst) {
        if crate::clock::nanos_since_boot() > deadline {
            log!("panic console: claim waited 2s for a checkpoint that never finished");
            return;
        }
        core::hint::spin_loop();
    }
}

/// The descriptor [`arm`] validated, so [`remap`]/[`rearm`] can re-publish it without re-deriving it. Null means no usable framebuffer exists.
static PENDING: FbCell = FbCell(UnsafeCell::new(Fb::DETACHED));
static RAW_PHYS: AtomicU64 = AtomicU64::new(0);
static RAW_SIZE: AtomicU64 = AtomicU64::new(0);

/// Boot-time and `set_resolution`-window only: publishers never race, so the load-then-store of `SEQ` needs no CAS.
fn publish(fb: Fb) {
    let seq = SEQ.load(Ordering::Relaxed);
    SEQ.store(seq.wrapping_add(1), Ordering::Relaxed);
    // The release fence orders the descriptor store between the odd and even markers; a release RMW alone would not.
    core::sync::atomic::fence(Ordering::Release);
    // SAFETY: the seqlock's payload store; `SEQ` is odd across this write, so
    // a concurrent `snapshot` discards what it reads. Publishers never race each other.
    unsafe { *FB.0.get() = fb };
    SEQ.store(seq.wrapping_add(2), Ordering::Release);
}

/// Stop painting until the next [`rearm`], for a window where the framebuffer may be freed and reallocated.
/// Worst case becomes no console, never a write into memory that has been freed and reused.
pub fn detach() {
    publish(Fb::DETACHED);
}

/// Re-publish the armed descriptor after such a window.
pub fn rearm() {
    // SAFETY: `PENDING` has one writer at a time (`arm`/`disable`,
    // single-threaded, or `set_resolution`'s single-CPU window); `Fb` is `Copy`, so this holds no reference into the cell.
    let fb = unsafe { *PENDING.0.get() };
    if validate(&fb) {
        publish(fb);
    }
}

/// Give up the display permanently when virtio-gpu wins device selection; clearing `PENDING` stops a later `rearm` resurrecting it.
pub fn disable() {
    // SAFETY: sound as `rearm`'s read — `PENDING` has one writer, called from the single-threaded boot sequence.
    unsafe { *PENDING.0.get() = Fb::DETACHED };
    RAW_PHYS.store(0, Ordering::Relaxed);
    detach();
}

/// Torn means unavailable, never a wild pointer: only two descriptors are
/// ever published, and every torn mixture is caught downstream by the null check or a zero field collapsing draws to no-ops.
/// A second valid descriptor would break that argument, leaving only the fences.
fn snapshot() -> Option<Fb> {
    for _ in 0..4 {
        let before = SEQ.load(Ordering::Acquire);
        if before & 1 != 0 {
            continue;
        }
        // SAFETY: the seqlock's payload read, the one place `FB` is read
        // while a publisher may be inside it — sound on the `SEQ` comparison below, not exclusion.
        let fb = unsafe { *FB.0.get() };
        core::sync::atomic::fence(Ordering::Acquire);
        if SEQ.load(Ordering::Relaxed) == before {
            return (!fb.ptr.is_null()).then_some(fb);
        }
    }
    None
}

/// Reject a descriptor that could turn a panic into a wild write.
fn validate(fb: &Fb) -> bool {
    if fb.ptr.is_null() || !mm::is_kernel_addr(fb.ptr as u64) {
        return false;
    }
    if !(1..=16384).contains(&fb.width) || !(1..=16384).contains(&fb.height) {
        return false;
    }
    if fb.stride_px < fb.width || fb.stride_px > 65536 {
        return false;
    }
    let needed = (fb.stride_px as u64)
        .checked_mul(fb.height as u64)
        .and_then(|v| v.checked_mul(4));
    matches!(needed, Some(n) if fb.bytes >= n)
}

/// Whether the UEFI memory map hands any of `[phys, phys + size)` to the PMM
/// as free RAM — `pmm::is_usable` counts `BootServicesData`/`LoaderData`.
/// Checked one entry at a time: UEFI describes memory one descriptor per
/// contiguous same-type run (UEFI 2.10 §7.2).
const fn framebuffer_is_reclaimed_ram(
    maps: &[MemoryMapEntry],
    phys: u64,
    size: u64,
) -> Option<u32> {
    let end = phys.saturating_add(if size == 0 { 1 } else { size });
    let mut i = 0;
    while i < maps.len() {
        let entry = &maps[i];
        if entry.start < end && phys < entry.end && mm::pmm::is_usable_type(entry.uefi_type) {
            return Some(entry.uefi_type);
        }
        i += 1;
    }
    None
}

/// A scanout whose first byte is `MemoryMappedIO` (11) and whose tail runs into
/// `BootServicesData` (4) is PMM-owned RAM; one that stops short of it is not.
const _: () = {
    const SPLIT: [MemoryMapEntry; 2] = [
        MemoryMapEntry { uefi_type: 11, start: 0xE000_0000, end: 0xE080_0000 },
        MemoryMapEntry { uefi_type: 4, start: 0xE080_0000, end: 0xE100_0000 },
    ];
    assert!(framebuffer_is_reclaimed_ram(&SPLIT, 0xE000_0000, 0x0100_0000).is_some());
    assert!(framebuffer_is_reclaimed_ram(&SPLIT, 0xE000_0000, 0x0080_0000).is_none());
};

/// Arm the console from `KernelArgs`, before `serial::init`; covers
/// everything up to `mm::init`, which the bootloader's identity+high map already reaches.
pub fn arm(args: &KernelArgs, maps: &[MemoryMapEntry]) {
    if args.gop_framebuffer == 0 {
        return;
    }

    if let Some(uefi_type) =
        framebuffer_is_reclaimed_ram(maps, args.gop_framebuffer, args.gop_framebuffer_size)
    {
        log!(
            "panic console: disarmed, framebuffer at {:#x} is UEFI type {} (PMM-owned RAM)",
            args.gop_framebuffer,
            uefi_type
        );
        return;
    }

    let fb = Fb {
        ptr: DirectMap::from_phys(args.gop_framebuffer).as_mut_ptr::<u8>(),
        bytes: args.gop_framebuffer_size,
        stride_px: args.gop_stride,
        width: args.gop_width,
        height: args.gop_height,
        format: args.gop_pixel_format,
    };
    if !validate(&fb) {
        log!(
            "panic console: disarmed, bad descriptor {}x{} stride={} size={:#x}",
            args.gop_width, args.gop_height, args.gop_stride, args.gop_framebuffer_size
        );
        return;
    }

    // SAFETY: sound as `disable`'s write — `arm` runs once, single-threaded, before any AP starts.
    unsafe { *PENDING.0.get() = fb };
    RAW_PHYS.store(args.gop_framebuffer, Ordering::Relaxed);
    RAW_SIZE.store(args.gop_framebuffer_size, Ordering::Relaxed);

    match args.gop_framebuffer.checked_add(args.gop_framebuffer_size) {
        Some(end) if end <= LOW_MAP_LIMIT => {
            publish(fb);
            log!(
                "panic console: armed {}x{} stride={} format={} at {:#x}",
                fb.width, fb.height, fb.stride_px, fb.format, args.gop_framebuffer
            );
        }
        _ => log!(
            "panic console: framebuffer at {:#x} is above the boot map, armed after mm::init",
            args.gop_framebuffer
        ),
    }
}

/// Re-establish the mapping after `mm::init` replaces the bootloader's page
/// tables, and arm a framebuffer above [`LOW_MAP_LIMIT`]'s reach.
pub fn remap() {
    let phys = RAW_PHYS.load(Ordering::Relaxed);
    if phys == 0 {
        return;
    }
    let size = RAW_SIZE.load(Ordering::Relaxed);
    mm::paging::map_mmio(phys, align_2m(size as usize) as u64, MmioPolicy::WriteCombining);
    rearm();
}

/// Render the newest records into the console's static scratch, before
/// `panic_flush` drains them; no pixels, no lock. Skipped when no
/// framebuffer is armed. Freezes the report at the instant of the panic —
/// [`live_tail`] re-reads a ring siblings may still be writing to. Keep
/// this even though no test distinguishes its absence: a sibling still
/// logging between panic and paint could otherwise push the report off `live_tail`'s window.
pub fn capture() {
    if snapshot().is_none() {
        return;
    }
    // A loser's report went to serial on its own flush; the winner's is the one painted.
    if !CAPTURE.claim(captor_token()) {
        return;
    }
    // SAFETY: `Rendered` must be a `static` (too large to return), and the
    // panic path may take no lock. `SNAPSHOT` is written only here, under
    // `CAPTURE`, held past this line; readers run after, gated by `PAINTING`.
    let into = unsafe { &mut *SNAPSHOT.0.get() };
    CAPTURED.store(into.render(0, u64::MAX) > 0, Ordering::Relaxed);
}

/// This CPU's claim on `SNAPSHOT`; tokens never collide, unclaimed-0 and `EARLY_CAPTOR` included.
fn captor_token() -> u32 {
    if log::PERCPU_READY.load(Ordering::Relaxed) {
        crate::arch::percpu::cpu_id().wrapping_add(2)
    } else {
        EARLY_CAPTOR
    }
}

/// Drop the captured report: this panic was survived. Called only on the recovery branch.
pub fn discard_capture() {
    // `CAPTURED` first: a next captor's fresh report must not be clobbered by this clear.
    CAPTURED.store(false, Ordering::Relaxed);
    CAPTURE.release();
}

/// Re-freeze the captured report so a line written *after* [`capture`] is
/// painted; only refreshes a capture that already exists — [`live_tail`] already reads live otherwise.
/// One caller: `apic::wait_for_log_file` logs this after `capture` already ran, on the machine with no serial fallback.
pub fn refresh_capture() {
    if !CAPTURED.load(Ordering::Relaxed) {
        return;
    }
    capture();
}

/// The tail of the live shards, for callers with nothing captured; consumes nothing, so `panic_flush` reports it again identically.
fn live_tail() -> View<'static> {
    // SAFETY: sound as `capture`'s `SNAPSHOT` write — `LIVE` is reached only
    // from here, every caller holds `PAINTING`; one block, not two, so render and view can't disagree.
    let into: &'static mut Rendered = unsafe { &mut *LIVE.0.get() };
    into.render(0, u64::MAX);
    let rendered: &'static Rendered = into;
    rendered.view()
}

// INVARIANT: render and everything it calls takes no lock, allocates
// nothing, uses no `&dyn`/unwrap/expect/[], and only checked/saturating
// arithmetic. Every framebuffer write is clamped to the published byte
// count. Stack budget is 256 bytes plus the one wrap array; the double
// fault path runs on IST1, 16384 bytes, already partly consumed.

/// What a fatal path paints: the captured report, or the live ring for a
/// path that reached `halt_all_cpus` without the panic handler. Idempotent
/// for the captured case, which [`page_forever`] walks repeatedly.
fn fatal_text() -> View<'static> {
    if CAPTURED.load(Ordering::Relaxed) {
        // SAFETY: sound as `capture`'s write — `CAPTURED` true means `SNAPSHOT` is written and never written again, so this branch is idempotent.
        unsafe { &*SNAPSHOT.0.get() }.view()
    } else {
        live_tail()
    }
}

/// Paint the newest page of the captured report, fatal paths only; returns whether this call took the screen, entitling [`page_forever`].
pub fn render() -> bool {
    if PAINTING.swap(true, Ordering::SeqCst) {
        return false;
    }
    paint(Fill::Fatal, fatal_text(), Page::Last, Watch::No);
    true
}

/// Cycle the report across the screen until the machine is switched off.
/// Reached only from `halt_all_cpus`, after `panic_flush`, on the CPU whose
/// [`render`] took `PAINTING`; the handler's other two exits call
/// [`render`] instead, since neither can safely loop in place.
pub fn page_forever() {
    if !crate::clock::calibrated() {
        return;
    }
    let text = fatal_text();
    let Some(fb) = snapshot() else { return };
    let Some((cols, grid_rows)) = geometry(&fb) else { return };
    let (_, pages, _) = pagination(text.text, cols, grid_rows);
    if pages < 2 {
        return;
    }
    // `None` is the screenful [`render`] already painted, not a numbered page, so the first key reaches either end.
    let mut shown: Option<usize> = None;
    let mut keys = KeyDecoder::new();
    // Once steered, the cycle is his: there is no way back to automatic paging.
    let mut steered = false;
    // Spins rather than `hlt`: nothing would wake it, and re-arming the LAPIC timer would dispatch the scheduler mid-panic.
    loop {
        let step = hold((!steered).then_some(PAGE_HOLD.nanos()), &mut keys);
        steered |= step.is_some();
        let next = match (shown, step.unwrap_or(PageKey::Down)) {
            (None, PageKey::Down) => 0,
            (None, PageKey::Up) => pages - 1,
            (Some(page), PageKey::Down) => (page + 1) % pages,
            (Some(page), PageKey::Up) => (page + pages - 1) % pages,
        };
        paint(Fill::Fatal, text, Page::Nth(next), Watch::No);
        shown = Some(next);
    }
}

/// Which way the next paint moves.
#[derive(Clone, Copy)]
enum PageKey {
    Up,
    Down,
}

/// HID usages: what the wire decoder emits, not scancodes.
const HID_PAGE_UP: u8 = 0x4B;
const HID_PAGE_DOWN: u8 = 0x4E;

/// Wait for a page key, giving up after `nanos`; `None` means the deadline
/// expired. [`i8042::poll_byte`] is an `inb` — no lock, no MMIO.
///
/// [`i8042::poll_byte`]: crate::drivers::i8042::poll_byte
fn hold(nanos: Option<u64>, keys: &mut KeyDecoder) -> Option<PageKey> {
    let target = nanos.map(|n| crate::clock::nanos_since_boot().saturating_add(n));
    while target.is_none_or(|t| crate::clock::nanos_since_boot() < t) {
        // The pointer shares the port; its packet bytes look like scancodes to anything that does not skip them.
        if let Some((byte, false)) = crate::drivers::i8042::poll_byte() {
            match keys.feed(byte) {
                KeyOutcome::Key { usage: HID_PAGE_UP, pressed: true } => return Some(PageKey::Up),
                KeyOutcome::Key { usage: HID_PAGE_DOWN, pressed: true } => {
                    return Some(PageKey::Down);
                }
                _ => {}
            }
        }
        core::hint::spin_loop();
    }
    None
}

/// Repaint at a boot phase boundary, so a machine that wedges later still shows how far it got.
pub fn boot_checkpoint() {
    if SCREEN_OWNED_BY_USERLAND.load(Ordering::Relaxed) {
        return;
    }
    // The same latch, taken the same way: an AP that misses `boot_aps`'s
    // deadline can panic mid-checkpoint under a plain load. Losing the race
    // costs a checkpoint or one fatal repaint; serial reports either way.
    if PAINTING.swap(true, Ordering::SeqCst) {
        return;
    }
    paint(Fill::Boot, live_tail(), Page::Last, Watch::No);
    PAINTING.store(false, Ordering::SeqCst);
}

/// Put the log between two marks on the panel, and keep it there: a
/// bracket, not a tail, so the answer cannot land on the wrong page.
/// `from`/`to` are timestamps, not byte positions — a byte range has no
/// meaning across shards and could be widened by a concurrent writer.
///
/// [`boot_checkpoint`] without the userland check — the keystroke is the
/// consent. A single paint is not a report, so this arms a hold that [`hold_report`] answers.
pub fn paint_report(from: u64, to: u64) {
    // SAFETY: sound as `capture`'s `SNAPSHOT` write — `REPORT` is reached
    // only from here and `report_text`; the dump reschedules every CPU, so only one is in flight.
    let into = unsafe { &mut *REPORT.0.get() };
    into.render(from, to);
    HOLD_UNTIL.store(
        crate::clock::nanos_since_boot().saturating_add(REPORT_HOLD.nanos()),
        Ordering::Relaxed,
    );
    paint_held_report();
}

/// The report as it was when the dump finished, held for [`REPORT_HOLD`]; empty until one has been asked for.
fn report_text() -> View<'static> {
    // SAFETY: sound as `paint_report`'s write — both callers run after it returns, and `paint_held_report` holds `PAINTING`.
    unsafe { &*REPORT.0.get() }.view()
}

fn paint_held_report() {
    if PAINTING.swap(true, Ordering::SeqCst) {
        return;
    }
    paint(Fill::Boot, report_text(), Page::Last, Watch::Yes);
    PAINTING.store(false, Ordering::SeqCst);
}

/// Put the report back if the panel has stopped carrying it. Called from
/// `drain_irqs` on every CPU, every pass. Repaints on evidence, not a
/// timer: a genuinely stopped machine costs [`PROBES`] reads per
/// [`REPORT_CHECK`] and never a paint — a timer would blank and redraw every
/// tick, risking a black frame if a camera catches the gap.
pub fn hold_report() {
    let until = HOLD_UNTIL.load(Ordering::Relaxed);
    if until == 0 {
        return;
    }
    let now = crate::clock::nanos_since_boot();
    if now >= until {
        HOLD_UNTIL.store(0, Ordering::Relaxed);
        return;
    }
    let due = HOLD_CHECK_AT.load(Ordering::Relaxed);
    if now < due
        || HOLD_CHECK_AT
            .compare_exchange(
                due,
                now.saturating_add(REPORT_CHECK.nanos()),
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_err()
    {
        return;
    }
    let Some(fb) = snapshot() else { return };
    if !mapped(&fb) || panel_carries_report(&fb) {
        return;
    }
    log!("panic console: the panel was drawn over, putting the report back");
    paint_held_report();
}

/// Which slice of the text a paint shows.
#[derive(Clone, Copy)]
enum Page {
    /// The newest screenful: a page-aligned last page would leave the bottom blank when rows divide badly.
    Last,
    Nth(usize),
}

/// Carries "halted" vs "still booting" at zero cost, and proves the console ran this boot.
#[derive(Clone, Copy)]
enum Fill {
    Fatal,
    Boot,
}

/// The text grid a framebuffer offers, or `None` when it offers none.
fn geometry(fb: &Fb) -> Option<(usize, usize)> {
    let cols = (fb.width as usize / GLYPH_W).min(MAX_COLS);
    let rows = (fb.height as usize / GLYPH_H).min(MAX_ROWS);
    (cols != 0 && rows != 0).then_some((cols, rows))
}

/// Display rows `text` occupies, wrapping at `cols`; wraps rather than clips, since a demangled backtrace symbol routinely exceeds 160 columns.
fn count_rows(text: &[u8], cols: usize) -> usize {
    let mut rows = 1;
    let mut col = 0usize;
    let mut i = 0usize;
    while i < text.len() {
        let newline = text[i] == b'\n';
        i += 1;
        col += 1;
        if newline || col == cols {
            rows += 1;
            col = 0;
        }
    }
    rows
}

/// Total display rows, pages, and rows per page. A text that fits gets one
/// page; one that does not gives the bottom row to the `[page n/m]` footer.
fn pagination(text: &[u8], cols: usize, grid_rows: usize) -> (usize, usize, usize) {
    let total = count_rows(text, cols);
    if total <= grid_rows || grid_rows < 2 {
        return (total, 1, grid_rows);
    }
    let per = grid_rows - 1;
    (total, total.div_ceil(per), per)
}

/// Where a display row starts, and which log line it is part of: colour belongs to the record, so the line index carries it through the wrap.
#[derive(Clone, Copy)]
struct Row {
    at: u32,
    line: u32,
}

/// Display rows `first ..`, one per slot in `out`; rows past the end get `text.len()`, which draws nothing.
fn row_offsets(text: &[u8], cols: usize, first: usize, out: &mut [Row]) {
    let len = text.len();
    for slot in out.iter_mut() {
        *slot = Row { at: len as u32, line: 0 };
    }
    if first == 0 {
        if let Some(slot) = out.first_mut() {
            *slot = Row { at: 0, line: 0 };
        }
    }
    let mut row = 0usize;
    let mut col = 0usize;
    let mut i = 0usize;
    let mut line = 0u32;
    while i < len {
        let newline = text[i] == b'\n';
        i += 1;
        col += 1;
        if newline {
            line += 1;
        }
        if newline || col == cols {
            row += 1;
            col = 0;
            if row >= first {
                let slot = row - first;
                if slot >= out.len() {
                    return;
                }
                out[slot] = Row { at: i as u32, line };
            }
        }
    }
}

/// Whether a later pass must tell this paint apart from the panel being
/// drawn over. Only the report does — a checkpoint's screen is superseded, a fatal one's by nothing.
#[derive(Clone, Copy, PartialEq)]
enum Watch {
    No,
    Yes,
}

/// Pixels of the watched paint remembered, so a later pass can tell "still up" from "drawn over" without a copy of the panel.
const PROBES: usize = 128;

/// How many of `PROBES` are a grid over the panel rather than ink of the text, catching a repaint with no glyphs over it.
/// Neither replaces the other: a grid probe on the report's own black background can't catch a same-colour overwrite the way an ink probe can.
const GRID_PROBES: usize = 32;
const GRID_COLS: usize = 8;
const GRID_ROWS: usize = GRID_PROBES / GRID_COLS;
const _: () = assert!(GRID_COLS > 1 && GRID_ROWS > 1);

/// One ink probe every this many inked glyphs, so probes span the page rather than clustering on its first line.
const PROBE_STRIDE: usize = 29;

static PROBE_AT: [AtomicU32; PROBES] = [const { AtomicU32::new(0) }; PROBES];
static PROBE_PX: [AtomicU32; PROBES] = [const { AtomicU32::new(0) }; PROBES];
static PROBE_N: AtomicUsize = AtomicUsize::new(0);

fn paint(fill: Fill, view: View, page: Page, watch: Watch) {
    let Some(fb) = snapshot() else { return };
    if !mapped(&fb) {
        return;
    }
    let Some((cols, grid_rows)) = geometry(&fb) else { return };
    let text = view.text;
    let len = text.len();
    let (total, pages, per) = pagination(text, cols, grid_rows);
    // `Last` is the newest `per` rows, not page `pages - 1`, so both an even
    // and uneven division fill the screen; the footer's page number therefore
    // can't be derived from `first` and `shown` carries it separately.
    let newest = total.saturating_sub(per);
    let (first, shown) = match page {
        Page::Last => (newest, pages),
        Page::Nth(n) => (n.saturating_mul(per).min(newest), (n + 1).min(pages)),
    };

    fill_screen(
        &fb,
        match fill {
            Fill::Fatal => rgb(&fb, 0x60, 0x00, 0x00),
            Fill::Boot => 0,
        },
    );

    // The only array in this module; `per <= MAX_ROWS` keeps the slice in bounds.
    let mut row_start = [Row { at: 0, line: 0 }; MAX_ROWS];
    let draw = per.min(total - first).min(MAX_ROWS);
    row_offsets(text, cols, first, &mut row_start[..draw]);

    let white = rgb(&fb, 0xFF, 0xFF, 0xFF);
    let alert = rgb(&fb, 0xFF, 0x50, 0x50);

    let mut inked = 0usize;
    let mut probes = 0usize;
    for (r, row) in row_start[..draw].iter().enumerate() {
        // Colour comes from the record's `Level`, so it holds for every display row a wrapped line occupies.
        let color = if view.is_alert(row.line as usize) { alert } else { white };
        let mut off = row.at as usize;
        let mut c = 0;
        while c < cols && off < len {
            let byte = text[off];
            if byte == b'\n' {
                break;
            }
            draw_glyph(&fb, c, r, byte, color);
            if watch == Watch::Yes {
                if let Some((bx, by)) = glyph_ink(byte) {
                    if inked.is_multiple_of(PROBE_STRIDE) && probes < PROBES - GRID_PROBES {
                        let (x, y) = (c * GLYPH_W + bx, r * GLYPH_H + by);
                        PROBE_AT[probes].store(((y as u32) << 16) | x as u32, Ordering::Relaxed);
                        probes += 1;
                    }
                    inked += 1;
                }
            }
            off += 1;
            c += 1;
        }
    }

    if pages > 1 {
        draw_footer(&fb, cols, grid_rows - 1, shown, pages, white);
    }

    flush_stores();
    if watch == Watch::Yes {
        sample_probes(&fb, probes);
    }
}

/// Read back what this paint left at each probe (not assumed), after the
/// grid is added to the draw loop's ink — a probe outside the published byte count was never written.
fn sample_probes(fb: &Fb, ink: usize) {
    let mut n = ink.min(PROBES - GRID_PROBES);
    let w = (fb.width as usize).saturating_sub(1);
    let h = (fb.height as usize).saturating_sub(1);
    for i in 0..GRID_PROBES {
        // Corners included, where a taskbar or the strip below the last row sits, and where `Ppm::fill` reads.
        let (x, y) = (w * (i % GRID_COLS) / (GRID_COLS - 1), h * (i / GRID_COLS) / (GRID_ROWS - 1));
        PROBE_AT[n].store(((y as u32) << 16) | x as u32, Ordering::Relaxed);
        n += 1;
    }
    for i in 0..n {
        let at = PROBE_AT[i].load(Ordering::Relaxed);
        let px = get_pixel(fb, (at & 0xFFFF) as usize, (at >> 16) as usize);
        PROBE_PX[i].store(px, Ordering::Relaxed);
    }
    PROBE_N.store(n, Ordering::Relaxed);
}

/// Whether every probe still holds what the last paint left there; no probes means nothing painted, so this answers yes.
fn panel_carries_report(fb: &Fb) -> bool {
    let n = PROBE_N.load(Ordering::Relaxed).min(PROBES);
    (0..n).all(|i| {
        let at = PROBE_AT[i].load(Ordering::Relaxed);
        get_pixel(fb, (at & 0xFFFF) as usize, (at >> 16) as usize)
            == PROBE_PX[i].load(Ordering::Relaxed)
    })
}

/// Put every store this module has made on the bus: the scanout is write-combining, and stores can sit in a buffer with nothing to evict them.
fn flush_stores() {
    // SAFETY: `SFENCE` (SDM Vol. 3A §11.3.1) is the only way to drain a write-combining buffer; it touches no memory or register.
    unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
}

/// `[page 2/4]` on the bottom row; not decoration — the pager advances on a timer with no key to press.
fn draw_footer(fb: &Fb, cols: usize, row: usize, page: usize, pages: usize, color: u32) {
    let mut buf = [0u8; 24];
    let mut n = 0;
    for &b in b"[page " {
        buf[n] = b;
        n += 1;
    }
    n += write_num(&mut buf[n..], page);
    buf[n] = b'/';
    n += 1;
    n += write_num(&mut buf[n..], pages);
    buf[n] = b']';
    n += 1;
    for (c, &byte) in buf[..n.min(cols)].iter().enumerate() {
        draw_glyph(fb, c, row, byte, color);
    }
}

/// Decimal `v` into the front of `out`, returning the bytes written.
/// `out` is the tail of `draw_footer`'s 24-byte buffer; both page numbers
/// are bounded by `SNAPSHOT_CAP`, far under the room that leaves for the `/` and `]` that follow.
fn write_num(out: &mut [u8], v: usize) -> usize {
    let mut digits = [0u8; 20];
    let mut n = 0;
    let mut v = v;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 || n == digits.len() {
            break;
        }
    }
    for i in 0..n.min(out.len()) {
        out[i] = digits[n - 1 - i];
    }
    n.min(out.len())
}

/// Whether the first and last framebuffer pages resolve in the *current*
/// CR3, not `kernel_cr3()`: a panic in syscall context runs on a user address space.
/// Proves it rather than assuming it, so broken paging becomes no console, never a fault inside the panic handler.
fn mapped(fb: &Fb) -> bool {
    let base = fb.ptr as u64;
    let Some(last) = base.checked_add(fb.bytes.saturating_sub(1)) else {
        return false;
    };
    mm::paging::present_in_current_cr3(base) && mm::paging::present_in_current_cr3(last)
}

/// `pixel_format` is 0 for RGB, 1 for BGR (`bootloader/src/main.rs`).
fn rgb(fb: &Fb, r: u32, g: u32, b: u32) -> u32 {
    if fb.format == 0 {
        r | (g << 8) | (b << 16)
    } else {
        b | (g << 8) | (r << 16)
    }
}

/// What is on the glass at one pixel, or 0 where nothing may be read — the
/// same clamp [`put_pixel`] applies, so a probe on a position that could not be written is never a false alarm.
fn get_pixel(fb: &Fb, x: usize, y: usize) -> u32 {
    let Some(row) = (y as u64).checked_mul(fb.stride_px as u64) else { return 0 };
    let Some(idx) = row.checked_add(x as u64).and_then(|v| v.checked_mul(4)) else { return 0 };
    if idx.saturating_add(4) > fb.bytes {
        return 0;
    }
    // SAFETY: a volatile read of the scanout has no safe spelling; bounded
    // by `idx + 4 <= fb.bytes` above, and `validate` refusing a null/out-of-map base.
    unsafe { core::ptr::read_volatile(fb.ptr.add(idx as usize) as *const u32) }
}

#[inline]
fn put_pixel(fb: &Fb, x: usize, y: usize, color: u32) {
    let Some(row) = (y as u64).checked_mul(fb.stride_px as u64) else { return };
    let Some(idx) = row.checked_add(x as u64).and_then(|v| v.checked_mul(4)) else { return };
    if idx.saturating_add(4) > fb.bytes {
        return;
    }
    // SAFETY: bounded exactly as `get_pixel` — same three checks, same validated descriptor.
    unsafe { core::ptr::write_volatile(fb.ptr.add(idx as usize) as *mut u32, color) };
}

/// Base pointer for a row, given `len` pixels will be written from it.
/// `None` means the row does not fit, and so does no later row.
fn row_base(fb: &Fb, y: usize, len: usize) -> Option<*mut u32> {
    let start = (y as u64).checked_mul(fb.stride_px as u64)?.checked_mul(4)?;
    let end = start.checked_add((len as u64).checked_mul(4)?)?;
    // SAFETY: proves the clamp once instead of once per pixel. Bounded by
    // the `then`: `start`/`end` are checked products and `end <= fb.bytes`
    // gates the pointer's existence.
    (end <= fb.bytes).then(|| unsafe { fb.ptr.add(start as usize) as *mut u32 })
}

/// Paint the whole panel a colour no glyph contains, over whatever is
/// there. The actuator for "something drew over the console's back": no
/// other painter can stage this, since `render` (the one that ignores the
/// userland claim) halts the machine on its way out.
#[cfg(feature = "test-actuators")]
pub fn graffiti() {
    let Some(fb) = snapshot() else { return };
    if !mapped(&fb) {
        return;
    }
    log!("SYS_DEBUG: painting over the screen a userland process owns");
    fill_screen(&fb, rgb(&fb, 0x00, 0xC0, 0x00));
    flush_stores();
}

/// Erases whatever the compositor left behind, so nothing on screen is
/// ambiguous about which boot it came from. Proves the clamp once per row,
/// not once per pixel: a boot checkpoint repaints several times over a
/// multi-megapixel panel.
fn fill_screen(fb: &Fb, color: u32) {
    let width = fb.width as usize;
    for y in 0..fb.height as usize {
        let Some(row) = row_base(fb, y, width) else { return };
        for x in 0..width {
            // SAFETY: `row_base` returns a pointer, not a slice, so this
            // loop pays no per-pixel bound; a volatile store has no safe
            // spelling. Bounded by `row_base` proving `width` u32s fit, and `x < width`.
            unsafe { core::ptr::write_volatile(row.add(x), color) };
        }
    }
}

/// The 16 rows the font draws a byte with; one mapping, so [`glyph_ink`]
/// cannot disagree with the draw.
fn glyph(byte: u8) -> &'static [u8] {
    let ch = match byte {
        b'\t' => b' ',
        0x20..=0x7E => byte,
        _ => b'.',
    };
    let base = (ch - 0x20) as usize * GLYPH_H;
    &FONT[base..base + GLYPH_H]
}

/// A pixel of this byte's glyph, in its cell. `None` for a glyph with no
/// ink — the only kind a probe must refuse, since a blank cell looks the same whether the report is still there or not.
fn glyph_ink(byte: u8) -> Option<(usize, usize)> {
    for (row, &bits) in glyph(byte).iter().enumerate() {
        for bit in 0..GLYPH_W {
            if bits & (0x80 >> bit) != 0 {
                return Some((bit, row));
            }
        }
    }
    None
}

fn draw_glyph(fb: &Fb, cell_x: usize, cell_y: usize, byte: u8, color: u32) {
    let ox = cell_x * GLYPH_W;
    let oy = cell_y * GLYPH_H;
    for (row, &bits) in glyph(byte).iter().enumerate() {
        if bits == 0 {
            continue;
        }
        for bit in 0..GLYPH_W {
            if bits & (0x80 >> bit) != 0 {
                put_pixel(fb, ox + bit, oy + row, color);
            }
        }
    }
}
