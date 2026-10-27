//! x86-64.

/// [`crate::shm::Bytes`]'s copy: one `rep movsb`, the CPU's own bulk copy, in
/// assembly the compiler sees only as an access to the memory it was handed.
///
/// # Safety
/// `src` is readable and `dst` writable for `len` bytes, and the two do not overlap.
pub(crate) unsafe fn copy(dst: *mut u8, src: *const u8, len: usize) {
    // SAFETY: the caller's. DF is clear on entry to an `asm!` block, so the copy
    // runs upward, and `rep movsb` leaves the flags as it found them.
    unsafe {
        core::arch::asm!(
            "rep movsb",
            inout("rcx") len => _,
            inout("rdi") dst => _,
            inout("rsi") src => _,
            options(nostack, preserves_flags),
        );
    }
}
