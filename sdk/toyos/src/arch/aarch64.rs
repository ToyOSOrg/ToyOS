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

/// `crate::timing`'s mode: `PSTATE.DIT`, through the `DIT` register's view
/// (`S3_3_C4_C2_5`, bit 24), which assembles without FEAT_DIT's name.
pub(crate) mod timing {
    use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

    /// Whether the machine has FEAT_DIT, once a thread has asked the kernel:
    /// `DIT` is an undefined register without it. A machine fact, so every
    /// copy of this crate in a process may keep its own.
    static SUPPORT: AtomicU8 = AtomicU8::new(UNKNOWN);
    const UNKNOWN: u8 = 0;
    const ABSENT: u8 = 1;
    const PRESENT: u8 = 2;

    const DIT: u64 = 1 << 24;

    fn supported() -> bool {
        match SUPPORT.load(Relaxed) {
            PRESENT => true,
            ABSENT => false,
            _ => {
                let present = toyos_abi::syscall::timing_control();
                SUPPORT.store(if present { PRESENT } else { ABSENT }, Relaxed);
                present
            }
        }
    }

    /// The thread's `PSTATE.DIT`, `None` without FEAT_DIT.
    #[inline]
    pub(crate) fn mode() -> Option<bool> {
        if !supported() {
            return None;
        }
        let value: u64;
        // SAFETY: `DIT` is readable at EL0 on a CPU with FEAT_DIT, which
        // `supported` established for every CPU.
        unsafe { core::arch::asm!("mrs {}, s3_3_c4_c2_5", out(reg) value, options(nomem, nostack, preserves_flags)) };
        Some(value & DIT != 0)
    }

    /// Set the thread's `PSTATE.DIT`. Called only once [`mode`] answered one.
    /// Not `nomem`, so that the compiler moves no memory access of the work
    /// it brackets across it.
    #[inline]
    pub(crate) fn set_mode(on: bool) {
        // SAFETY: as `mode`'s, which found FEAT_DIT before any call here; the
        // write changes `PSTATE.DIT` alone.
        unsafe { core::arch::asm!("msr s3_3_c4_c2_5, {}", in(reg) if on { DIT } else { 0 }, options(nostack, preserves_flags)) };
    }
}
