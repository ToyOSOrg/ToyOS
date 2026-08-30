//! The i8042 PS/2 controller: keyboard and TrackPoint aux port.
//!
//! The ISR is the sole reader of port 0x60 and must never take a `Lock`,
//! allocate, call `log!`, wake a waiter, or loop unboundedly — see
//! [`handler`]. Delivery is pinned to one CPU (`IRQ_CPU`); the drain's own
//! polled port I/O must run there too, with interrupts off. Init treats the
//! controller as untrusted hardware: every wait is bounded and a failure
//! costs the keyboard, never the boot.

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

use toyos_ps2::{KeyDecoder, KeyOutcome, MouseDecoder, MouseOutcome};

use crate::arch::cpu::{inb, outb};
use crate::arch::idt::I8042_VECTOR;
use crate::irq_ring::IrqSource;
use crate::log;
use crate::sync::Lock;
use crate::time::{Budget, Cadence, Duration};
use super::ioapic::{self, Gsi};

mod tally;

use tally::{Carried, Tally};

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const COMMAND: u16 = 0x64;

const OBF: u8 = 1 << 0;
const IBF: u8 = 1 << 1;
const AUXB: u8 = 1 << 5;

/// `IAPC_BOOT_ARCH` bit 1 (ACPI 6.5 §5.10): "the motherboard has a port
/// 60/64 keyboard controller".
const FADT_8042: u16 = 1 << 1;

/// A floating bus reads `0xff`; every wait below gives up on this value
/// rather than only a deadline, so a machine with no controller doesn't
/// spend the whole init budget waiting on hardware that isn't there.
const FLOATING_BUS: u8 = 0xFF;

const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_DISABLE_AUX: u8 = 0xA7;
const CMD_ENABLE_AUX: u8 = 0xA8;
const CMD_TEST_AUX: u8 = 0xA9;
const CMD_SELF_TEST: u8 = 0xAA;
const CMD_TEST_PORT1: u8 = 0xAB;
const CMD_DISABLE_PORT1: u8 = 0xAD;
const CMD_ENABLE_PORT1: u8 = 0xAE;
const CMD_WRITE_AUX: u8 = 0xD4;

const CFG_PORT1_IRQ: u8 = 1 << 0;
const CFG_PORT2_IRQ: u8 = 1 << 1;
const CFG_PORT1_CLOCK_OFF: u8 = 1 << 4;
const CFG_PORT2_CLOCK_OFF: u8 = 1 << 5;
const CFG_TRANSLATE: u8 = 1 << 6;

const ISA_IRQ_KEYBOARD: u8 = 1;
const ISA_IRQ_AUX: u8 = 12;

/// Larger than any legitimate burst (3-byte mouse packet + 4-byte extended
/// key); past this the drain masks the line rather than hold the CPU.
const ISR_BURST: usize = 16;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static QUARANTINE: AtomicBool = AtomicBool::new(false);
static KBD_EVENTS: AtomicU32 = AtomicU32::new(0);
static AUX_EVENTS: AtomicU32 = AtomicU32::new(0);
static LOST_EDGES: AtomicU32 = AtomicU32::new(0);
static DROPPED: AtomicU32 = AtomicU32::new(0);
static DROPPED_TOTAL: AtomicU32 = AtomicU32::new(0);
/// Pointer bytes the framer discarded resyncing. Zero in a healthy stream.
static DISCARDS: AtomicU32 = AtomicU32::new(0);
/// Set-1 overrun codes: a keyboard reporting lost bytes, not one we cannot decode.
static OVERRUNS: AtomicU32 = AtomicU32::new(0);
static KEYBOARD_GSI: AtomicU32 = AtomicU32::new(u32::MAX);
static AUX_GSI: AtomicU32 = AtomicU32::new(u32::MAX);

/// Interrupts split by whether the ISR found a byte behind them. One atomic
/// value, not two counters (`tally.rs`): read mid-burst, two counters would
/// answer "did anything arrive" wrongly.
static TALLY: Tally = Tally::new();

/// Bytes taken off the ring, counted in [`pop`] before the byte leaves it —
/// so a byte is always in the ring or in this count, never in neither.
static RX_BYTES: AtomicU32 = AtomicU32::new(0);

/// When the pin first asserted. Set only in the ISR — never in
/// `handler_poll`, whose bytes came from a poll, not an assertion.
static FIRST_IRQ_NS: AtomicU64 = AtomicU64::new(0);

/// `init` consumed a byte with the IRQ bits live (the read-back's response, or
/// a byte `handler_poll` took), so its edge can deliver empty; the first
/// interrupt settles the debt.
static ARM_EDGE_OWED: AtomicBool = AtomicBool::new(false);
/// That first interrupt was the arming edge: empty, its byte already init's.
static ARM_EDGE_CONSUMED: AtomicBool = AtomicBool::new(false);

/// When the pin last asserted.
static LAST_IRQ_NS: AtomicU64 = AtomicU64::new(0);

/// The CPU the vector is pinned to: the sole reader of port 0x60, and the
/// only CPU an `irq_ring` record for this source can exist on.
static IRQ_CPU: AtomicU32 = AtomicU32::new(u32::MAX);

fn is_irq_cpu() -> bool {
    IRQ_CPU.load(Ordering::Relaxed) == crate::arch::percpu::cpu_id()
}

// The first interrupt and the quiet verdict are each reported exactly once,
// from the pass that discovers them — never a wall-clock deadline, since
// `service` only runs inside a scheduler pass and an idle boot may never
// enter one again to notice it expired.

/// `init` never armed the pin, so there is nothing to say.
const HEALTH_OFF: u8 = 0;
/// Armed and watching.
const HEALTH_ARMED: u8 = 1;
/// A CPU ran out of work; the quiet verdict is owed and the next pass emits it.
const HEALTH_QUIET_DUE: u8 = 2;
/// The quiet verdict is out; still watching, since a later interrupt answers it.
const HEALTH_QUIET_SAID: u8 = 3;
/// The boot verdict is out; the counters speak for themselves from here.
const HEALTH_DONE: u8 = 4;
/// Bytes arrived and decoded to nothing, said naming them; still watching,
/// since the byte that completes a sequence may be one interrupt away.
const HEALTH_MUTE_SAID: u8 = 5;
/// The pin asserted with no byte behind it, distinct from `HEALTH_MUTE_SAID`
/// so the first byte that does decode to nothing is still reported.
const HEALTH_EMPTY_SAID: u8 = 6;
/// The mute verdict beat the sequence and named nothing; revised once, to
/// [`HEALTH_MUTE_SAID`], when a byte is first blamed.
const HEALTH_MUTE_BLIND: u8 = 7;

static HEALTH: AtomicU8 = AtomicU8::new(HEALTH_OFF);
static ARMED_NS: AtomicU64 = AtomicU64::new(0);

// Repeats, but only when the pin has asserted since the last line — so past
// the first repeat, silence means no interrupt, not a driver that stopped.
fn health_period_ns() -> u64 {
    /// A log-rate limit; an idle machine pays nothing.
    const HEALTH: Cadence = Cadence::every(
        Duration::from_secs(10),
        "the PMM dump's own cadence, and one line per 10s of typing",
    );
    if crate::actuator::i8042_fast_health() {
        Duration::from_millis(500).nanos()
    } else {
        HEALTH.nanos()
    }
}
static NEXT_REPORT_NS: AtomicU64 = AtomicU64::new(u64::MAX);
static REPORTED_IRQS: AtomicU32 = AtomicU32::new(0);

/// Bytes the drain did not turn into an event, oldest first — a byte a
/// decoder is still holding is not one of these.
const UNEXPLAINED_LEN: usize = 8;
static UNEXPLAINED: [AtomicU16; UNEXPLAINED_LEN] = [const { AtomicU16::new(0) }; UNEXPLAINED_LEN];
static UNEXPLAINED_N: AtomicU32 = AtomicU32::new(0);
const UNEXPLAINED_AUX: u16 = 1 << 8;

fn record_unexplained(byte: u8, aux: bool) {
    let n = UNEXPLAINED_N.fetch_add(1, Ordering::Relaxed) as usize;
    if let Some(slot) = UNEXPLAINED.get(n) {
        slot.store(u16::from(byte) | if aux { UNEXPLAINED_AUX } else { 0 }, Ordering::Relaxed);
    }
}

/// Renders ` no event from [0xe0, aux 0x08],` — empty when every byte decoded.
struct Unexplained;

impl core::fmt::Display for Unexplained {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let seen = UNEXPLAINED_N.load(Ordering::Relaxed) as usize;
        if seen == 0 {
            return Ok(());
        }
        write!(f, " no event from [")?;
        for (i, slot) in UNEXPLAINED.iter().take(seen).enumerate() {
            let value = slot.load(Ordering::Relaxed);
            if i > 0 {
                write!(f, ", ")?;
            }
            if value & UNEXPLAINED_AUX != 0 {
                write!(f, "aux ")?;
            }
            write!(f, "{:#04x}", value as u8)?;
        }
        if seen > UNEXPLAINED_LEN {
            write!(f, ", +{}", seen - UNEXPLAINED_LEN)?;
        }
        write!(f, "],")
    }
}

fn claim_health(from: u8, to: u8) -> bool {
    HEALTH
        .compare_exchange(from, to, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

/// Called from the idle loop, interrupts off, before halting: pure atomics,
/// no lock, no port I/O. `true` keeps the CPU awake for exactly one more
/// pass; an inactive driver answers `false`, so nothing spins.
pub fn verdict_due() -> bool {
    if !ACTIVE.load(Ordering::Relaxed) {
        return false;
    }
    match HEALTH.load(Ordering::Relaxed) {
        HEALTH_ARMED => {
            claim_health(HEALTH_ARMED, HEALTH_QUIET_DUE);
            true
        }
        HEALTH_QUIET_DUE => true,
        _ => false,
    }
}

fn millis_since_boot() -> u64 {
    crate::clock::nanos_since_boot() / 1_000_000
}

fn first_irq_ms() -> u64 {
    FIRST_IRQ_NS.load(Ordering::Relaxed) / 1_000_000
}

/// Say once whether the armed pin has ever asserted; "nothing decoded" is
/// claimed only once every arrived byte is accounted for, via the
/// `has_bytes` guard.
fn report_health(state: u8) {
    let counts = TALLY.read();
    let irqs = counts.irqs();
    if counts.carried > 0 || RX_BYTES.load(Ordering::Relaxed) > 0 {
        if has_bytes() {
            return;
        }
        let keys = KBD_EVENTS.load(Ordering::Relaxed);
        let motion = AUX_EVENTS.load(Ordering::Relaxed);
        // A further line fires only when the picture changes — blind to named
        // when the run ends and blames its bytes, mute to decoded when a key
        // arrives — so a half-arrived keystroke never freezes here as DONE.
        let next = if keys + motion > 0 {
            HEALTH_DONE
        } else if UNEXPLAINED_N.load(Ordering::Relaxed) == 0 {
            HEALTH_MUTE_BLIND
        } else {
            HEALTH_MUTE_SAID
        };
        if next == state || !claim_health(state, next) {
            return;
        }
        if next != HEALTH_DONE {
            log!(
                "i8042: {} interrupts and {} bytes, nothing decoded —{} first seen at {}ms",
                irqs,
                RX_BYTES.load(Ordering::Relaxed),
                Unexplained,
                first_irq_ms()
            );
        } else {
            log!(
                "i8042: the pin asserts — {} interrupts, {} bytes, {} keys, {} motion,{} first seen at {}ms",
                irqs,
                RX_BYTES.load(Ordering::Relaxed),
                keys,
                motion,
                Unexplained,
                first_irq_ms()
            );
        }
        to_screen();
        arm_repeat();
        return;
    }
    // The third case: the pin asserts but nothing has come over it — distinct
    // from "nothing decoded" (bytes exist) and "never asserted" below.
    if irqs > 0 {
        // Except init's own echo — the arming edge, whose byte it consumed:
        // the quiet verdict stands saying so, a second empty one goes below.
        if irqs == 1 && counts.empty == 1 && ARM_EDGE_CONSUMED.load(Ordering::Relaxed) {
            if state == HEALTH_QUIET_DUE && claim_health(HEALTH_QUIET_DUE, HEALTH_QUIET_SAID) {
                log!(
                    "i8042: armed at {}ms, idle at {}ms, 1 interrupt, the arming edge (its byte \
                     was init's own read-back) — the pin has never asserted for input (kbd GSI \
                     {}, aux GSI {})",
                    ARMED_NS.load(Ordering::Relaxed) / 1_000_000,
                    millis_since_boot(),
                    KEYBOARD_GSI.load(Ordering::Relaxed) as i64,
                    AUX_GSI.load(Ordering::Relaxed) as i64
                );
                to_screen();
            }
            return;
        }
        if state != HEALTH_EMPTY_SAID && claim_health(state, HEALTH_EMPTY_SAID) {
            log!(
                "i8042: {} interrupts and no byte behind any of them — the output buffer was empty when the ISR read it, first seen at {}ms",
                irqs,
                first_irq_ms()
            );
            to_screen();
            arm_repeat();
        }
        return;
    }
    if state == HEALTH_QUIET_DUE && claim_health(HEALTH_QUIET_DUE, HEALTH_QUIET_SAID) {
        log!(
            "i8042: armed at {}ms, idle at {}ms, 0 interrupts — the pin has never asserted (kbd GSI {}, aux GSI {})",
            ARMED_NS.load(Ordering::Relaxed) / 1_000_000,
            millis_since_boot(),
            KEYBOARD_GSI.load(Ordering::Relaxed) as i64,
            AUX_GSI.load(Ordering::Relaxed) as i64
        );
        to_screen();
    }
}

fn arm_repeat() {
    NEXT_REPORT_NS.store(crate::clock::nanos_since_boot() + health_period_ns(), Ordering::Relaxed);
}

/// Say the counters again, at most once per [`health_period_ns`] and only
/// when the pin has asserted since the last line. No `to_screen`: the ring
/// is the primary channel and this line already appears there.
fn report_counters() {
    let counts = TALLY.read();
    let irqs = counts.irqs();
    if irqs == REPORTED_IRQS.load(Ordering::Relaxed) {
        return;
    }
    let now = crate::clock::nanos_since_boot();
    let next = NEXT_REPORT_NS.load(Ordering::Relaxed);
    if now < next
        || NEXT_REPORT_NS
            .compare_exchange(next, now + health_period_ns(), Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    REPORTED_IRQS.store(irqs, Ordering::Relaxed);
    // `empty` is read from the same snapshot as `irqs`, so the two counts
    // cannot disagree about how many interrupts there were.
    log!(
        "i8042: {} interrupts, {} bytes, {} keys, {} motion, {} undecoded, {} discarded, {} overruns, {} dropped, {} lost edges, {} empty — last byte at {}ms",
        irqs,
        RX_BYTES.load(Ordering::Relaxed),
        KBD_EVENTS.load(Ordering::Relaxed),
        AUX_EVENTS.load(Ordering::Relaxed),
        UNEXPLAINED_N.load(Ordering::Relaxed),
        DISCARDS.load(Ordering::Relaxed),
        OVERRUNS.load(Ordering::Relaxed),
        DROPPED_TOTAL.load(Ordering::Relaxed),
        LOST_EDGES.load(Ordering::Relaxed),
        counts.empty,
        LAST_IRQ_NS.load(Ordering::Relaxed) / 1_000_000
    );
}

/// One status-register snapshot, for a machine with nothing else to explain
/// a quiet pin. Side-effect-free (0x64, and an RTE read under the topology's
/// own lock), so it need not run on `IRQ_CPU` and cannot race the ISR.
#[cfg(feature = "boot-actuators")]
pub fn report_line() {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    log!(
        "i8042: line status={:#04x} irqs={} bytes={} kbd {} aux {}",
        inb(STATUS),
        TALLY.read().irqs(),
        RX_BYTES.load(Ordering::Relaxed),
        Rte(KEYBOARD_GSI.load(Ordering::Relaxed)),
        Rte(AUX_GSI.load(Ordering::Relaxed)),
    );
}

/// `gsi=1 rte=0x0000000000000024`, or why there is no entry to print.
#[cfg(feature = "boot-actuators")]
struct Rte(u32);

#[cfg(feature = "boot-actuators")]
impl core::fmt::Display for Rte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 == u32::MAX {
            return write!(f, "unrouted");
        }
        match ioapic::redirection(Gsi(self.0)) {
            Some(entry) => write!(f, "gsi={} rte={:#018x}", self.0, entry),
            None => write!(f, "gsi={} rte=busy", self.0),
        }
    }
}

/// Put the verdict on the panel too, but only when there is no other
/// channel: declines once `serial::has_console()`, mirroring
/// `panic_flush`'s own test.
fn to_screen() {
    if crate::drivers::serial::has_console() {
        return;
    }
    crate::drivers::panic_console::boot_checkpoint();
}

/// The decoder saw a device reset. Handled on `IRQ_CPU`, whichever CPU noticed.
static AUX_RESET_PENDING: AtomicBool = AtomicBool::new(false);
static AUX_REENABLE_FAILURES: AtomicU32 = AtomicU32::new(0);

/// Sized for "the controller is gone": a live device acks a one-byte command
/// in well under a millisecond, and this is interrupts-off time.
const AUX_REENABLE: Budget = Budget::of(
    Duration::from_millis(30),
    "the re-enable handshake is abandoned and counted against AUX_REENABLE_GIVE_UP",
);

/// Consecutive re-enable failures before the aux line is masked and the
/// pointer written off, rather than retried forever.
const AUX_REENABLE_GIVE_UP: u32 = 3;

// Single producer, pinned to one CPU behind an interrupt gate so the
// handler cannot nest; Release/Acquire on both indices since a drain can
// run on any CPU.

const RING_LEN: usize = 256;
const AUX_FLAG: u64 = 1 << 8;
/// Microsecond arrival time in the rest of the slot. The mouse framer
/// resyncs on the gap between adjacent bytes, so batching would flatten it.
const TIME_SHIFT: u32 = 9;

static BYTES: [AtomicU64; RING_LEN] = [const { AtomicU64::new(0) }; RING_LEN];
static HEAD: AtomicU32 = AtomicU32::new(0);
static TAIL: AtomicU32 = AtomicU32::new(0);

fn push_isr(byte: u8, aux: bool, arrived_ns: u64) {
    let head = HEAD.load(Ordering::Relaxed);
    if head.wrapping_sub(TAIL.load(Ordering::Acquire)) as usize >= RING_LEN {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let slot = &BYTES[head as usize % RING_LEN];
    let value = ((arrived_ns / 1_000) << TIME_SHIFT)
        | if aux { AUX_FLAG } else { 0 }
        | byte as u64;
    slot.store(value, Ordering::Relaxed);
    HEAD.store(head.wrapping_add(1), Ordering::Release);
}

fn pop() -> Option<(u8, bool, u64)> {
    let tail = TAIL.load(Ordering::Relaxed);
    if tail == HEAD.load(Ordering::Acquire) {
        return None;
    }
    let value = BYTES[tail as usize % RING_LEN].load(Ordering::Relaxed);
    // Counted before the slot releases: a byte is in the ring or in this
    // count, never in neither. See [`RX_BYTES`].
    RX_BYTES.fetch_add(1, Ordering::Relaxed);
    TAIL.store(tail.wrapping_add(1), Ordering::Release);
    Some((value as u8, value & AUX_FLAG != 0, (value >> TIME_SHIFT) * 1_000))
}

fn has_bytes() -> bool {
    HEAD.load(Ordering::Acquire) != TAIL.load(Ordering::Relaxed)
}

/// Under `i8042-fault`, armed after init so the next interrupt looks
/// permanently full — the only way to reach the ISR's bound without a
/// genuinely broken controller.
static FAULT: AtomicBool = AtomicBool::new(false);

/// Under `i8042-split-burst`: past [`SPLIT_CAP`] taken bytes the ISR answers
/// empty until [`SPLIT_RESCUED`] — the verdict-beats-the-sequence interleaving, staged.
static SPLIT_TAKEN: AtomicU32 = AtomicU32::new(0);
static SPLIT_RESCUED: AtomicBool = AtomicBool::new(false);
const SPLIT_CAP: u32 = 4;

fn split_hidden() -> bool {
    crate::actuator::i8042_split_burst()
        && !SPLIT_RESCUED.load(Ordering::Relaxed)
        && SPLIT_TAKEN.load(Ordering::Relaxed) >= SPLIT_CAP
}

#[inline]
fn buffer_full(status: u8) -> bool {
    if crate::actuator::i8042_fault() && FAULT.load(Ordering::Relaxed) {
        return true;
    }
    status & OBF != 0
}

/// Rust half of the pin-interrupt handler. Read the module doc before adding
/// anything to it.
pub extern "sysv64" fn handler() {
    crate::irq_census::irq_took!(I8042);
    let timestamp = crate::clock::nanos_since_boot();
    // No compare-exchange: this handler cannot nest, so there's no second writer.
    let first = FIRST_IRQ_NS.load(Ordering::Relaxed) == 0;
    if first {
        FIRST_IRQ_NS.store(timestamp, Ordering::Relaxed);
    }
    LAST_IRQ_NS.store(timestamp, Ordering::Relaxed);
    let mut n = 0;
    while n < ISR_BURST {
        let status = inb(STATUS);
        if !buffer_full(status) || split_hidden() {
            break;
        }
        // Timestamped per byte, not once for the burst: the mouse framer
        // resyncs on the gap between adjacent bytes, and a burst would flatten it.
        push_isr(inb(DATA), status & AUXB != 0, crate::clock::nanos_since_boot());
        if crate::actuator::i8042_split_burst() {
            SPLIT_TAKEN.fetch_add(1, Ordering::Relaxed);
        }
        n += 1;
    }
    if n == ISR_BURST && buffer_full(inb(STATUS)) {
        // It cannot mask the line itself — that needs the I/O APIC lock.
        QUARANTINE.store(true, Ordering::Relaxed);
    }
    // Only the first interrupt can be the arming edge (IRR delivers it before
    // any later assertion), and it settles the debt either way.
    if first && ARM_EDGE_OWED.swap(false, Ordering::Relaxed) && n == 0 {
        ARM_EDGE_CONSUMED.store(true, Ordering::Relaxed);
    }
    // Recorded after the burst, not before: the Release here also publishes
    // the bytes above it, so no reader sees the count without the bytes.
    TALLY.record(if n == 0 { Carried::Nothing } else { Carried::Bytes });
    if n > 0 {
        crate::irq_ring::isr_publish(IrqSource::I8042, timestamp);
        crate::preempt::set_need_resched();
    }
    crate::arch::apic::eoi();
}

/// Bytes a decoder has taken but not yet resolved. Held until the byte that
/// *ends* the run, so a multi-byte undecodable sequence can be named whole
/// rather than by its last byte alone.
struct Partial {
    bytes: [u8; UNEXPLAINED_LEN],
    len: usize,
}

impl Partial {
    const fn new() -> Self {
        Self { bytes: [0; UNEXPLAINED_LEN], len: 0 }
    }

    fn push(&mut self, byte: u8) {
        if self.len < UNEXPLAINED_LEN {
            self.bytes[self.len] = byte;
            self.len += 1;
        }
    }

    /// The run produced an event, so none of it is a suspect.
    fn clear(&mut self) {
        self.len = 0;
    }

    /// The run ended on `last` and produced nothing. All of it is.
    fn blame(&mut self, last: u8, aux: bool) {
        for i in 0..self.len {
            record_unexplained(self.bytes[i], aux);
        }
        self.len = 0;
        record_unexplained(last, aux);
    }
}

struct Decoders {
    keys: KeyDecoder,
    pointer: MouseDecoder,
    kbd_partial: Partial,
    aux_partial: Partial,
}

/// No ISR may lock this: `drain` holds it in thread context, so an ISR
/// taking it here self-deadlocks the CPU.
static PS2: Lock<Decoders> = Lock::new(Decoders {
    keys: KeyDecoder::new(),
    pointer: MouseDecoder::new(),
    kbd_partial: Partial::new(),
    aux_partial: Partial::new(),
});

/// Turn whatever the ISR published into events and wakes. Runs at the top of
/// every scheduler pass on every CPU, so the idle cost is one atomic load.
pub fn service() {
    // Unconditional and first: an undrained `irq_ring` record keeps
    // `any_pending_self` true, spinning a CPU that never halts.
    let recorded = crate::irq_ring::take(IrqSource::I8042).is_some();
    if QUARANTINE.load(Ordering::Relaxed) {
        quarantine();
        return;
    }
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    // Polled I/O: only `IRQ_CPU` may run this; another CPU leaves the
    // request standing until its own next pass.
    if AUX_RESET_PENDING.load(Ordering::Relaxed) && is_irq_cpu() {
        aux_reenable();
    }
    widen_edge_window();
    // The staged split's second half: once the mute verdict is out, the hidden
    // bytes are polled in — interrupts off, `handler_poll` shares `push_isr`'s producer seat.
    if crate::actuator::i8042_split_burst()
        && !SPLIT_RESCUED.load(Ordering::Relaxed)
        && HEALTH.load(Ordering::Relaxed) >= HEALTH_MUTE_SAID
        && is_irq_cpu()
    {
        SPLIT_RESCUED.store(true, Ordering::Relaxed);
        let _irq = crate::hw::IrqGuard::close();
        handler_poll();
    }
    if has_bytes() {
        // Asked again with bytes in hand: a record read absent may belong
        // to an interrupt that arrived just after that read.
        let recorded = recorded || crate::irq_ring::take(IrqSource::I8042).is_some();
        service_bytes(recorded);
    }
    // Last: reported from the top, the first pass would print "0 bytes".
    let health = HEALTH.load(Ordering::Relaxed);
    if health != HEALTH_DONE {
        report_health(health);
    }
    report_counters();
}

/// Under `i8042-edge-race`, widens the window between reading the record and
/// reading the ring so an interrupt can land inside it.
fn widen_edge_window() {
    if !crate::actuator::i8042_edge_race() {
        return;
    }
    for _ in 0..200 {
        core::hint::spin_loop();
    }
}

/// Decode what the ISR left in the ring and wake whoever it belongs to.
/// `recorded` is whether this pass found an `irq_ring` record for the source.
fn service_bytes(recorded: bool) {
    // Only `IRQ_CPU` can hold a record for this source; counting `!recorded`
    // on any other CPU would report a lost edge on every `--smp N>1` boot.
    if !recorded && is_irq_cpu() {
        // Loud the first time, silent after: nothing reads a rate.
        if LOST_EDGES.fetch_add(1, Ordering::Relaxed) == 0 {
            log!("i8042: bytes with no IRQ record — an edge was lost");
        }
    }

    let Drained { bytes, keys, motion, aux_reset } = drain();

    // Wake only when the decode queued something, or a stray wake parks the
    // next reader until the following real event.
    let woke_kb = keys > 0;
    if woke_kb {
        crate::inbox::Source::Keyboard.wake();
    }
    let woke_ms = motion > 0;
    if woke_ms {
        crate::inbox::Source::Mouse.wake();
    }
    trace_drain(bytes, keys, motion, woke_kb, woke_ms);

    if aux_reset {
        AUX_RESET_PENDING.store(true, Ordering::Relaxed);
    }
}

struct Drained {
    bytes: usize,
    keys: usize,
    motion: usize,
    aux_reset: bool,
}

/// Consume the ring. Releases `PS2` before returning, so the caller's wakes
/// never run under the driver lock. Lock order: PS2 before KEY_BUF, never
/// the reverse.
fn drain() -> Drained {
    let mut state = PS2.lock();
    let mut out = Drained { bytes: 0, keys: 0, motion: 0, aux_reset: false };
    let mut lost = false;

    let dropped = DROPPED.swap(0, Ordering::Relaxed);
    DROPPED_TOTAL.fetch_add(dropped, Ordering::Relaxed);
    if dropped > 0 {
        // Never expected: 256 slots against ~300 B/s, drained every pass.
        log!("i8042: ring overflow, {} bytes dropped — resyncing", dropped);
        // A hole in a framed stream: both decoders' partial state is
        // meaningless now, and the pointer would stay one byte off forever.
        state.keys.reset();
        state.pointer.reset();
        state.kbd_partial.clear();
        state.aux_partial.clear();
        lost = true;
    }

    while let Some((byte, aux, arrived)) = pop() {
        out.bytes += 1;
        // Whether the run is over and whether it produced anything — a
        // dropped break or a zero-motion packet counts as "nothing" too.
        let explained = if aux {
            match state.pointer.feed(byte, arrived) {
                MouseOutcome::Pending => {
                    state.aux_partial.push(byte);
                    continue;
                }
                MouseOutcome::Packet { buttons, dx, dy } => {
                    let queued = crate::mouse::handle_motion(
                        crate::mouse::PointerSource::PS2,
                        buttons,
                        crate::mouse::Motion::Relative { dx, dy },
                        0,
                    );
                    if queued {
                        out.motion += 1;
                    }
                    queued
                }
                MouseOutcome::Reset => {
                    out.aux_reset = true;
                    true
                }
                MouseOutcome::Discarded => {
                    DISCARDS.fetch_add(1, Ordering::Relaxed);
                    false
                }
            }
        } else {
            match state.keys.feed(byte) {
                KeyOutcome::Pending => {
                    state.kbd_partial.push(byte);
                    continue;
                }
                KeyOutcome::Key { usage, pressed } => {
                    let queued = crate::keyboard::handle_key(usage, pressed);
                    if queued {
                        out.keys += 1;
                    }
                    queued
                }
                // Overrun codes explain themselves; counted, not blamed.
                KeyOutcome::Lost => {
                    OVERRUNS.fetch_add(1, Ordering::Relaxed);
                    lost = true;
                    true
                }
                KeyOutcome::None => false,
            }
        };
        let partial = if aux { &mut state.aux_partial } else { &mut state.kbd_partial };
        if explained {
            partial.clear();
        } else {
            partial.blame(byte, aux);
        }
    }

    if lost {
        // A held key's break, or the packet lifting a held pointer button,
        // may be among what was lost — so every held input is released.
        out.keys += crate::keyboard::release_all();
        if crate::mouse::release_buttons(crate::mouse::PointerSource::PS2) {
            out.motion += 1;
        }
    }

    KBD_EVENTS.fetch_add(out.keys as u32, Ordering::Relaxed);
    AUX_EVENTS.fetch_add(out.motion as u32, Ordering::Relaxed);
    // `RX_BYTES` is not here: `pop` already counted each byte as it took it.
    out
}

/// A controller producing bytes faster than the ISR's bound can drain them.
/// One masked line and a dead keyboard, never a spinning CPU.
fn quarantine() {
    QUARANTINE.store(false, Ordering::Relaxed);
    ACTIVE.store(false, Ordering::Relaxed);
    // The pin is about to be masked, so no health verdict follows this line.
    HEALTH.store(HEALTH_DONE, Ordering::Relaxed);
    // Force-released: nothing else can lift a held key or pointer button
    // once the line is masked.
    crate::keyboard::release_all();
    crate::mouse::release_buttons(crate::mouse::PointerSource::PS2);
    // The count, not the intent: the log line is only true if the mask took.
    let mut masked = 0;
    for line in [KEYBOARD_GSI.load(Ordering::Relaxed), AUX_GSI.load(Ordering::Relaxed)] {
        if line != u32::MAX && ioapic::set_masked(Gsi(line), true).is_ok() {
            masked += 1;
        }
    }
    log!(
        "i8042: quarantined — output buffer never emptied, masked={} (kbd={} aux={} lost={})",
        masked,
        KBD_EVENTS.load(Ordering::Relaxed),
        AUX_EVENTS.load(Ordering::Relaxed),
        LOST_EDGES.load(Ordering::Relaxed)
    );
}

/// `woke_*` are the gates the wakes actually ran under, not a re-derivation,
/// so a test can assert the gate agrees with the event count.
fn trace_drain(bytes: usize, keys: usize, motion: usize, woke_kb: bool, woke_ms: bool) {
    if !crate::actuator::i8042_trace() {
        return;
    }
    log!(
        "i8042: drain bytes={} keys={} motion={} woke_kb={} woke_ms={}",
        bytes,
        keys,
        motion,
        u8::from(woke_kb),
        u8::from(woke_ms)
    );
}

// Each read below is done as its section's sole reader: init before the
// vector is armed, the aux re-enable on `IRQ_CPU` under `IrqGuard::close`,
// and the panic pager with every CPU halted — so no ISR ever races them.

fn deadline(millis: u64) -> u64 {
    crate::clock::nanos_since_boot() + millis * 1_000_000
}

/// Each stage below is a [`Budget`]; expiry names the stage and degrades the
/// probe's answer rather than panicking.
const fn ms(budget: Budget) -> u64 {
    budget.duration().millis()
}

/// Covers every controller-only step outside a named stage (disable, flush,
/// config r/w, both interface tests, the arming write) — none waits on an
/// EC, but the time is still spent.
const CONTROLLER: Budget = Budget::of(
    Duration::from_millis(250),
    "the stage that ran out is named and the probe reports DISABLED",
);
/// `0xAA`; separates "a controller" from "something else decoding
/// 0x60/0x64" (SMM-trapped ports for USB legacy emulation).
const SELFTEST: Budget = Budget::of(
    Duration::from_millis(500),
    "the controller is reported absent and the machine boots with no PS/2 input",
);
/// `0xF5`, the `0xF0 0x00` read-back and `0xF4` — each acknowledged by the
/// keyboard, not the controller.
const KEYBOARD: Budget = Budget::of(
    Duration::from_millis(750),
    "the keyboard stage is named as the one that ran out",
);
/// The aux port's `0xFF` is a device reset, answered with a real self-test:
/// this stage must not be shortened to fix an arithmetic error elsewhere.
const AUX_RESET: Budget = Budget::of(
    Duration::from_millis(600),
    "the pointer is written off and the keyboard half still comes up",
);

/// Derived, never written down independently: a literal short of the
/// stages' sum would let a slow-but-real machine exhaust it before the
/// arming write, and that timeout would then present as `DISABLED — cfg …
/// did not take`, a controller fault it is not.
fn init_budget_ms() -> u64 {
    if crate::actuator::i8042_budget_expired() {
        0
    } else {
        ms(CONTROLLER) + ms(SELFTEST) + ms(KEYBOARD) + ms(AUX_RESET)
    }
}

/// A stage's own deadline, clamped to the probe's; `None` once the probe's
/// own budget is already spent. Names the stage that ran out, since the
/// clamp alone can't tell a slow EC from a broken controller.
fn stage(millis: u64, budget: u64, name: &str) -> Option<u64> {
    if crate::clock::nanos_since_boot() >= budget {
        log!(
            "i8042: {}ms init budget spent before the {name} stage — no PS/2 input",
            init_budget_ms()
        );
        return None;
    }
    Some(deadline(millis).min(budget))
}

fn budget_spent(budget: u64) -> bool {
    crate::clock::nanos_since_boot() >= budget
}

/// The status register, or `None` when nothing decodes the port.
fn status() -> Option<u8> {
    match inb(STATUS) {
        FLOATING_BUS => None,
        other => Some(other),
    }
}

fn wait_writable(deadline: u64) -> bool {
    loop {
        let Some(status) = status() else { return false };
        if status & IBF == 0 {
            return true;
        }
        if crate::clock::nanos_since_boot() >= deadline {
            return false;
        }
    }
}

fn read_data(deadline: u64) -> Option<u8> {
    loop {
        if status()? & OBF != 0 {
            return Some(inb(DATA));
        }
        if crate::clock::nanos_since_boot() >= deadline {
            return None;
        }
    }
}

fn command(cmd: u8, deadline: u64) -> bool {
    wait_writable(deadline) && {
        // SAFETY: COMMAND (0x64) is the 8042's fixed command port; `cmd` is
        // always one of this module's own `CMD_*` constants, and the
        // controller has no path to memory.
        unsafe { outb(COMMAND, cmd) };
        true
    }
}

fn write_data(byte: u8, deadline: u64) -> bool {
    wait_writable(deadline) && {
        // SAFETY: DATA (0x60) is the controller's data port; `byte` is a
        // config word or device command, neither reaching memory.
        unsafe { outb(DATA, byte) };
        true
    }
}

fn read_config(deadline: u64) -> Option<u8> {
    command(CMD_READ_CONFIG, deadline).then(|| read_data(deadline)).flatten()
}

fn write_config(value: u8, deadline: u64) -> bool {
    command(CMD_WRITE_CONFIG, deadline) && write_data(value, deadline)
}

/// Iteration-bounded, not clock-bounded: draining 32 times is already past
/// any legitimate backlog, and OBF still set after that will not clear.
fn flush() -> bool {
    for _ in 0..32 {
        match status() {
            None => return false,
            Some(s) if s & OBF == 0 => return true,
            Some(_) => {
                inb(DATA);
            }
        }
    }
    status().is_some_and(|s| s & OBF == 0)
}

/// Sends a device command byte by byte, each acked with 0xFA. `aux` prefixes
/// every byte with the controller command that redirects it to port 2.
/// No retry on 0xFE (resend): unreachable on QEMU, and a silent retry would
/// hide the one wire-error case worth knowing about.
fn port_command(bytes: &[u8], deadline: u64, aux: bool) -> bool {
    let tag = if aux { "aux" } else { "kbd" };
    for &byte in bytes {
        if (aux && !command(CMD_WRITE_AUX, deadline)) || !write_data(byte, deadline) {
            log!("i8042: {tag} cmd {:#04x} — input buffer never cleared", byte);
            return false;
        }
        match read_data(deadline) {
            Some(0xFA) => {}
            other => {
                log!("i8042: {tag} cmd {:#04x} answered {:?}, not ack", byte, other);
                return false;
            }
        }
    }
    true
}

/// The keyboard port, unprefixed.
fn device_command(bytes: &[u8], deadline: u64) -> bool {
    port_command(bytes, deadline, false)
}

/// What `0xF0 0x00` established.
enum SetQuery {
    /// The read-back byte, translated like every other byte from port 1.
    Told(u8),
    /// A non-ack byte came back; the device does not implement the exchange.
    Refused(u8),
    /// Nothing came back, or the controller never took the write.
    Silent,
}

/// Ask which scancode set the keyboard is in. Read, never write: nothing
/// else in the machine's life sends the matching `0xF0 0x02` either (Linux's
/// `atkbd_select_set`, EDK2's `Ps2KeyboardDxe`), and a write cannot improve
/// on a read that already answers.
fn query_scancode_set(deadline: u64) -> SetQuery {
    if !write_data(0xF0, deadline) {
        return SetQuery::Silent;
    }
    match read_data(deadline) {
        Some(0xFA) => {}
        Some(other) => return SetQuery::Refused(other),
        None => return SetQuery::Silent,
    }
    if !write_data(0x00, deadline) {
        return SetQuery::Silent;
    }
    // A device may ack the command byte and then refuse the argument.
    match echo_the_argument(read_data(deadline)) {
        Some(0xFA) => {}
        Some(other) => return SetQuery::Refused(other),
        None => return SetQuery::Silent,
    }
    match read_data(deadline) {
        Some(set) => SetQuery::Told(set),
        None => SetQuery::Silent,
    }
}

/// Under `i8042-kbd-echo`, answers the argument byte `0xEE` — ECHO's own
/// reply, and the shape a real EC's refusal takes; QEMU always reports its set.
fn echo_the_argument(real: Option<u8>) -> Option<u8> {
    if crate::actuator::i8042_kbd_echo() {
        Some(0xEE)
    } else {
        real
    }
}

/// Same as `port_command`, prefixed for port 2.
fn aux_command(bytes: &[u8], deadline: u64) -> bool {
    port_command(bytes, deadline, true)
}

/// Re-enables data reporting after the device resets itself. Lines are left
/// unmasked: masking stops neither an executing ISR nor a latched vector,
/// and drops an edge on a masked entry outright.
fn aux_reenable() {
    AUX_RESET_PENDING.store(false, Ordering::Relaxed);
    let ok = {
        let _irq = crate::hw::IrqGuard::close();
        let budget = deadline(ms(AUX_REENABLE));
        // Masking the line doesn't stop the device: port 1 is disabled so a
        // stray keystroke mid-handshake can't be consumed as the aux ack.
        command(CMD_DISABLE_PORT1, budget);
        let ok = aux_command(&[0xF4], budget);
        command(CMD_ENABLE_PORT1, budget);
        // Edge delivery: a byte left in OBF means no further interrupt ever.
        handler_poll();
        ok
    };
    if ok {
        AUX_REENABLE_FAILURES.store(0, Ordering::Relaxed);
        log!("i8042: aux reset itself, reporting re-enabled");
        return;
    }
    let failures = AUX_REENABLE_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
    if failures < AUX_REENABLE_GIVE_UP {
        log!("i8042: aux reset itself, re-enable failed ({failures}/{AUX_REENABLE_GIVE_UP})");
        return;
    }
    let aux = AUX_GSI.swap(u32::MAX, Ordering::Relaxed);
    if aux != u32::MAX {
        let _ = ioapic::set_masked(Gsi(aux), true);
    }
    crate::mouse::release_buttons(crate::mouse::PointerSource::PS2);
    log!("i8042: aux re-enable failed {failures} times — pointer written off, line masked");
}

/// What firmware claims about the 8042 — never what decides. Under
/// `i8042-fadt-denial`, substitutes a real laptop's own FADT (8042 clear)
/// for QEMU's, whose flag and hardware always agree — the only way to test
/// that a denial doesn't stop the probe.
fn firmware_claim(rsdp_addr: u64) -> Result<(u8, u16), crate::drivers::acpi::TableError> {
    if crate::actuator::i8042_fadt_denial() {
        return Ok((6, 0x0011));
    }
    crate::drivers::acpi::iapc_boot_arch(rsdp_addr)
}

pub fn init(rsdp_addr: u64) {
    // Logged, never obeyed: bit 1 is one summary bit, while the handshake
    // below is three direct observations of the machine in front of us.
    match firmware_claim(rsdp_addr) {
        Ok((revision, flags)) => log!(
            "i8042: FADT rev {} iapc_boot_arch={:#06x}, bit 1 (8042) {} — probing either way",
            revision,
            flags,
            if flags & FADT_8042 != 0 { "set" } else { "clear" }
        ),
        Err(e) => log!("i8042: no trustworthy FADT ({e:?}) — firmware claims nothing either way"),
    }

    // One `inb` settles every machine that has nothing there, before a single
    // byte is written to ports that might belong to something else.
    if status().is_none() {
        log!("i8042: absent — port {STATUS:#x} reads {FLOATING_BUS:#04x}, nothing decodes it");
        return;
    }

    // The whole probe's budget: the sum of every stage plus the steps
    // between them, so no machine can spend it before the last stage has its own.
    let budget = deadline(init_budget_ms());

    // Firmware may leave scanning on. A keystroke arriving mid-handshake
    // makes the config read return a scancode and everything after garbage.
    command(CMD_DISABLE_PORT1, budget);
    command(CMD_DISABLE_AUX, budget);

    if !flush() {
        log!("i8042: absent (output buffer never drains)");
        return;
    }

    let Some(before) = read_config(budget) else {
        log!("i8042: absent (no config byte)");
        return;
    };
    // IRQs off until the device answers; translate on, since set 1 is what
    // this kernel decodes; port-1 clock on.
    let wanted = (before & !(CFG_PORT1_IRQ | CFG_PORT2_IRQ | CFG_PORT1_CLOCK_OFF)) | CFG_TRANSLATE;
    if !write_config(wanted, budget) {
        log!("i8042: absent (config write never accepted)");
        return;
    }
    match read_config(budget) {
        Some(v) if v == wanted => {}
        other => {
            log!("i8042: absent (cfg wrote {:#04x}, read back {:?})", wanted, other);
            return;
        }
    }

    let Some(selftest_deadline) = stage(ms(SELFTEST), budget, "self-test") else {
        return;
    };
    command(CMD_SELF_TEST, selftest_deadline);
    match read_data(selftest_deadline) {
        Some(0x55) => {}
        other => {
            log!(
                "i8042: absent (self-test {:?}, {}ms) — no PS/2 input",
                other,
                ms(SELFTEST)
            );
            return;
        }
    }
    // Some controllers reset the config byte across 0xAA.
    write_config(wanted, budget);

    command(CMD_TEST_PORT1, budget);
    let port1 = read_data(budget);
    if port1 != Some(0x00) {
        log!("i8042: port 1 interface test {:?} — no keyboard", port1);
        return;
    }
    // Enabling port 2 clears its clock-disable bit iff the port exists; the
    // interface test is the cheap way to learn it does not.
    command(CMD_ENABLE_AUX, budget);
    let dual = read_config(budget).is_some_and(|c| c & CFG_PORT2_CLOCK_OFF == 0);
    command(CMD_DISABLE_AUX, budget);
    let port2 = dual && {
        command(CMD_TEST_AUX, budget);
        read_data(budget) == Some(0x00)
    };

    command(CMD_ENABLE_PORT1, budget);
    log!(
        "i8042: ok selftest=0x55 cfg={:#04x}->{:#04x} port1=ok port2={}",
        before,
        wanted,
        if port2 { "ok" } else if dual { "failed" } else { "absent" }
    );

    // The slowest step on a real EC, hence its own budget.
    let Some(kbd) = stage(ms(KEYBOARD), budget, "keyboard") else {
        return;
    };
    if !device_command(&[0xF5], kbd) {
        log!("i8042: kbd would not stop scanning — disabled");
        return;
    }
    // The controller translates the reply too, so the read-back names the
    // wire format outright; refusing to decode a format we did not ask for
    // beats typing nonsense on a machine we cannot single-step.
    let (wire, how) = match query_scancode_set(kbd) {
        SetQuery::Told(0x41) => ("set2+xlat", "readback 0x41"),
        SetQuery::Told(0x01) => ("set1 raw", "readback 0x01, translation not applied"),
        SetQuery::Told(0x43) => {
            log!("i8042: kbd DISABLED — readback 0x43 means set 1 through the set2 table");
            return;
        }
        SetQuery::Told(0x02) => {
            log!("i8042: kbd DISABLED — readback 0x02 means set 2 raw on the wire");
            return;
        }
        SetQuery::Told(other) => {
            log!("i8042: kbd DISABLED — readback {:#04x} names no known wire format", other);
            return;
        }
        SetQuery::Refused(byte) if before & CFG_TRANSLATE != 0 => {
            log!(
                "i8042: kbd will not report its scancode set (0xF0 0x00 answered {:#04x}); firmware's own cfg {:#04x} has translate on, so the wire is set 1",
                byte,
                before
            );
            ("set2+xlat", "assumed, the set query was refused")
        }
        SetQuery::Refused(byte) => {
            log!(
                "i8042: kbd DISABLED - the set query answered {:#04x} and firmware's cfg {:#04x} has translate off, so nothing says what the wire carries",
                byte,
                before
            );
            return;
        }
        SetQuery::Silent => {
            log!("i8042: kbd DISABLED - the 0xF0 0x00 set query did not complete");
            return;
        }
    };
    if !device_command(&[0xF4], kbd) {
        log!("i8042: kbd would not resume scanning — disabled");
        return;
    }

    // The TrackPoint. Failure here costs the pointer and nothing else, so
    // every step logs and falls through rather than returning.
    let aux = port2 && {
        command(CMD_ENABLE_AUX, budget);
        stage(ms(AUX_RESET), budget, "aux reset").is_some_and(|reset| {
            // 0xFF answers 0xFA, then 0xAA (BAT ok), then the device id.
            aux_command(&[0xFF], reset)
                && read_data(reset) == Some(0xAA)
                && read_data(reset).is_some()
                && aux_command(&[0xF2], reset)
                && {
                    let id = read_data(reset);
                    if id != Some(0x00) {
                        log!("i8042: aux id {:?}, not a plain 3-byte mouse — framing anyway", id);
                    }
                    true
                }
                // No IntelliMouse knock: a fixed 3-byte frame is what makes
                // resync trivially self-healing.
                && aux_command(&[0xF3, 0x64], reset)
                && aux_command(&[0xE8, 0x03], reset)
                && aux_command(&[0xF4], reset)
        })
    };
    if port2 && !aux {
        log!("i8042: aux init failed — no pointer");
        command(CMD_DISABLE_AUX, budget);
    }

    // Steps above leave residue in the output buffer.
    flush();

    let apic_id = crate::arch::apic::id();
    let Some(kbd_line) = ioapic::gsi_for_isa_irq(ISA_IRQ_KEYBOARD) else {
        log!("i8042: no I/O APIC covers IRQ 1 — keyboard cannot be routed");
        return;
    };
    // `route` refuses rather than mis-route: a keyboard-less boot is
    // diagnosable, an interrupt delivered to the wrong CPU is not.
    if let Err(e) = ioapic::route(kbd_line.gsi, I8042_VECTOR, apic_id, kbd_line.trigger, kbd_line.polarity)
    {
        log!("i8042: GSI {} not routable to apic {}: {:?}", kbd_line.gsi.0, apic_id, e);
        return;
    }
    KEYBOARD_GSI.store(kbd_line.gsi.0, Ordering::Relaxed);
    // `apic_id` is this CPU's: everything downstream that says "the pinned
    // CPU" reads it from here.
    IRQ_CPU.store(crate::arch::percpu::cpu_id(), Ordering::Relaxed);

    let aux_line = aux.then(|| ioapic::gsi_for_isa_irq(ISA_IRQ_AUX)).flatten().filter(|l| {
        match ioapic::route(l.gsi, I8042_VECTOR, apic_id, l.trigger, l.polarity) {
            Ok(()) => true,
            Err(e) => {
                log!("i8042: GSI {} not routable to apic {}: {:?}", l.gsi.0, apic_id, e);
                false
            }
        }
    });
    if let Some(l) = aux_line {
        AUX_GSI.store(l.gsi.0, Ordering::Relaxed);
    }

    // Interrupts off while arming: a byte landing between the last flush and
    // the unmask would sit in OBF forever otherwise (edge delivery doesn't
    // re-assert until read).
    crate::arch::cpu::disable_interrupts();
    let mut config = wanted | CFG_PORT1_IRQ;
    if aux_line.is_some() {
        // `wanted` has port 2 disabled (from firmware's config); writing it
        // back unchanged would undo the 0xA8 enable above.
        config = (config | CFG_PORT2_IRQ) & !CFG_PORT2_CLOCK_OFF;
    }
    // The read-back may not be skipped: a controller that drops the write
    // still fills the output buffer and never asserts, so nothing else
    // downstream can tell.
    let wrote = write_config(config, budget);
    let readback = read_config(budget);
    if !wrote || readback != Some(config) {
        crate::arch::cpu::enable_interrupts();
        // A budget spent upstream makes this write and its read-back give up
        // instantly, indistinguishable from a dropped write — named explicitly.
        if budget_spent(budget) {
            log!(
                "i8042: DISABLED — the {}ms init budget was spent before the pin could be armed; this is a timeout, not a controller fault",
                init_budget_ms()
            );
        } else {
            match readback {
                Some(v) => log!(
                    "i8042: DISABLED — cfg {:#04x} did not take (read back {:#04x}); the pin would never assert",
                    config,
                    v
                ),
                None => log!(
                    "i8042: DISABLED — cfg {:#04x} did not take (no config byte came back); the pin would never assert",
                    config
                ),
            }
        }
        return;
    }
    // The read-back consumed its response with the IRQ bits live, so its edge is owed.
    ARM_EDGE_OWED.store(true, Ordering::Relaxed);
    let unmasked = ioapic::set_masked(kbd_line.gsi, false).is_ok();
    // Captured, not discarded: an aux GSI that won't unmask is the
    // TrackPoint silently dead on a boot that otherwise reads green.
    let aux_unmasked = aux_line.is_some_and(|l| ioapic::set_masked(l.gsi, false).is_ok());
    ACTIVE.store(true, Ordering::Relaxed);
    crate::keyboard::declare_source();
    if aux_line.is_some() {
        crate::mouse::declare_source();
    }
    ARMED_NS.store(crate::clock::nanos_since_boot(), Ordering::Relaxed);
    HEALTH.store(HEALTH_ARMED, Ordering::Relaxed);
    handler_poll();
    crate::arch::cpu::enable_interrupts();

    // Stages this boot's own arming edge: the vector, first, with no byte behind it.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::i8042_arm_edge() {
        crate::arch::apic::send_self(I8042_VECTOR);
    }

    log!(
        "i8042: kbd {} ({}) scanning on, GSI {} -> vec {:#04x} apic {} {}",
        wire,
        how,
        kbd_line.gsi.0,
        I8042_VECTOR,
        apic_id,
        if unmasked { "on" } else { "MASKED" }
    );
    match aux_line {
        Some(l) => log!(
            "i8042: aux rate=100 res=8/mm, GSI {} -> vec {:#04x} apic {} {}",
            l.gsi.0,
            I8042_VECTOR,
            apic_id,
            if aux_unmasked { "on" } else { "MASKED" }
        ),
        None => log!("i8042: no pointer on the aux port"),
    }

    if crate::actuator::i8042_fault() {
        FAULT.store(true, Ordering::Relaxed);
        log!("i8042: fault injection armed");
    }
}

/// One byte from the controller if it has one; never waits. Only legal once
/// every CPU is halted — port 0x60's sole reader is otherwise the ISR.
pub fn poll_byte() -> Option<(u8, bool)> {
    let status = inb(STATUS);
    if status & OBF == 0 {
        return None;
    }
    Some((inb(DATA), status & AUXB != 0))
}

/// The handler's drain loop, without the EOI. Runs with interrupts off on
/// `IRQ_CPU`, keeping `push_isr`'s producer single. Publishes the same
/// record the ISR does — a silent push here would manufacture a lost edge.
fn handler_poll() {
    let timestamp = crate::clock::nanos_since_boot();
    let mut n = 0;
    while n < ISR_BURST {
        let status = inb(STATUS);
        if status & OBF == 0 {
            break;
        }
        push_isr(inb(DATA), status & AUXB != 0, crate::clock::nanos_since_boot());
        n += 1;
    }
    if n > 0 {
        crate::irq_ring::isr_publish(IrqSource::I8042, timestamp);
        crate::preempt::set_need_resched();
    }
}
