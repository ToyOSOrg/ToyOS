//! `usb-storage-gate`: in-guest half of the USB mass-storage gate.
//!
//! Verifies blocks the host wrote and writes blocks the host can check, so
//! neither half of the driver certifies itself.
//!
//! Never writes to a disk without the block-0 stamp `bcachefs_adapter::probe`
//! requires before formatting `/home`.

use alloc::vec;

use crate::block::Handle;
use crate::drivers::usb_storage;

/// 16 bytes so the block count behind it stays 8-byte aligned.
const MAGIC: &[u8; 16] = b"TOYOS-USB-GATE1\0";
const AT_BLOCKS: usize = 16;
const AT_NONCE: usize = 24;

const BLOCK: usize = crate::mm::PAGE_SIZE as usize;

/// Blocks the host wrote and the guest must read back unchanged.
const HOST_BLOCKS: [i64; 2] = [1, -1];
/// Blocks the guest writes and the host must find afterwards.
const GUEST_BLOCKS: [i64; 2] = [2, -2];
/// Long enough to cross the driver's per-command batch of eight blocks.
const RUN_START: u64 = 4;
const RUN_LEN: u32 = 9;

/// Mirrored byte-for-byte by the harness; must not change independently.
fn pattern(nonce: u64, block: u64, i: usize) -> u8 {
    let n = (nonce >> ((i % 8) * 8)) as u8;
    let b = (block ^ (block >> 13) ^ (block >> 27)) as u8;
    n ^ b.wrapping_mul(37) ^ (i as u8).wrapping_mul(101)
}

fn fill(buf: &mut [u8], nonce: u64, block: u64) {
    for (i, byte) in buf.iter_mut().enumerate() {
        *byte = pattern(nonce, block, i);
    }
}

/// FNV-1a over a whole block, mirrored byte-for-byte by the harness.
///
/// [`first_bad`] is the guest's own comparator and nothing in the guest can
/// certify it; this is a number the harness recomputes from the image, so a
/// comparator that always agreed would be caught by the disagreement here.
fn digest(buf: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in buf {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Where a block does not match, or `None` if it does.
fn first_bad(buf: &[u8], nonce: u64, block: u64) -> Option<(usize, u8, u8)> {
    buf.iter().enumerate().find_map(|(i, &got)| {
        let want = pattern(nonce, block, i);
        (got != want).then_some((i, got, want))
    })
}

/// Resolve a possibly-negative block index against the disk's size.
fn at(blocks: u64, index: i64) -> u64 {
    if index >= 0 {
        index as u64
    } else {
        blocks.saturating_sub(index.unsigned_abs())
    }
}

pub fn run() {
    let disks = usb_storage::count();
    log!("usb-gate: {disks} disk(s) on the bus");
    for index in 0..disks {
        let Some((disk, _)) = usb_storage::handle(index) else { continue };
        check(index, &disk);
    }
    log!("usb-gate: sweep complete");
}

fn check(index: usize, disk: &Handle) {
    let read =|block: u64, count: u32, buf: &mut [u8]| disk.lock().read_blocks(block, count, buf);
    let write = |block: u64, count: u32, buf: &[u8]| disk.lock().write_blocks(block, count, buf);

    let blocks = disk.block_count();
    let mut head = vec![0u8; BLOCK];
    if read(0, 1, &mut head).is_err() {
        log!("usb-gate: disk {index} would not give up block 0");
        return;
    }
    if &head[..MAGIC.len()] != MAGIC {
        log!("usb-gate: disk {index} carries no stamp, leaving it alone");
        return;
    }
    // Refuse a stamp written for a different block count: offsets would mean
    // something else on this disk.
    let stamped = u64::from_le_bytes(head[AT_BLOCKS..AT_BLOCKS + 8].try_into().unwrap());
    if stamped != blocks {
        log!("usb-gate: disk {index} is stamped for {stamped} blocks and has {blocks}");
        return;
    }
    let nonce = u64::from_le_bytes(head[AT_NONCE..AT_NONCE + 8].try_into().unwrap());
    // +2 leaves room for the two blocks addressed from the end.
    if blocks < RUN_START + RUN_LEN as u64 + 2 {
        log!("usb-gate: disk {index} has only {blocks} blocks, too few to test");
        return;
    }
    log!("usb-gate: disk {index} designated, blocks={blocks} nonce={nonce:#018x}");

    let mut reads_ok = true;
    let mut buf = vec![0u8; BLOCK];
    for index in HOST_BLOCKS {
        let block = at(blocks, index);
        buf.fill(0);
        if read(block, 1, &mut buf).is_err() {
            reads_ok = false;
            log!("usb-gate: host block {block} could not be read");
            continue;
        }
        let hash = digest(&buf);
        match first_bad(&buf, nonce, block) {
            None => log!("usb-gate: host block {block} verified digest={hash:#018x}"),
            Some((i, got, want)) => {
                reads_ok = false;
                log!(
                    "usb-gate: host block {block} differs at byte {i}: {got:#04x} not {want:#04x} \
                     digest={hash:#018x}"
                );
            }
        }
    }

    // Checks the error channel: a refusal must be distinguishable from data,
    // not silently zero-filled.
    let past_end = read(blocks, 1, &mut buf).is_err();
    log!("usb-gate: read past the last block refused={past_end}");

    // No device can be staged slow enough to expire the deadline naturally, so
    // an already-passed one is established directly.
    //
    // This spent-budget read is positioned here rather than at the end
    // because every assertion that follows depends on the device being left
    // untouched by it.
    //
    // Goes straight to the driver, not through `BlockDevice`, so this call is
    // not captured under a read that was never issued.
    //
    // budget= separates `BudgetExpired` from `BlockError::Device`.
    let spent = {
        let _op = crate::scheduler::Operation::begin(crate::time::Deadline::passed());
        crate::drivers::xhci::storage_read(index, at(blocks, HOST_BLOCKS[0]), 1, &mut buf)
    };
    log!("usb-gate: read with a spent budget refused={} budget={}",
        spent.is_err(),
        spent == Err(crate::block::BlockError::BudgetExpired));

    // A read the controller cuts short while the device's CSW claims it moved
    // everything.
    //
    // arm_short_read() is armed immediately before this specific read, not
    // via a boot-wide fault count, so the short read lands on a known
    // transfer.
    //
    // The short-read probe is issued against a host-staged block (not a
    // guest-written one) so `matched` compares against bytes the guest could
    // not itself have produced.
    //
    // A caller is handed the wrong LBA's data when the two accounts disagree
    // and only the device's is kept.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::usb_short_read() {
        let block = at(blocks, HOST_BLOCKS[0]);
        buf.fill(0);
        crate::drivers::xhci::arm_short_read();
        let refused = read(block, 1, &mut buf).is_err();
        let matched = !refused && first_bad(&buf, nonce, block).is_none();
        log!("usb-gate: short read of block {block} refused={refused} matched={matched}");
    }

    // Keyed on the inverted nonce so a driver that returns the wrong block
    // cannot pass by returning data of the right kind.
    let guest_nonce = !nonce;
    let mut writes_ok = true;
    // Nonzero only when a write itself reported failure, distinct from a
    // readback mismatch.
    let mut write_errors = 0usize;
    for index in GUEST_BLOCKS {
        let block = at(blocks, index);
        fill(&mut buf, guest_nonce, block);
        if write(block, 1, &buf).is_err() {
            writes_ok = false;
            write_errors += 1;
            log!("usb-gate: block {block} refused the write");
        }
    }

    let mut run = vec![0u8; RUN_LEN as usize * BLOCK];
    for i in 0..RUN_LEN as u64 {
        let block = RUN_START + i;
        let at = i as usize * BLOCK;
        fill(&mut run[at..at + BLOCK], guest_nonce, block);
    }
    if write(RUN_START, RUN_LEN, &run).is_err() {
        writes_ok = false;
        write_errors += 1;
        log!("usb-gate: the {RUN_LEN}-block run refused the write");
    }
    if disk.lock().flush().is_err() {
        writes_ok = false;
        log!("usb-gate: the disk refused to flush");
    }

    let mut back = vec![0u8; RUN_LEN as usize * BLOCK];
    if read(RUN_START, RUN_LEN, &mut back).is_err() {
        writes_ok = false;
        log!("usb-gate: the {RUN_LEN}-block run could not be read back");
    }
    if writes_ok && back != run {
        writes_ok = false;
        let at = back.iter().zip(&run).position(|(a, b)| a != b).unwrap_or(0);
        log!("usb-gate: readback of the {RUN_LEN}-block run differs at byte {at}");
    }
    for index in GUEST_BLOCKS {
        let block = at(blocks, index);
        buf.fill(0);
        if read(block, 1, &mut buf).is_err() {
            writes_ok = false;
            log!("usb-gate: block {block} could not be read back");
            continue;
        }
        if let Some((i, got, want)) = first_bad(&buf, guest_nonce, block) {
            writes_ok = false;
            log!("usb-gate: readback of block {block} differs at byte {i}: {got:#04x} not {want:#04x}");
        }
    }

    log!(
        "usb-gate: disk done reads={} writes={} refusal={past_end} wr_err={write_errors} healthy={}",
        if reads_ok { "ok" } else { "bad" },
        if writes_ok { "ok" } else { "bad" },
        usb_storage::healthy(index)
    );
}

/// Blocks per read-and-write-back pair — eight of this driver's largest SCSI
/// command, so each pair is several commands and not one.
#[cfg(feature = "boot-actuators")]
const WEDGE_CHUNK: u32 = 64;

/// Pairs before the wedge, so the wedge is not the first thing this command's
/// device saw.
#[cfg(feature = "boot-actuators")]
const WEDGE_CHUNKS: u32 = 16;

/// Leave this machine wedged inside one Bulk-Only command, at the phase `at`
/// names, with megabytes of writes behind it.
///
/// **The stimulus for a state no ordinary boot reaches.** A shutdown reaches
/// its sync with nothing dirty on most boots, so the traffic and the command
/// the wedge is taken inside are both issued here rather than waited for.
///
/// **Every block is read first and written back byte for byte**, so the medium
/// is what it was however much of a write either reset completes; and at the
/// disk's own end, because a fixed offset is inside a partition this kernel
/// mounts on the smaller disks the same actuator boots on.
#[cfg(feature = "boot-actuators")]
pub fn wedge_inside_a_write(at: toyos_xhci::bot::Phase) {
    let Some((disk, _)) = usb_storage::handle(0) else {
        log!("usb-wedge: no USB disk on this machine, so there is no command to wedge inside");
        return;
    };
    let span = u64::from(WEDGE_CHUNK) * u64::from(WEDGE_CHUNKS);
    let Some(first) = disk.block_count().checked_sub(span) else {
        log!("usb-wedge: disk 0 holds {} blocks, fewer than the {span} this wedge writes",
            disk.block_count());
        return;
    };
    let mut buf = vec![0u8; WEDGE_CHUNK as usize * BLOCK];
    // The last pair carries the wedge, so every pair before it is traffic the
    // device has already taken.
    for chunk in 0..WEDGE_CHUNKS {
        let block = first + u64::from(chunk) * u64::from(WEDGE_CHUNK);
        if disk.lock().read_blocks(block, WEDGE_CHUNK, &mut buf).is_err() {
            log!("usb-wedge: disk 0 would not give up block {block}, so no write is staged from it");
            return;
        }
        if chunk + 1 == WEDGE_CHUNKS {
            log!("{USB_WEDGE_STAGED} {at} phase, after {} KiB rewritten with the bytes just read \
                 from it", u64::from(WEDGE_CHUNK) * u64::from(chunk) * BLOCK as u64 / 1024);
            crate::drivers::xhci::arm_mid_write_wedge(at);
        }
        if disk.lock().write_blocks(block, WEDGE_CHUNK, &buf).is_err() {
            log!("usb-wedge: disk 0 refused the write at block {block}");
            return;
        }
    }
    log!("{USB_WEDGE_MISSED} — every write completed, so no CPU was stopped inside one and this \
         boot ends itself the ordinary way");
}

/// What the wedge says before the write it is taken inside, and what it says if
/// that write ran to completion instead. Judged by the harness, so both are
/// constants (`src/bootlog.rs`).
#[cfg(feature = "boot-actuators")]
pub const USB_WEDGE_STAGED: &str = "usb-wedge: stopping every CPU at the";
#[cfg(feature = "boot-actuators")]
pub const USB_WEDGE_MISSED: &str = "usb-wedge: the write completed";

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
    crate::arch::apic::arm_within(toyos_sched::fair::QUANTUM_NS);
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
