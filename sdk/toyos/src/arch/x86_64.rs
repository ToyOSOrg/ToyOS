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

/// [`crate::shm::Bytes::zero`]'s fill: one `rep stosb`, as opaque as [`copy`].
///
/// # Safety
/// `dst` is writable for `len` bytes.
pub(crate) unsafe fn zero(dst: *mut u8, len: usize) {
    // SAFETY: the caller's; DF is clear on entry, as for `copy`.
    unsafe {
        core::arch::asm!(
            "rep stosb",
            inout("rcx") len => _,
            inout("rdi") dst => _,
            in("al") 0u8,
            options(nostack, preserves_flags),
        );
    }
}

/// `crate::timing`'s mode: none a thread sets on x86-64, so no [`Mode`]
/// exists.
pub(crate) mod timing {
    #[derive(Clone, Copy)]
    pub(crate) enum Mode {}

    #[inline(always)]
    pub(crate) fn mode() -> Option<Mode> {
        None
    }

    impl Mode {
        pub(crate) fn is_on(self) -> bool {
            match self {}
        }

        pub(crate) fn set(self, _on: bool) {
            match self {}
        }
    }
}

/// The thread control block the thread pointer (`FS.base`) addresses, psABI
/// TLS variant II: `TP + 0` the self-pointer, `TP + 8` the DTV, then this
/// SDK's words; the executable's TLS data lies below TP.
pub(crate) mod tcb {
    pub(crate) const VARIANT: crate::tls::Variant = crate::tls::Variant::II;
    pub(crate) const BYTES: usize = 64;
    pub(crate) const DTV: usize = 8;
    pub(crate) const TID: usize = 16;

    /// The calling thread's thread pointer, read off its self-pointer.
    #[inline]
    pub(crate) fn thread_pointer() -> usize {
        let tp: usize;
        // SAFETY: `fs:0` is the TCB's self-pointer, which every thread's
        // startup writes before it names its thread pointer.
        unsafe { core::arch::asm!("mov {}, fs:[0]", out(reg) tp, options(nostack, readonly, preserves_flags)) };
        tp
    }

    /// The calling thread's id, read off its TCB in one load.
    #[inline]
    pub(crate) fn current_tid() -> u32 {
        let tid: u32;
        // SAFETY: `TP + TID` lies inside the TCB every thread's startup lays
        // out before it names its thread pointer.
        unsafe {
            core::arch::asm!(
                "mov {tid:e}, dword ptr fs:[{at}]",
                tid = out(reg) tid,
                at = const TID,
                options(nostack, readonly, preserves_flags),
            );
        }
        tid
    }
}
