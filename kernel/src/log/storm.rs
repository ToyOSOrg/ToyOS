//! Generates patterned records so the log gate's reader can check a conservation law over them.

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

/// One patterned record for `thread`/`index`; called by `SYS_DEBUG`'s `LOG_PATTERNED` and by `log-nested-reserve` from an interrupt handler.
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
