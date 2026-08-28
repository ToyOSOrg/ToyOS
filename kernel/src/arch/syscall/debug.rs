//! `SYS_DEBUG` support, entirely behind `test-actuators` — a shipping kernel builds without this module.
//!
//! The actions stay in `super::dispatch`'s match because four of them `return` or don't return at all, which a function call can't do.

use toyos_abi::syscall::SyscallError;

/// `SYS_DEBUG` action 2's lock only — once taken it is never released.
pub(super) static LOCK_ACROSS_SWITCH: crate::sync::Lock<()> = crate::sync::Lock::new(());

/// Flips false after action 2's one trip, refusing a second call into the lock that never releases.
pub(super) static LOCK_ACROSS_SWITCH_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(true);

/// Screen-test sync signal — `halt_all_cpus` paints before it flushes serial.
pub(super) const FATAL_HALT_NONCE: &str = "SYS_DEBUG: fatal halt 4b1d9e2c";

/// One heap allocation of `bytes` at `align` — raw `alloc`/`dealloc` because a dropped `Vec` is a malloc/free pair LLVM may delete.
pub(super) fn debug_heap_alloc(bytes: usize, align: usize) -> u64 {
    let Ok(layout) = core::alloc::Layout::from_size_align(bytes, align) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    // SAFETY: `layout` came from `Layout::from_size_align`, which refused zero size and non-power-of-two alignment above.
    let p = unsafe { alloc::alloc::alloc(layout) };
    // The null return is reported rather than unwrapped so a refusal is distinguishable from success in userland.
    if p.is_null() {
        return SyscallError::ResourceExhausted.to_u64();
    }
    // SAFETY: `p` is non-null and points to a live allocation of at least one byte, checked above.
    // The write is volatile for the same reason the alloc/dealloc pair is raw: an unobserved write is otherwise free for LLVM to delete.
    unsafe { core::ptr::write_volatile(p, 1u8) };
    // SAFETY: `p` came from `alloc` with this exact `layout` and has not been freed.
    unsafe { alloc::alloc::dealloc(p, layout) };
    0
}

/// Sixteen bytes of kernel memory a test can name and check for an unauthorized write.
pub(super) mod canary {
    use core::sync::atomic::{AtomicU64, Ordering};

    const VALUE: [u64; 2] = [0x_C0DE_1A55_0F17_1E55, 0x0005_EE7A_110F_1700];

    static WORDS: [AtomicU64; 2] =
        [AtomicU64::new(VALUE[0]), AtomicU64::new(VALUE[1])];

    /// An address in the direct map, the half `AddressSpace::translate` must refuse.
    pub fn address() -> u64 {
        WORDS.as_ptr() as u64
    }

    pub fn changed() -> bool {
        [WORDS[0].load(Ordering::Relaxed), WORDS[1].load(Ordering::Relaxed)] != VALUE
    }
}
