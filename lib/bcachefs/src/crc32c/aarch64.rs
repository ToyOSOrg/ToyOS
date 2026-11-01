//! FEAT_CRC32's `CRC32CX` and `CRC32CB`, which accumulate CRC-32C (Arm ARM,
//! DDI 0487, "CRC32C"; mandatory from Armv8.1, ID_AA64ISAR0_EL1.CRC32).
//! Every AArch64 machine ToyOS supports has it, and so do the hosts that
//! build and test it.

use core::arch::aarch64::{__crc32cb, __crc32cd};

pub fn update(crc: u32, data: &[u8]) -> u32 {
    // SAFETY: the CPU has FEAT_CRC32 (the module header), which is all
    // `accumulate` asks of its caller.
    unsafe { accumulate(crc, data) }
}

/// # Safety
/// The CPU has FEAT_CRC32.
#[target_feature(enable = "crc")]
unsafe fn accumulate(mut crc: u32, data: &[u8]) -> u32 {
    let (words, tail) = data.as_chunks::<8>();
    for word in words {
        crc = __crc32cd(crc, u64::from_le_bytes(*word));
    }
    for &byte in tail {
        crc = __crc32cb(crc, byte);
    }
    crc
}
