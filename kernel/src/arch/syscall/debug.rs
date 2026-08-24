//! `SYS_DEBUG`'s state: the locks, the latches and the memory a test can name.
//!
//! **The whole of `SYS_DEBUG` is behind `test-actuators` or it is nowhere**, and
//! this module is the half that is not an arm of the dispatch. A shipping kernel
//! is built without the feature, the module does not exist, the number falls to
//! the dispatch's default and answers what an unassigned number answers — so
//! there is nothing for a process to reach and nothing for it to discover. The
//! actions themselves stay in `super::dispatch`'s match, because four of them
//! cost the caller its process or the machine its CPUs by `return`ing or not
//! returning at all, and an arm that does that cannot be a function call.

use toyos_abi::syscall::SyscallError;

/// `SYS_DEBUG` action 2's lock, and nothing else's.
///
/// Action 2 takes it and then calls a switching scheduler entry — the shape
/// spec §6.4's tripwire exists to refuse. The assert fires while the guard is
/// still alive, so the guard never drops and this lock stays held for the rest
/// of the boot; that is why it is private to the one deliberate-panic action
/// and shared with nothing.
pub(super) static LOCK_ACROSS_SWITCH: crate::sync::Lock<()> = crate::sync::Lock::new(());

/// One trip per boot, because the lock above is never released.
///
/// On a kernel that carries `SYS_DEBUG` at all, without this a process could
/// call action 2 a second time and spin `Lock::lock`'s full 500M iterations on a
/// lock nothing will ever hand over — with IF=0 (`MSR_FMASK` masks it on syscall
/// entry) and preemption disabled, so on a single-CPU machine the timer, the log
/// drains and every other thread are frozen for that whole window. Refusing the
/// second call keeps the tripwire testable and the stall unreachable.
pub(super) static LOCK_ACROSS_SWITCH_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(true);

/// The last line `SYS_DEBUG` action 3 puts in the log ring before halting.
///
/// Actions 0 and 1 both satisfy the panic handler's recovery predicate and
/// return to userland by design, so neither can exercise the fatal funnel.
/// Action 3 reaches `halt_all_cpus` directly, which is where the on-screen
/// panic console paints.
///
/// The string is the whole synchronisation mechanism for the screen test:
/// `halt_all_cpus` renders *before* it flushes serial, so a host that has
/// seen this line knows the paint already finished — no sleep, no polling.
pub(super) const FATAL_HALT_NONCE: &str = "SYS_DEBUG: fatal halt 4b1d9e2c";

/// One kernel heap allocation of `bytes` at `align`, taken and released.
/// `SYS_DEBUG` actions 5, 6 and 7 are its only callers.
///
/// Raw `alloc`/`dealloc` rather than a `Vec` that is immediately dropped:
/// LLVM is allowed to delete a malloc/free pair whose result is never
/// observed, and an actuator the optimiser can remove certifies nothing. The
/// null return is reported rather than unwrapped for the same reason — a
/// refusal and a success have to be distinguishable from userland.
pub(super) fn debug_heap_alloc(bytes: usize, align: usize) -> u64 {
    let Ok(layout) = core::alloc::Layout::from_size_align(bytes, align) else {
        return SyscallError::InvalidArgument.to_u64();
    };
    // SAFETY: `layout` came from `Layout::from_size_align`, which refused a
    // zero size or a non-power-of-two alignment on the line above, and that is
    // `alloc`'s whole contract. **Irreducible on purpose**: the doc comment
    // above is the argument — a `Vec` dropped immediately is a malloc/free pair
    // whose result nothing observes, which LLVM may delete, and an actuator the
    // optimiser can remove certifies nothing about the allocator.
    let p = unsafe { alloc::alloc::alloc(layout) };
    if p.is_null() {
        return SyscallError::ResourceExhausted.to_u64();
    }
    // SAFETY: `p` is a live, non-null allocation of at least one byte, asserted
    // by the null check above. Volatile for the same reason the raw pair is raw.
    unsafe { core::ptr::write_volatile(p, 1u8) };
    // SAFETY: `p` came from `alloc` with this exact `layout` and has not been
    // freed, which is `dealloc`'s contract.
    unsafe { alloc::alloc::dealloc(p, layout) };
    0
}

/// Sixteen bytes of kernel memory a test can name, and ask about afterwards.
///
/// A guest cannot read the kernel's address space, so a write that lands there
/// is invisible to every assertion a test can make from userland — which is
/// exactly the write `SYS_DLOPEN`'s `init_out` used to allow, and a gate that
/// could only check the syscall's *verdict* would pass against a kernel that
/// still made it. Nothing here is faked: the address is this static's own, the
/// write a broken kernel makes is a real one, and what is read back is the
/// memory itself.
pub(super) mod canary {
    use core::sync::atomic::{AtomicU64, Ordering};

    const VALUE: [u64; 2] = [0x_C0DE_1A55_0F17_1E55, 0x0005_EE7A_110F_1700];

    static WORDS: [AtomicU64; 2] =
        [AtomicU64::new(VALUE[0]), AtomicU64::new(VALUE[1])];

    /// The direct map is where the kernel's own statics live, so this is an
    /// address in it — the half `AddressSpace::translate` must refuse.
    pub fn address() -> u64 {
        WORDS.as_ptr() as u64
    }

    pub fn changed() -> bool {
        [WORDS[0].load(Ordering::Relaxed), WORDS[1].load(Ordering::Relaxed)] != VALUE
    }
}
