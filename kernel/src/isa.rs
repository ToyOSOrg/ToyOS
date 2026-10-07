//! A legacy function driven by a process: the I/O ports it decodes, opened in
//! the CPU's I/O permission bitmap, and the lines it raises, answered as
//! interrupt records on the claim.
//!
//! **What can be claimed is a row the boot filled, matched exactly.** A row is
//! filled once, before userland runs, and never changes: the i8042's, named by
//! an `isa:` selector, and the firmware's ACPI fixed hardware, named by its
//! class. A port no row names is never opened, a set that is not a whole row
//! is refused rather than trimmed to one, and a row with a port this kernel
//! declared (`arch::pio::holder`) is refused naming who holds it.
//!
//! **The ports belong to a process, not to the handle.** The first read of the
//! claim binds them to the process that reads it ([`bind`]); from then until
//! that process's teardown ([`process_ends`]) every switch onto one of its
//! threads opens them and every other switch closes them
//! (`arch::pio::switch_to`). A handle moved on after that answers nothing but
//! refusals, and the row is claimable again only once both the claim and the
//! process are gone. A binding reaches the process's other threads at their
//! next switch, and its end needs no switch at all: teardown runs once every
//! thread has left, and no thread that has left returns to Ring 3.
//!
//! **A line is routed once per boot and masked while no claim holds it**, since
//! an interrupt-remapping entry is never given back. Its handler counts into
//! the record a claimed PCI function's does and posts the claim's watch, and
//! the holder reads the record back the same way. A level line is masked by
//! its handler too, and stays masked until the holder has served what raised
//! it and acknowledged the claim ([`ack`]); it is masked from the claim until
//! the holder's first acknowledgement.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

use toyos_abi::pci::DeviceIrqRecord;
use toyos_abi::syscall::IsaId;
use toyos_userbound::Ports;

use crate::arch::pio;
use crate::device::ClaimError;
use crate::pcidev::record::Interrupt;
use crate::process::Pid;
use crate::sync::Lock;
use crate::watch::IrqWatch;

/// Every row any architecture fills: the i8042's and the ACPI fixed hardware's.
pub const MAX_ROWS: usize = 2;

/// One function a process may be handed whole.
pub struct Function {
    /// What the log calls it.
    pub name: &'static str,
    pub runs: Vec<Ports>,
    /// The ISA lines an `isa:` selector spells the row with; empty for a row
    /// only its class claims.
    pub irqs: Vec<u8>,
    /// The lines it raises, resolved.
    pub wires: Vec<pio::Wire>,
}

/// No process holds the row's ports.
const NOBODY: u32 = Pid::MAX.0;

/// Each row's function, written once by [`fill`].
static FUNCTIONS: [AtomicPtr<Function>; MAX_ROWS] = [const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_ROWS];

/// The row's lines, routed by its first claim and kept, read by its handler.
static LINES: [AtomicPtr<Vec<pio::Line>>; MAX_ROWS] = [const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_ROWS];

/// A line this machine could not route, which refuses every claim after it too.
static UNROUTABLE: [AtomicBool; MAX_ROWS] = [const { AtomicBool::new(false) }; MAX_ROWS];

/// A claim on the row exists.
static MINTED: [Lock<bool>; MAX_ROWS] = [const { Lock::new(false) }; MAX_ROWS];

/// The pid whose threads the row's ports are open for, or [`NOBODY`]; read by
/// every context switch, which takes no lock.
static BOUND: [AtomicU32; MAX_ROWS] = [const { AtomicU32::new(NOBODY) }; MAX_ROWS];

static IRQ: [Interrupt; MAX_ROWS] = [const { Interrupt::new() }; MAX_ROWS];

/// What a claim's poll waits on, one per row.
static WATCHES: [IrqWatch; MAX_ROWS] = [const { IrqWatch::new() }; MAX_ROWS];

/// Fill `row` with what it hands out. Boot's alone, once per row.
pub fn fill(row: usize, function: Function) {
    assert!(!crate::smp::is_ready(), "isa: {} filled after userland could claim it", function.name);
    let was = FUNCTIONS[row].swap(Box::into_raw(Box::new(function)), Ordering::Release);
    assert!(was.is_null(), "isa: row {row} filled twice");
}

fn function(row: usize) -> Option<&'static Function> {
    let at = FUNCTIONS[row].load(Ordering::Acquire);
    // SAFETY: `fill` stored a leaked `Box` once, and nothing frees it.
    (!at.is_null()).then(|| unsafe { &*at })
}

/// The row's ports, or `None` for a row the boot did not fill.
pub fn runs(row: usize) -> Option<&'static [Ports]> {
    function(row).map(|f| f.runs.as_slice())
}

fn lines(row: usize) -> &'static [pio::Line] {
    let at = LINES[row].load(Ordering::Acquire);
    // SAFETY: as `function`'s, stored by `claim_row`.
    if at.is_null() { &[] } else { unsafe { &*at } }
}

/// The row an `isa:` selector names whole.
pub fn claim(set: IsaId) -> Result<usize, ClaimError> {
    let row = (0..MAX_ROWS)
        .find(|&row| {
            function(row).is_some_and(|f| {
                !f.irqs.is_empty()
                    && set.ports().eq(f.runs.iter().flat_map(|run| run.iter()))
                    && set.irqs().eq(f.irqs.iter().copied())
            })
        })
        .ok_or(ClaimError::Absent)?;
    claim_row(row)
}

/// Mint the claim on `row`: its ports checked against every port this kernel
/// declared, its edge lines routed and unmasked, its level lines routed and
/// left masked for the holder's first acknowledgement.
pub fn claim_row(row: usize) -> Result<usize, ClaimError> {
    let function = function(row).ok_or(ClaimError::Absent)?;
    if let Some((run, holder)) = function.runs.iter().find_map(|&run| pio::holder(run).map(|h| (run, h))) {
        log!("isa: {}'s ports {:#x}+{} are {holder}'s", function.name, run.first(), run.count());
        return Err(ClaimError::KernelDriven);
    }
    let mut minted = MINTED[row].lock();
    if *minted || BOUND[row].load(Ordering::Acquire) != NOBODY {
        return Err(ClaimError::Owned);
    }
    if UNROUTABLE[row].load(Ordering::Relaxed) {
        return Err(ClaimError::Unusable);
    }
    if LINES[row].load(Ordering::Acquire).is_null() {
        let mut routed = Vec::new();
        for &wire in &function.wires {
            match pio::route(row, wire) {
                Ok(line) => routed.push(line),
                Err(why) => {
                    log!("isa: {} line {} not routable: {why}", function.name, pio::describe(wire));
                    UNROUTABLE[row].store(true, Ordering::Relaxed);
                    return Err(ClaimError::Unusable);
                }
            }
        }
        LINES[row].store(Box::into_raw(Box::new(routed)), Ordering::Release);
    }
    for &line in lines(row) {
        if !pio::level(line) {
            pio::set_masked(line, false);
        }
    }
    *minted = true;
    Ok(row)
}

/// The claim's last handle went: its lines masked, its record emptied and its
/// polls answered. The ports stay with the process that bound them until that
/// process ends.
pub fn release(row: usize) {
    for &line in lines(row) {
        pio::set_masked(line, true);
    }
    IRQ[row].clear();
    // The claim is gone, so a poll on it is answered rather than left for the
    // next holder's interrupts; the row stays minted until that is made, so no
    // next claim's poll is among the ones answered.
    WATCHES[row].cancel_polls();
    *MINTED[row].lock() = false;
}

/// The holder served what raised the row's level lines: unmask them. A row
/// with no level line has nothing to acknowledge.
pub fn ack(row: usize) -> Result<(), ()> {
    let mut level = lines(row).iter().copied().filter(|&line| pio::level(line)).peekable();
    if level.peek().is_none() {
        return Err(());
    }
    for line in level {
        pio::set_masked(line, false);
    }
    Ok(())
}

/// Open the row's ports to `pid` for the rest of its life. Called once per
/// claim, by the claim's first read, and never twice for a row: [`claim_row`]
/// mints none while a process holds its ports.
pub fn bind(row: usize, pid: Pid) {
    let name = function(row).expect("a claimed row was filled").name;
    match BOUND[row].compare_exchange(NOBODY, pid.raw(), Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => log!("isa: {name}'s ports are pid {pid}'s"),
        Err(held) => assert!(held == pid.raw(), "isa: row {row} is pid {held}'s, and pid {pid} bound it"),
    }
    // This thread is already running, so no switch opens them for it.
    let _irq = crate::arch::IrqGuard::close();
    pio::switch_to(Some(pid));
}

/// Whether `pid` holds the row's ports.
pub fn bound_to(row: usize, pid: Pid) -> bool {
    BOUND[row].load(Ordering::Acquire) == pid.raw()
}

/// Called from the process teardown, once every thread has left.
pub fn process_ends(pid: Pid) {
    for (row, bound) in BOUND.iter().enumerate() {
        if bound.compare_exchange(pid.raw(), NOBODY, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            log!("isa: {}'s ports went back with pid {pid}", function(row).expect("a bound row was filled").name);
        }
    }
}

/// The interrupts since the last read, or `None` for none.
pub fn take_record(row: usize) -> Option<DeviceIrqRecord> {
    let taken = IRQ[row].take();
    if taken.is_some() && IRQ[row].take_unannounced() {
        log!("isa: {} took its first interrupt", function(row).expect("a claimed row was filled").name);
    }
    taken.map(|count| DeviceIrqRecord { count })
}

pub fn has_irq(row: usize) -> bool {
    IRQ[row].armed()
}

/// Records one interrupt, masks the row's level lines and posts the claim's
/// watch. Called from the row's handler, so it takes no lock but the watch's
/// own and the I/O APIC's masked one, and allocates nothing.
pub fn isr(row: usize) {
    IRQ[row].took();
    for &line in lines(row) {
        if pio::level(line) {
            pio::set_masked(line, true);
        }
    }
    WATCHES[row].post_in_place();
}

pub fn watch(row: usize) -> &'static IrqWatch {
    &WATCHES[row]
}

/// The function a filled row raises `wire` with too, if one does: a second
/// row on one line would take the other's interrupts.
pub fn line_holder(wire: pio::Wire) -> Option<&'static str> {
    (0..MAX_ROWS).filter_map(function).find(|f| f.wires.iter().any(|&w| pio::same(w, wire))).map(|f| f.name)
}
