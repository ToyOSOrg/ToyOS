//! SSE4.2's `CRC32`, which accumulates CRC-32C (Intel SDM Vol. 2A, "CRC32 —
//! Accumulate CRC32 Value"; CPUID.01H:ECX.SSE4_2[bit 20]). Every x86-64 part
//! ToyOS supports, 2020 and later, has it, and so do the hosts that build and
//! test it.

use core::arch::x86_64::{_mm_crc32_u64, _mm_crc32_u8};

pub fn update(crc: u32, data: &[u8]) -> u32 {
    // SAFETY: the CPU has SSE4.2 (the module header), which is all
    // `accumulate` asks of its caller.
    unsafe { accumulate(crc, data) }
}

/// # Safety
/// The CPU has SSE4.2.
#[target_feature(enable = "sse4.2")]
unsafe fn accumulate(crc: u32, data: &[u8]) -> u32 {
    let (words, tail) = data.as_chunks::<8>();
    let mut wide = u64::from(crc);
    for word in words {
        wide = _mm_crc32_u64(wide, u64::from_le_bytes(*word));
    }
    // The instruction's 64-bit form leaves the register in the low half.
    let mut crc = wide as u32;
    for &byte in tail {
        crc = _mm_crc32_u8(crc, byte);
    }
    crc
}
