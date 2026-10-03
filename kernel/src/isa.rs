//! A legacy ISA function driven by a process: the I/O ports it decodes,
//! opened in the CPU's I/O permission bitmap, and the lines it raises,
//! answered as interrupt records on the claim.
//!
//! **What can be claimed is the architecture's [`GRANTABLE`] table, matched
//! exactly.** A port no row names is never opened, a set that is not a whole
//! row is refused rather than trimmed to one, and a row this kernel drives
//! itself is refused by name.
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
//! the holder reads the record back the same way.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use toyos_abi::pci::DeviceIrqRecord;
use toyos_abi::syscall::IsaId;

use crate::arch::pio::{self, GRANTABLE};
use crate::device::ClaimError;
use crate::pcidev::record::Interrupt;
use crate::process::Pid;
use crate::sync::Lock;
use crate::watch::IrqWatch;

/// One function a process may be handed whole.
pub struct Grantable {
    /// What the log calls it.
    pub name: &'static str,
    /// Ascending, as [`IsaId`] spells them, and each one the I/O permission
    /// bitmap names: a port past it is no `u8`.
    pub ports: &'static [u8],
    pub irqs: &'static [u8],
    /// Whether this kernel drives the function itself, which no claim shares.
    pub kernel_drives: fn() -> bool,
}

/// How many rows any architecture's table has; a static array per row below.
const MAX_ROWS: usize = 1;
const _: () = assert!(GRANTABLE.len() <= MAX_ROWS, "every grantable row needs its state");

/// No process holds the row's ports.
const NOBODY: u32 = Pid::MAX.0;

struct Row {
    /// A claim on the row exists.
    minted: bool,
    /// The row's lines, routed by its first claim and kept; `Err` is a line
    /// this machine could not route, which refuses every claim after it too.
    lines: Option<Result<Vec<pio::Line>, ()>>,
}

static ROWS: [Lock<Row>; MAX_ROWS] =
    [const { Lock::new(Row { minted: false, lines: None }) }; MAX_ROWS];

/// The pid whose threads the row's ports are open for, or [`NOBODY`]; read by
/// every context switch, which takes no lock.
static BOUND: [AtomicU32; MAX_ROWS] = [const { AtomicU32::new(NOBODY) }; MAX_ROWS];

static IRQ: [Interrupt; MAX_ROWS] = [const { Interrupt::new() }; MAX_ROWS];

/// What a claim's poll waits on, one per row.
static WATCHES: [IrqWatch; MAX_ROWS] = [const { IrqWatch::new() }; MAX_ROWS];

/// Mint the claim on the row `set` names, lines routed and unmasked.
pub fn claim(set: IsaId) -> Result<usize, ClaimError> {
    let row = GRANTABLE
        .iter()
        .position(|g| {
            set.ports().eq(g.ports.iter().map(|&port| u16::from(port)))
                && set.irqs().eq(g.irqs.iter().copied())
        })
        .ok_or(ClaimError::Absent)?;
    let grantable = &GRANTABLE[row];
    if (grantable.kernel_drives)() {
        return Err(ClaimError::KernelDriven);
    }
    let mut state = ROWS[row].lock();
    if state.minted || BOUND[row].load(Ordering::Acquire) != NOBODY {
        return Err(ClaimError::Owned);
    }
    let lines = state.lines.get_or_insert_with(|| {
        grantable
            .irqs
            .iter()
            .map(|&irq| {
                pio::route(row, irq).map_err(|why| {
                    log!("isa: {} line {irq} not routable: {why}", grantable.name);
                })
            })
            .collect()
    });
    let Ok(lines) = lines else { return Err(ClaimError::Unusable) };
    for &line in lines.iter() {
        pio::set_masked(line, false);
    }
    state.minted = true;
    Ok(row)
}

/// The claim's last handle went: its lines masked, its record emptied and its
/// polls answered. The ports stay with the process that bound them until that
/// process ends.
pub fn release(row: usize) {
    // Not held across the watch's answer; the row stays minted until that is
    // made, so no next claim's poll is among the ones answered.
    if let Some(Ok(lines)) = &ROWS[row].lock().lines {
        for &line in lines {
            pio::set_masked(line, true);
        }
    }
    IRQ[row].clear();
    // The claim is gone, so a poll on it is answered rather than left for the
    // next holder's interrupts.
    WATCHES[row].cancel_polls();
    ROWS[row].lock().minted = false;
}

/// Open the row's ports to `pid` for the rest of its life. Called once per
/// claim, by the claim's first read, and never twice for a row: [`claim`]
/// mints none while a process holds its ports.
pub fn bind(row: usize, pid: Pid) {
    match BOUND[row].compare_exchange(NOBODY, pid.raw(), Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => log!("isa: {}'s ports are pid {pid}'s", GRANTABLE[row].name),
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
    for (row, bound) in BOUND.iter().enumerate().take(GRANTABLE.len()) {
        if bound.compare_exchange(pid.raw(), NOBODY, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            log!("isa: {}'s ports went back with pid {pid}", GRANTABLE[row].name);
        }
    }
}

/// The interrupts since the last read, or `None` for none.
pub fn take_record(row: usize) -> Option<DeviceIrqRecord> {
    let taken = IRQ[row].take();
    if taken.is_some() && IRQ[row].take_unannounced() {
        log!("isa: {} took its first interrupt", GRANTABLE[row].name);
    }
    taken.map(|count| DeviceIrqRecord { count })
}

pub fn has_irq(row: usize) -> bool {
    IRQ[row].armed()
}

/// Records one interrupt and posts the claim's watch. Called from the row's
/// handler, so it takes no lock but the watch's own and allocates nothing.
pub fn isr(row: usize) {
    IRQ[row].took();
    WATCHES[row].post_in_place();
}

pub fn watch(row: usize) -> &'static IrqWatch {
    &WATCHES[row]
}
