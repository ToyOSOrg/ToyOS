//! Captures a crash's evidence before anything else can touch it, so a second
//! panic during reporting cannot erase the first.
//!
//! [`record_panic`] and [`record_fault`] run before any lock, formatter or
//! device: a bounded byte copy into statics, no allocation, nothing that can
//! itself panic. [`last_words`] renders both crashes to the 16550 raw, and
//! also as a record — the only channel a machine with no serial port has.

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::arch::cpu;
use crate::arch::percpu::CpuFaultState;
use crate::drivers::serial;
use crate::time::{Budget, Duration};

/// Slots in every per-CPU array here: an APIC id masked to six bits.
const SLOTS: usize = 64;

/// Per-CPU panic-reentry depth, indexed by masked APIC id, not by percpu: a
/// corrupted percpu block would make `swap_fault_state` itself fault here.
/// Per-CPU rather than one global flag, so a panic on one CPU cannot mask a
/// concurrent first panic on another; a masked-id collision is safe because
/// `halt_all_cpus` halts the second CPU anyway.
static PANIC_DEPTH: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];

/// This CPU's reentry depth, indexed by the id the hardware gives the CPU
/// (`arch::cpu::hardware_id`), which answers before any per-CPU state exists.
pub fn depth_slot() -> &'static AtomicU32 {
    &PANIC_DEPTH[cpu::hardware_id() as usize & (SLOTS - 1)]
}

/// Path capture bound; overflow is cut from the front (see [`copy_tail`]).
const FILE_BYTES: usize = 96;
const MSG_BYTES: usize = 128;

/// What a slot holds, or that it holds nothing.
#[derive(Clone, Copy)]
#[repr(u8)]
enum Kind {
    None = 0,
    Panic = 1,
    Fault = 2,
}

impl Kind {
    fn of(raw: u8) -> Self {
        match raw {
            1 => Self::Panic,
            2 => Self::Fault,
            // An unclaimed slot and a corrupted kind byte both read as "nothing captured".
            _ => Self::None,
        }
    }
}

/// One CPU's first unfinished crash. One CPU writes its own slot and the same
/// CPU reads it, with interrupts masked throughout, and no other CPU ever looks.
/// Lengths are stored `Release` after the bytes and loaded `Acquire` before
/// them, so a slot read mid-fill — an NMI panicking between the claim and the
/// copy — reports a short string rather than the previous crash's tail; every
/// other field is `Relaxed`.
struct Evidence {
    kind: AtomicU8,
    file_len: AtomicU8,
    msg_len: AtomicU8,
    /// The path did not fit and its head is what was dropped.
    file_cut: AtomicU8,
    /// Unmasked, so two CPUs sharing a slot is visible in the report.
    apic: AtomicU32,
    line: AtomicU32,
    column: AtomicU32,
    rip: AtomicU64,
    cr2: AtomicU64,
    error_code: AtomicU64,
    file: [AtomicU8; FILE_BYTES],
    /// A panic's literal message, or a fault's vector name.
    msg: [AtomicU8; MSG_BYTES],
}

impl Evidence {
    const fn new() -> Self {
        Self {
            kind: AtomicU8::new(Kind::None as u8),
            file_len: AtomicU8::new(0),
            msg_len: AtomicU8::new(0),
            file_cut: AtomicU8::new(0),
            apic: AtomicU32::new(0),
            line: AtomicU32::new(0),
            column: AtomicU32::new(0),
            rip: AtomicU64::new(0),
            cr2: AtomicU64::new(0),
            error_code: AtomicU64::new(0),
            file: [const { AtomicU8::new(0) }; FILE_BYTES],
            msg: [const { AtomicU8::new(0) }; MSG_BYTES],
        }
    }
}

static FIRST: [Evidence; SLOTS] = [const { Evidence::new() }; SLOTS];

fn evidence() -> &'static Evidence {
    &FIRST[cpu::hardware_id() as usize & (SLOTS - 1)]
}

/// Claims this CPU's slot for the first crash; declines if one is already claimed.
fn claim(slot: &Evidence, kind: Kind) -> bool {
    slot.kind
        .compare_exchange(Kind::None as u8, kind as u8, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

/// Copy a panic's site into this CPU's slot. The first statement of the panic
/// handler, before the early-boot branch and before the reentry guard.
pub fn record_panic(info: &core::panic::PanicInfo) {
    let slot = evidence();
    if !claim(slot, Kind::Panic) {
        return;
    }
    slot.apic.store(cpu::hardware_id(), Ordering::Relaxed);
    if let Some(location) = info.location() {
        copy_tail(&slot.file, &slot.file_len, &slot.file_cut, location.file().as_bytes());
        slot.line.store(location.line(), Ordering::Relaxed);
        slot.column.store(location.column(), Ordering::Relaxed);
    }
    if let Some(message) = info.message().as_str() {
        copy_head(&slot.msg, &slot.msg_len, message.as_bytes());
    }
}

/// Copies a fatal exception into this CPU's slot, before the fault reports
/// itself. `name` is copied rather than kept as a pointer, like the path: a
/// machine that has already failed once is not trusted to keep it valid.
pub fn record_fault(name: &str, rip: u64, cr2: u64, error_code: u64) {
    let slot = evidence();
    if !claim(slot, Kind::Fault) {
        return;
    }
    slot.apic.store(cpu::hardware_id(), Ordering::Relaxed);
    copy_head(&slot.msg, &slot.msg_len, name.as_bytes());
    slot.rip.store(rip, Ordering::Relaxed);
    slot.cr2.store(cr2, Ordering::Relaxed);
    slot.error_code.store(error_code, Ordering::Relaxed);
}

/// Releases this CPU's slot so the next crash captures its own first event.
/// Called wherever a CPU's fault state returns to `Normal`.
pub fn forget() {
    evidence().kind.store(Kind::None as u8, Ordering::Relaxed);
}

/// The last `dst.len()` bytes of `src`: a path's tail is its identity and its
/// head is whichever directory this build ran in.
fn copy_tail(dst: &[AtomicU8], len: &AtomicU8, cut: &AtomicU8, src: &[u8]) {
    let mut from = src.len().saturating_sub(dst.len());
    // A cut inside a multi-byte character makes the whole line unprintable.
    while from > 0 && matches!(src.get(from), Some(b) if b & 0xC0 == 0x80) {
        from += 1;
    }
    let tail = src.get(from..).unwrap_or(&[]);
    for (slot, &b) in dst.iter().zip(tail) {
        slot.store(b, Ordering::Relaxed);
    }
    // `Release`: the bytes before the length, for a reader that interrupts
    // this copy as much as for one on another CPU.
    len.store(tail.len() as u8, Ordering::Release);
    cut.store(u8::from(from > 0), Ordering::Relaxed);
}

/// The first `dst.len()` bytes of `src`, cut back to a character boundary.
fn copy_head(dst: &[AtomicU8], len: &AtomicU8, src: &[u8]) {
    let mut n = dst.len().min(src.len());
    while n > 0 && matches!(src.get(n), Some(b) if b & 0xC0 == 0x80) {
        n -= 1;
    }
    for (slot, &b) in dst.iter().zip(src.get(..n).unwrap_or(&[])) {
        slot.store(b, Ordering::Relaxed);
    }
    // `Release`, as in [`copy_tail`].
    len.store(n as u8, Ordering::Release);
}

/// A slot's bytes as text, in the caller's own buffer.
fn read<'a>(src: &[AtomicU8], len: &AtomicU8, out: &'a mut [u8]) -> &'a str {
    // `Acquire`: pairs with the copy's `Release`, so the bytes read are at
    // least the ones that length was stored after.
    let n = (len.load(Ordering::Acquire) as usize).min(out.len()).min(src.len());
    for (byte, slot) in out.iter_mut().zip(src.iter()) {
        *byte = slot.load(Ordering::Relaxed);
    }
    core::str::from_utf8(out.get(..n).unwrap_or(&[])).unwrap_or("<not utf-8>")
}

fn state_name(state: CpuFaultState) -> &'static str {
    match state {
        CpuFaultState::Normal => "Normal",
        CpuFaultState::PageFault => "PageFault",
        CpuFaultState::Fatal => "Fatal",
        CpuFaultState::Panic => "Panic",
    }
}

/// What the second panic's message says when it was formatted at runtime and
/// this module refused to run a formatter to find out.
const NOT_CAPTURED: &str = "<formatted at runtime; not captured>";

/// This CPU's captured crash, as the one or two lines a black-box report opens
/// with.
///
/// **The first thing in that report, before any tail of records.** The panel's
/// newest lines are the ones written *after* the crash — the register dump, the
/// page walk, the backtrace, the reboot bound's own line — so a report cut to
/// its tail is a report with the crash missing, which is what run 14's stick
/// carried. `out` is written into the page directly and drops what does not fit.
pub fn first_words(out: &mut impl core::fmt::Write) {
    let slot = evidence();
    let mut file_bytes = [0u8; FILE_BYTES];
    let mut msg_bytes = [0u8; MSG_BYTES];
    let kind = Kind::of(slot.kind.load(Ordering::Relaxed));
    let file = read(&slot.file, &slot.file_len, &mut file_bytes);
    let message = read(&slot.msg, &slot.msg_len, &mut msg_bytes);
    let cut = if slot.file_cut.load(Ordering::Relaxed) == 0 { "" } else { "..." };
    let (line, column) = (slot.line.load(Ordering::Relaxed), slot.column.load(Ordering::Relaxed));
    let apic = slot.apic.load(Ordering::Relaxed);
    let _ = match kind {
        Kind::Panic => core::writeln!(
            out,
            "PANIC (apic {apic}): panicked at {cut}{file}:{line}:{column}: {message}"
        ),
        Kind::Fault => core::writeln!(
            out,
            "FAULT (apic {apic}): {message} rip={:#018x} cr2={:#018x} err={:#018x}",
            slot.rip.load(Ordering::Relaxed),
            slot.cr2.load(Ordering::Relaxed),
            slot.error_code.load(Ordering::Relaxed),
        ),
        // The panic path ran without `record_panic` having claimed a slot,
        // which is a state worth saying rather than an empty first line.
        Kind::None => core::writeln!(out, "PANIC (apic {apic}): nothing was captured on this cpu"),
    };
}

/// Reports this CPU's captured crash and the panic (`second`) that just
/// reentered it. `prev` is `None` where the caller has no fault state to
/// give — the reentry guard runs before the state swap. `on_the_record` also
/// writes an `alert!`; false where the report path itself is the suspect, as
/// in the reentry guard.
pub fn last_words(
    header: &str,
    prev: Option<CpuFaultState>,
    second: &core::panic::PanicInfo,
    on_the_record: bool,
) {
    let slot = evidence();
    let mut file_bytes = [0u8; FILE_BYTES];
    let mut msg_bytes = [0u8; MSG_BYTES];
    let kind = Kind::of(slot.kind.load(Ordering::Relaxed));
    let file = read(&slot.file, &slot.file_len, &mut file_bytes);
    let message = read(&slot.msg, &slot.msg_len, &mut msg_bytes);
    let cut = if slot.file_cut.load(Ordering::Relaxed) == 0 { "" } else { "..." };
    let line = slot.line.load(Ordering::Relaxed);
    let column = slot.column.load(Ordering::Relaxed);
    let rip = slot.rip.load(Ordering::Relaxed);
    let cr2 = slot.cr2.load(Ordering::Relaxed);
    let error_code = slot.error_code.load(Ordering::Relaxed);

    let (second_file, second_line, second_column) = match second.location() {
        Some(location) => (location.file(), location.line(), location.column()),
        None => ("<no location>", 0, 0),
    };
    let second_message = second.message().as_str().unwrap_or(NOT_CAPTURED);

    let raw: fn(&[u8]) = serial::panic_raw;
    raw(b"\n!!! ");
    raw(header.as_bytes());
    raw(b" !!! (apic ");
    serial::panic_raw_dec(u64::from(cpu::hardware_id()));
    if let Some(prev) = prev {
        raw(b", the cpu was already in ");
        raw(state_name(prev).as_bytes());
    }
    raw(b")\n  first (apic ");
    serial::panic_raw_dec(u64::from(slot.apic.load(Ordering::Relaxed)));
    raw(b"): ");
    match kind {
        Kind::Panic => {
            raw(b"panic at ");
            raw(cut.as_bytes());
            raw(file.as_bytes());
            raw(b":");
            serial::panic_raw_dec(u64::from(line));
            raw(b":");
            serial::panic_raw_dec(u64::from(column));
            raw(b": ");
            raw(message.as_bytes());
        }
        Kind::Fault => {
            raw(message.as_bytes());
            raw(b" rip=");
            serial::panic_raw_hex(rip);
            raw(b" cr2=");
            serial::panic_raw_hex(cr2);
            raw(b" err=");
            serial::panic_raw_hex(error_code);
        }
        Kind::None => raw(b"nothing captured on this cpu"),
    }
    raw(b"\n  second: panic at ");
    raw(second_file.as_bytes());
    raw(b":");
    serial::panic_raw_dec(u64::from(second_line));
    raw(b":");
    serial::panic_raw_dec(u64::from(second_column));
    raw(b": ");
    raw(second_message.as_bytes());
    raw(b"\n");

    if !on_the_record {
        return;
    }
    // One ASCII line: the panel renders codepoints 0x20..=0x7E, one record per
    // row. The raw report above already went out because `alert!` can still fail.
    let state = prev.map_or("", state_name);
    match kind {
        Kind::Panic => alert!(
            "{header}: the cpu was already in {state}; first: panic at {cut}{file}:{line}:{column}: \
             {message}; second: panic at {second_file}:{second_line}:{second_column}: \
             {second_message}"
        ),
        Kind::Fault => alert!(
            "{header}: the cpu was already in {state}; first: {message} rip={rip:#018x} \
             cr2={cr2:#018x} err={error_code:#018x}; second: panic at \
             {second_file}:{second_line}:{second_column}: {second_message}"
        ),
        Kind::None => alert!(
            "{header}: the cpu was already in {state}; first: nothing captured on this cpu; \
             second: panic at {second_file}:{second_line}:{second_column}: {second_message}"
        ),
    }
}

/// Time `/system/bin/logd` gets to durably write the panic report before halt;
/// a `Budget` (not a `Bound`) because expiry degrades gracefully instead of panicking.
const LOG_FILE_DRAIN: Budget = Budget::of(
    Duration::from_millis(500),
    "the report reaches the panel and not /log",
);

// Read by tests/toyos.rs — keep in sync or its drift check fails.
const LOG_DRAIN_EXPIRED: &str = "the report did not reach /log";

/// Whether `/log` still owes this boot the report.
// True only before durable_ns passes `want` — logd publishes it after fsync returns, never before.
fn log_file_owed(want: u64) -> bool {
    crate::log::user::durable_ns() < want
}

/// Give `/system/bin/logd` a chance to put this report on the stick before the machine stops.
// The panic path never writes /log directly — every lock a write needs may already be held by the panicking thread itself.
fn wait_for_log_file() {
    // Skip when serial exists: panic_flush already got the report off the box, and waiting here would only delay the pager.
    if serial::has_console() {
        return;
    }
    // INVARIANT: nothing below runs before both facts this wait rests on hold.
    // The deadline is read off the calibrated clock, so an uncalibrated one
    // makes it unreachable; and what the wait is owed by is `logd`, which only
    // a released machine can run. On a boot that crashes before either — the
    // one this whole path exists for on a machine with no serial port — waiting
    // costs the seal and buys nothing, so it is skipped and not shortened.
    if !crate::clock::calibrated() || !crate::arch::smp::is_ready() {
        return;
    }
    // Sampled once — a sibling still logging on its way down must not be able to push this deadline out indefinitely.
    let want = crate::log::read::newest_committed_at_ns();
    if !log_file_owed(want) {
        return;
    }
    // Wake siblings first: one may be halted waiting for an interrupt with no timer armed to wake it otherwise.
    crate::arch::irqchip::kick_all_but_self();
    let deadline = crate::clock::now() + LOG_FILE_DRAIN.duration();
    while log_file_owed(want) {
        if crate::clock::now() >= deadline {
            // /log has failed to answer, so fold this into the still-unpainted panel snapshot — the panel is the only channel left.
            crate::log!(
                "panic: {LOG_DRAIN_EXPIRED} in {}ns; the panel is the only copy",
                LOG_FILE_DRAIN.nanos()
            );
            crate::drivers::panic_console::refresh_capture();
            return;
        }
        core::hint::spin_loop();
    }
}

/// Halt all CPUs: stop the others, flush pending log output, then hold this
/// machine's panel until a key retires the reboot bound or the bound returns it
/// to firmware. Every fatal path's one funnel.
// panic_flush bypasses the log-ring and serial locks — once the others are stopped a wedged holder never releases them, so taking them normally could deadlock.
pub fn halt_all_cpus() -> ! {
    // Before the wait and the panel: from here this machine holds a report for
    // whoever is in front of it, with interrupts masked and under a bound of its
    // own — which to a hard-lockup sample or a deadline poll is
    // indistinguishable from a wedge, and is the opposite of one. Both bounds,
    // because a `WEDGED` record either of them sealed would replace the report
    // this path exists to deliver.
    crate::hardlockup::stand_down();
    crate::deadline::stand_down();
    wait_for_log_file();
    crate::arch::irqchip::stop_other_cpus();
    let bound = crate::panic_reboot::arm(true);
    // Folded into the still-unpainted capture only where the panel is this
    // boot's only account of itself, exactly as `wait_for_log_file`'s own line
    // is: a refresh re-freezes the ring, and `screen_late_panic` reads the
    // panel for a record written *after* `capture()` to prove the paint comes
    // from the frozen snapshot. A machine with a console gets the arm line on it.
    if !serial::has_console() {
        crate::drivers::panic_console::refresh_capture();
    }
    // Render before the flush: it can't fail the proven serial channel, and a serial line then proves the paint already finished.
    let painted = crate::drivers::panic_console::render();
    // SAFETY: sound only once nothing else will run — the other CPUs are already stopped.
    unsafe { serial::panic_flush(); }
    // Must follow the flush — it's the deepest stack this path reaches.
    crate::arch::trap::report_fault_stack();
    // page_forever runs strictly after the flush: it is an unbounded loop and may only run once the serial report is out.
    // Only the CPU that painted watches the bound; the rest halt below, since two CPUs polling one keyboard would split every key.
    if painted {
        crate::drivers::panic_console::page_forever(bound);
    }
    cpu::halt();
}
