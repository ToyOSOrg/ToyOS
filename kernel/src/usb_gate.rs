//! `usb-reset-under-load`: the stick written continuously, so a reset lands on
//! a controller that is moving bytes.

use alloc::vec;

use crate::block::BlockDevice;
use crate::drivers::usb_storage;

const BLOCK: usize = crate::mm::PAGE_SIZE as usize;

/// Blocks per read-and-write-back pair — eight of this driver's largest SCSI
/// command, so each pair is several commands and not one.
#[cfg(feature = "boot-actuators")]
const WEDGE_CHUNK: u32 = 64;

/// The smallest disk this load will sweep: a gibibyte in 4 KiB blocks.
///
/// **A refusal and not a smaller sweep.** The sweep takes the last eighth of
/// the disk, which on anything smaller is inside a partition this kernel
/// mounts; a disk with no room for it is one this control cannot be staged on,
/// and saying so is the answer.
#[cfg(feature = "boot-actuators")]
const SWEEP_FLOOR: u64 = 262_144;

/// Stream writes to the stick until something else ends the machine, so the
/// reset lands on a controller that is moving bytes and a device that is
/// programming flash.
///
/// **The last eighth once and never twice.** Every run is read first and
/// written back byte for byte, so the medium is what it was however much of a
/// run either reset completes; and the sweep stops at the end of that span
/// rather than wrapping, so no block on the owner's stick is programmed twice
/// in a boot. A sweep that reaches the end says so by name, because a bus that
/// went idle before the reset is the idle case again under this arm's name.
#[cfg(feature = "boot-actuators")]
pub fn sweep_under_load() {
    let Some((disk, _)) = usb_storage::handle(0) else {
        log!("{LOAD_REFUSED}: no USB disk on this machine");
        return;
    };
    let blocks = disk.block_count();
    if blocks < SWEEP_FLOOR {
        log!("{LOAD_REFUSED}: disk 0 holds {blocks} blocks and this load sweeps the last \
             eighth of a disk of at least {SWEEP_FLOOR}");
        return;
    }
    let first = blocks - blocks / 8;
    log!("{LOAD_RUNNING} from block {first} to {blocks}, rewriting each run with the bytes \
         just read from it, until this machine is reset out from under it");
    // `IF` on and preemption off: the bound that has to end this machine is the
    // boot deadline, polled from the timer entry, and a CPU that takes no
    // interrupt at all is a hard lockup ended half a bound earlier by a
    // different mechanism under this arm's name.
    //
    // Read before the `sti`, which is the one fact here about the caller rather
    // than about this function.
    let interrupts_were_on = crate::arch::cpu::interrupts_enabled();
    crate::preempt::disable();
    crate::arch::irqchip::arm_within(kernel::sched::fair::QUANTUM_NS);
    crate::arch::cpu::enable_interrupts();
    let mut buf = vec![0u8; WEDGE_CHUNK as usize * BLOCK];
    let mut at = first;
    let mut stopped = false;
    while at + u64::from(WEDGE_CHUNK) <= blocks {
        if disk.lock().read_blocks(at, WEDGE_CHUNK, &mut buf).is_err()
            || disk.lock().write_blocks(at, WEDGE_CHUNK, &buf).is_err()
        {
            log!("{LOAD_STOPPED} at block {at}");
            stopped = true;
            break;
        }
        at += u64::from(WEDGE_CHUNK);
    }
    if !stopped {
        log!("{LOAD_SWEPT} at block {at}, so the bus is idle for the rest of this boot");
    }
    // Put back on the one path out of here, because the caller goes on to drain
    // write-back, sync every filesystem, flush every disk and wait for the log
    // to be durable, and none of that may run under a preempt count or an `IF`
    // this left behind. Not `preempt::enable`: the request stays set and the
    // caller's own next preemption point serves it, rather than a scheduler pass
    // taken from inside the shutdown syscall.
    if !interrupts_were_on {
        crate::arch::cpu::disable_interrupts();
    }
    crate::preempt::enable_no_resched();
}

/// What the load says when it starts, when it cannot, when the disk stopped
/// answering it, and when it reached the end of its one pass. Judged by the
/// harness, so all four are constants (`src/bootlog.rs`).
#[cfg(feature = "boot-actuators")]
pub const LOAD_RUNNING: &str = "usb-load: sweeping disk 0";
#[cfg(feature = "boot-actuators")]
pub const LOAD_REFUSED: &str = "usb-load: refused";
#[cfg(feature = "boot-actuators")]
pub const LOAD_STOPPED: &str = "usb-load: the disk stopped answering";
#[cfg(feature = "boot-actuators")]
pub const LOAD_SWEPT: &str = "usb-load: the sweep reached the end of the disk";
