//! Generates patterned records so the log gate's reader can check a conservation law over them.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::sched::kthread::{self, OnPanic};

// Exceeds a shard's capacity, so the drop path under test is reached at every `--smp` count.
const STORM_RECORDS: u64 = 1024;

// Must exceed one machine word: a single-store payload couldn't reveal a torn write.
const PAYLOAD: usize = 96;

/// Deterministic checksum of `thread` and `index`, embedded in a record's `k=` field.
pub fn checksum(thread: u64, index: u64) -> u64 {
    (thread.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ index.wrapping_mul(0xC2B2_AE3D_27D4_EB4F))
        .rotate_left(17)
}

/// One payload byte at `offset`, deterministic in `checksum`; always lowercase ASCII.
pub fn payload_byte(checksum: u64, offset: usize) -> u8 {
    b'a' + (checksum.wrapping_add(offset as u64) % 26) as u8
}

/// One patterned record for `thread`/`index`; also called by `log-nested-reserve` from an interrupt handler.
/// The reader regenerates this text independently from `t=`/`i=`, so the format here must stay in sync with it.
pub fn emit_patterned(thread: u64, index: u64) {
    let checksum = checksum(thread, index);
    let mut payload = [0u8; PAYLOAD];
    for (offset, byte) in payload.iter_mut().enumerate() {
        *byte = payload_byte(checksum, offset);
    }
    // Fallback rather than `expect`: a panic here would halt the machine over the producer's own formatting.
    let payload = core::str::from_utf8(&payload).unwrap_or("");
    crate::log!("logstorm t={thread} i={index} k={checksum:016x} {payload}");
}

static STARTED: AtomicBool = AtomicBool::new(false);

/// Spawns one storm thread per shard, once for the life of the machine.
/// Called from `SYS_LOG_READ`, which is what makes the storm concurrent with a reader by construction.
pub fn start_once() {
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let threads = super::shard_count();
    // The reader parses this line to learn the storm's shape.
    crate::log!("logstorm start threads={threads} records={STORM_RECORDS}");
    for thread in 0..threads {
        // `Halt`: a panicked storm thread invalidates the gate's conservation law, so continuing would answer over an incomplete storm.
        kthread::spawn("logstorm", body, thread as u64, OnPanic::Halt);
    }
}

extern "C" fn body(thread: u64) -> ! {
    for index in 0..STORM_RECORDS {
        emit_patterned(thread, index);
    }
    // The reader decides from its own cursor rather than waiting on this record: a barrier here was tried and hung at scale.
    crate::log!("logstorm done t={thread} emitted={STORM_RECORDS}");

    // Parks rather than exits: kthread rows are never removed, and spinning here would compete with the reader for the rest of the boot.
    crate::completion::park_forever();
}
