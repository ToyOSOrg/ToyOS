//! AArch64.

/// [`crate::shm::Bytes`]'s copy: 64 bytes a step through four vector
/// registers, then 8 and then 1, in assembly the compiler sees only as an
/// access to the memory it was handed.
///
/// # Safety
/// `src` is readable and `dst` writable for `len` bytes, and the two do not overlap.
pub(crate) unsafe fn copy(dst: *mut u8, src: *const u8, len: usize) {
    // SAFETY: the caller's; every access is within `len` bytes of each pointer.
    unsafe {
        core::arch::asm!(
            "2:",
            "cmp {len}, #64",
            "b.lo 3f",
            "ldp q0, q1, [{src}]",
            "ldp q2, q3, [{src}, #32]",
            "stp q0, q1, [{dst}]",
            "stp q2, q3, [{dst}, #32]",
            "add {src}, {src}, #64",
            "add {dst}, {dst}, #64",
            "sub {len}, {len}, #64",
            "b 2b",
            "3:",
            "cmp {len}, #8",
            "b.lo 4f",
            "ldr {word}, [{src}], #8",
            "str {word}, [{dst}], #8",
            "sub {len}, {len}, #8",
            "b 3b",
            "4:",
            "cbz {len}, 5f",
            "ldrb {word:w}, [{src}], #1",
            "strb {word:w}, [{dst}], #1",
            "sub {len}, {len}, #1",
            "b 4b",
            "5:",
            len = inout(reg) len => _,
            src = inout(reg) src => _,
            dst = inout(reg) dst => _,
            word = out(reg) _,
            out("v0") _,
            out("v1") _,
            out("v2") _,
            out("v3") _,
            options(nostack),
        );
    }
}

/// [`crate::shm::Bytes::zero`]'s fill: 16 bytes a step from the zero
/// register, then 1, as opaque as [`copy`].
///
/// # Safety
/// `dst` is writable for `len` bytes.
pub(crate) unsafe fn zero(dst: *mut u8, len: usize) {
    // SAFETY: the caller's; every store is within `len` bytes of `dst`.
    unsafe {
        core::arch::asm!(
            "2:",
            "cmp {len}, #16",
            "b.lo 3f",
            "stp xzr, xzr, [{dst}], #16",
            "sub {len}, {len}, #16",
            "b 2b",
            "3:",
            "cbz {len}, 4f",
            "strb wzr, [{dst}], #1",
            "sub {len}, {len}, #1",
            "b 3b",
            "4:",
            len = inout(reg) len => _,
            dst = inout(reg) dst => _,
            options(nostack),
        );
    }
}
