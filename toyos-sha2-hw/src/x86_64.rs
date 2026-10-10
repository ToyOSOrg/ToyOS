//! SHA-256's compression on the SHA extensions: `SHA256RNDS2` computes two
//! rounds over the working variables held as ABEF and CDGH, and `SHA256MSG1`
//! and `SHA256MSG2` the message schedule four words at a time (Intel SDM Vol.
//! 2B).
//!
//! **One `asm!` block saves every XMM register it uses and restores it before
//! it ends, and so declares none.** The loader's target is soft-float and has
//! no XMM register class to name a clobber in, and the UEFI calling convention
//! leaves XMM6–XMM15 the firmware's; every other target runs the same block.

use core::arch::asm;
use core::arch::x86_64::{__cpuid, __cpuid_count};

use toyos_sha2::{Compress256, K256};

/// `K256` at an address the block reads it from.
static K: [u32; 64] = K256;

/// `PSHUFB`'s control that reverses each 32-bit word's bytes: a block's
/// big-endian words into the lanes' little-endian ones.
static FLIP: [u8; 16] = [3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12];

/// The compression, where CPUID reports the SHA extensions and SSSE3, whose
/// `PSHUFB` and `PALIGNR` the block takes; nothing else hands out [`blocks`].
pub fn compress() -> Option<Compress256> {
    // SDM Vol. 2A, CPUID: leaf 7 sub-leaf 0's EBX bit 29 is SHA, leaf 1's ECX
    // bit 9 is SSSE3, and leaf 0's EAX is the highest leaf there is.
    let sha = __cpuid(0).eax >= 7 && __cpuid_count(7, 0).ebx & (1 << 29) != 0;
    let ssse3 = __cpuid(1).ecx & (1 << 9) != 0;
    (sha && ssse3).then_some(blocks as Compress256)
}

/// `blocks` into `state`, four rounds to a `rounds4` and the hash value held
/// in XMM1 and XMM2 across the run.
fn blocks(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
    // The loop below runs at least once.
    if blocks.is_empty() {
        return;
    }
    let mut saved = [0u8; 11 * 16];
    // SAFETY: `compress` hands this function out only where CPUID reports SHA
    // and SSSE3. The block reads `blocks` from its start to `end`, which is a
    // whole number of 64-byte blocks past it, reads `state`, `K` and `FLIP`,
    // writes `state` and `saved`, and leaves XMM0–XMM10 as it found them.
    unsafe {
        asm!(
            "movdqu %xmm0, 0({saved})",
            "movdqu %xmm1, 16({saved})",
            "movdqu %xmm2, 32({saved})",
            "movdqu %xmm3, 48({saved})",
            "movdqu %xmm4, 64({saved})",
            "movdqu %xmm5, 80({saved})",
            "movdqu %xmm6, 96({saved})",
            "movdqu %xmm7, 112({saved})",
            "movdqu %xmm8, 128({saved})",
            "movdqu %xmm9, 144({saved})",
            "movdqu %xmm10, 160({saved})",
            // `state` is DCBA and HGFE, low lane last; the instructions take
            // ABEF in XMM1 and CDGH in XMM2.
            "movdqu ({state}), %xmm1",
            "movdqu 16({state}), %xmm2",
            "movdqa %xmm1, %xmm7",
            "punpcklqdq %xmm2, %xmm1",
            "punpckhqdq %xmm7, %xmm2",
            "pshufd $0x1b, %xmm1, %xmm1",
            "pshufd $0xb1, %xmm2, %xmm2",
            "movdqu ({flip}), %xmm8",
            // `rounds4 i, m0, m1, m2, m3`: rounds `i` to `i + 3`, the
            // schedule's four words for them in `m0` and the twelve before in
            // `m1`–`m3`, oldest first. From round 4 `SHA256MSG1` starts the
            // words sixteen rounds on, and from round 12 `PALIGNR` and
            // `SHA256MSG2` finish the ones four rounds on, until the
            // schedule's last word.
            ".macro rounds4 i, m0, m1, m2, m3",
            ".if \\i < 16",
            "movdqu \\i*4({data}), \\m0",
            "pshufb %xmm8, \\m0",
            ".endif",
            "movdqu \\i*4({k}), %xmm0",
            "paddd \\m0, %xmm0",
            "sha256rnds2 %xmm1, %xmm2",
            ".if \\i >= 12 && \\i < 60",
            "movdqa \\m0, %xmm7",
            "palignr $4, \\m3, %xmm7",
            "paddd %xmm7, \\m1",
            "sha256msg2 \\m0, \\m1",
            ".endif",
            "punpckhqdq %xmm0, %xmm0",
            "sha256rnds2 %xmm2, %xmm1",
            ".if \\i >= 4 && \\i < 52",
            "sha256msg1 \\m0, \\m3",
            ".endif",
            ".endm",
            "2:",
            "movdqa %xmm1, %xmm9",
            "movdqa %xmm2, %xmm10",
            // The schedule's sixteen words live in XMM3–XMM6, four to a
            // register, each group of four rounds a rotation of them.
            "rounds4 0, %xmm3, %xmm4, %xmm5, %xmm6",
            "rounds4 4, %xmm4, %xmm5, %xmm6, %xmm3",
            "rounds4 8, %xmm5, %xmm6, %xmm3, %xmm4",
            "rounds4 12, %xmm6, %xmm3, %xmm4, %xmm5",
            "rounds4 16, %xmm3, %xmm4, %xmm5, %xmm6",
            "rounds4 20, %xmm4, %xmm5, %xmm6, %xmm3",
            "rounds4 24, %xmm5, %xmm6, %xmm3, %xmm4",
            "rounds4 28, %xmm6, %xmm3, %xmm4, %xmm5",
            "rounds4 32, %xmm3, %xmm4, %xmm5, %xmm6",
            "rounds4 36, %xmm4, %xmm5, %xmm6, %xmm3",
            "rounds4 40, %xmm5, %xmm6, %xmm3, %xmm4",
            "rounds4 44, %xmm6, %xmm3, %xmm4, %xmm5",
            "rounds4 48, %xmm3, %xmm4, %xmm5, %xmm6",
            "rounds4 52, %xmm4, %xmm5, %xmm6, %xmm3",
            "rounds4 56, %xmm5, %xmm6, %xmm3, %xmm4",
            "rounds4 60, %xmm6, %xmm3, %xmm4, %xmm5",
            "paddd %xmm9, %xmm1",
            "paddd %xmm10, %xmm2",
            "add $64, {data}",
            "cmp {end}, {data}",
            "jne 2b",
            ".purgem rounds4",
            "movdqa %xmm1, %xmm7",
            "punpcklqdq %xmm2, %xmm1",
            "punpckhqdq %xmm7, %xmm2",
            "pshufd $0xb1, %xmm1, %xmm1",
            "pshufd $0x1b, %xmm2, %xmm2",
            "movdqu %xmm2, ({state})",
            "movdqu %xmm1, 16({state})",
            "movdqu 0({saved}), %xmm0",
            "movdqu 16({saved}), %xmm1",
            "movdqu 32({saved}), %xmm2",
            "movdqu 48({saved}), %xmm3",
            "movdqu 64({saved}), %xmm4",
            "movdqu 80({saved}), %xmm5",
            "movdqu 96({saved}), %xmm6",
            "movdqu 112({saved}), %xmm7",
            "movdqu 128({saved}), %xmm8",
            "movdqu 144({saved}), %xmm9",
            "movdqu 160({saved}), %xmm10",
            state = in(reg) state.as_mut_ptr(),
            data = inout(reg) blocks.as_ptr() => _,
            end = in(reg) blocks.as_ptr_range().end,
            k = in(reg) K.as_ptr(),
            flip = in(reg) FLIP.as_ptr(),
            saved = in(reg) saved.as_mut_ptr(),
            options(att_syntax, nostack),
        );
    }
}
