//! In-guest half of the NVMe operation-budget gate: proves a caller with an already-expired deadline is refused before a command reaches the controller.

use alloc::vec;

use crate::page_cache;
use crate::scheduler::Operation;
use crate::time::Deadline;

/// Any block works: the refusal happens before the LBA reaches a command.
const AT: u64 = 0;

/// Reads with an expired deadline (refused), then re-reads the same block (must succeed).
pub fn run() {
    let mut buf = vec![0u8; 4096];

    // Deadline is set above the trait; `read_blocks` may only narrow it, never widen it.
    let refused = {
        let _op = Operation::begin(Deadline::passed());
        let mut guard = page_cache::lock();
        let (_cache, dev) = guard.cache_and_dev();
        dev.read_blocks(AT, 1, &mut buf)
    };
    // budget= distinguishes a spent budget from an abandoned controller; `may_issue` refuses both.
    log!("nvme-gate: read with a spent budget refused={} budget={}",
        refused.is_err(),
        refused == Err(crate::block::BlockError::BudgetExpired));

    // Second read proves the controller wasn't abandoned mid-command.
    let mut guard = page_cache::lock();
    let (_cache, dev) = guard.cache_and_dev();
    let served = dev.read_blocks(AT, 1, &mut buf).is_ok();
    drop(guard);
    log!("nvme-gate: the same block read afterwards ok={served}");
}

/// Reset-escalation half: an abandoned read forces the driver to reset instead of going permanently offline.
pub fn silent_command() {
    let mut buf = vec![0u8; 4096];
    let refused = {
        // Armed immediately before this read: the skipped wait must be this read's, not init's Identify.
        crate::drivers::nvme::silent_command::arm();
        let mut guard = page_cache::lock();
        let (_cache, dev) = guard.cache_and_dev();
        dev.read_blocks(AT, 1, &mut buf)
    };
    log!("nvme-gate: the silent command's read refused={} budget={}",
        refused.is_err(),
        refused == Err(crate::block::BlockError::BudgetExpired));

    let mut guard = page_cache::lock();
    let (_cache, dev) = guard.cache_and_dev();
    let served = dev.read_blocks(AT, 1, &mut buf).is_ok();
    drop(guard);
    log!("nvme-gate: the same block read after the reset ok={served}");
}
