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
//! an interrupt-remapping entry is never given back. Its ISR counts into the
//! record a claimed PCI function's does, read back the same way.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use toyos_abi::pci::DeviceIrqRecord;
use toyos_abi::syscall::IsaId;

use crate::arch::pio::{self, GRANTABLE};
use crate::device::ClaimError;
use crate::pcidev::record::Interrupt;
use crate::process::Pid;
use crate::sync::Lock;
use crate::watch::Watch;

/// One function a process may be handed whole.
pub struct Grantable {
    /// What the log calls it.
    pub name: &'static str,
    /// Ascending, as [`IsaId`] spells them.
    pub ports: &'static [u16],
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
static WATCHES: [Watch; MAX_ROWS] = [const { Watch::new() }; MAX_ROWS];

/// Mint the claim on the row `set` names, lines routed and unmasked.
pub fn claim(set: IsaId) -> Result<usize, ClaimError> {
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::isa_claim_straddles_quarantine() {
        let begun = straddle::begin();
        let answer = mint(set);
        straddle::answered(begun);
        if answer.is_ok() {
            crate::arch::keyboard_controller::raise_flood();
        }
        return answer;
    }
    mint(set)
}

fn mint(set: IsaId) -> Result<usize, ClaimError> {
    let row = GRANTABLE
        .iter()
        .position(|g| {
            set.ports().eq(g.ports.iter().copied()) && set.irqs().eq(g.irqs.iter().copied())
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
    IRQ[row].clear();
    for &line in lines.iter() {
        pio::set_masked(line, false);
    }
    state.minted = true;
    Ok(row)
}

/// The claim's last handle went: its lines masked, its record emptied. The
/// ports stay with the process that bound them until that process ends.
pub fn release(row: usize) {
    let mut state = ROWS[row].lock();
    if let Some(Ok(lines)) = &state.lines {
        for &line in lines {
            pio::set_masked(line, true);
        }
    }
    IRQ[row].clear();
    state.minted = false;
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
    IRQ[row].take().map(|count| DeviceIrqRecord { count })
}

pub fn has_irq(row: usize) -> bool {
    IRQ[row].armed()
}

/// Called from the row's ISR: no lock, no allocation.
pub fn isr(row: usize) {
    IRQ[row].took();
}

/// Turn every interrupt taken since the last pass into a wake.
pub fn drain_pending() {
    for (row, irq) in IRQ.iter().enumerate().take(GRANTABLE.len()) {
        if !irq.take_pending() {
            continue;
        }
        if irq.take_unannounced() {
            log!("isa: {} took its first interrupt", GRANTABLE[row].name);
        }
        WATCHES[row].post();
    }
}

pub fn watch(row: usize) -> &'static Watch {
    &WATCHES[row]
}

/// The `isa-claim-straddles-quarantine` actuator: a claim answered between the
/// two steps of the i8042's quarantine, which holds after its first until one
/// begun after it has been; a granted one then raises the flood again.
#[cfg(feature = "boot-actuators")]
pub mod straddle {
    use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};

    /// Claims begun this boot.
    static BEGUN: AtomicU64 = AtomicU64::new(0);
    /// [`BEGUN`] when the quarantine's first step ran; [`NONE`] before it.
    static HELD_FROM: AtomicU64 = AtomicU64::new(NONE);
    const NONE: u64 = u64::MAX;
    /// The lines the first step masked, for the second's log.
    static MASKED: AtomicU32 = AtomicU32::new(0);
    /// `WAITING` → `STRADDLED` → `RESUMED`, and nothing moves it back: the
    /// second step runs once.
    static STEP: AtomicU8 = AtomicU8::new(WAITING);
    const WAITING: u8 = 0;
    const STRADDLED: u8 = 1;
    const RESUMED: u8 = 2;

    pub(super) fn begin() -> u64 {
        BEGUN.fetch_add(1, Ordering::SeqCst)
    }

    /// A claim that began before the first step read the controller as driven
    /// either way, so only a later one decides anything.
    pub(super) fn answered(begun: u64) {
        let from = HELD_FROM.load(Ordering::SeqCst);
        if from != NONE
            && begun >= from
            && STEP.compare_exchange(WAITING, STRADDLED, Ordering::SeqCst, Ordering::SeqCst).is_ok()
        {
            crate::arch::keyboard_controller::wake_irq_cpu();
        }
    }

    /// The quarantine's first step ran and masked `masked` lines.
    pub fn hold(masked: u32) {
        MASKED.store(masked, Ordering::SeqCst);
        HELD_FROM.store(BEGUN.load(Ordering::SeqCst), Ordering::SeqCst);
        log!("isa: the i8042's quarantine holds after its first step for a claim");
    }

    /// The first step's masked count, once, after a claim begun after it has
    /// been answered; the second step is the caller's.
    pub fn resume() -> Option<u32> {
        STEP.compare_exchange(STRADDLED, RESUMED, Ordering::SeqCst, Ordering::SeqCst).ok()?;
        log!("isa: a claim was answered between the i8042's quarantine steps");
        Some(MASKED.load(Ordering::SeqCst))
    }
}
