use crate::{tcb, Tid};

/// Where a thread finds its own [`Tid`]: the kernel writes it into the thread
/// control block at `TP + TCB_TID` before the thread's first instruction. The
/// word is the thread's own memory, so what it says is the thread's word about
/// itself and nothing more.
pub const TCB_TID: usize = tcb::X86_64_TID;

/// The calling thread's id, read off its control block without a syscall.
#[inline]
pub fn current_tid() -> Tid {
    let tid: u32;
    // SAFETY: `fs` is this thread's TP, set by the kernel at the thread's
    // start, and `TP + TCB_TID` lies inside the 64-byte TCB the kernel
    // reserves there; a 4-byte load of it touches nothing else.
    unsafe {
        core::arch::asm!(
            "mov {tid:e}, dword ptr fs:[{at}]",
            tid = out(reg) tid,
            at = const TCB_TID,
            options(nostack, readonly, preserves_flags),
        );
    }
    Tid(tid)
}

/// The counter the clock page's line describes: the time-stamp counter,
/// read after every earlier load has completed, so a stamp taken after reading
/// another writer's record cannot be older than that record's, and before any
/// later instruction starts, so a load after it cannot have been answered
/// before it (SDM Vol. 2B, RDTSC: "If software requires RDTSC to be executed
/// prior to execution of any subsequent instruction (including any memory
/// accesses), it can execute the sequence LFENCE immediately after RDTSC").
#[inline]
pub fn counter() -> u64 {
    // SAFETY: `lfence` has no operands; `rdtsc` is unprivileged while CR4.TSD
    // is clear, which the kernel's control-register declaration makes true on
    // every CPU.
    unsafe {
        core::arch::x86_64::_mm_lfence();
        let now = core::arch::x86_64::_rdtsc();
        core::arch::x86_64::_mm_lfence();
        now
    }
}

/// The processor's CPUID identity.
pub fn processor_id() -> Option<crate::ProcessorId> {
    let leaf_0 = core::arch::x86_64::__cpuid(0);
    let vendor = core::array::from_fn(|i| [leaf_0.ebx, leaf_0.edx, leaf_0.ecx][i / 4].to_le_bytes()[i % 4]);
    Some(crate::ProcessorId { vendor, signature: core::arch::x86_64::__cpuid(1).eax })
}

/// `in` and `out` from Ring 3: a port this process holds no grant for, which
/// its CPU's I/O permission bitmap refuses, faults it.
pub mod ioport {
    pub fn in8(port: u16) -> u8 {
        let value: u8;
        // SAFETY: an `in` has no memory effect.
        unsafe { core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags)) };
        value
    }

    pub fn in16(port: u16) -> u16 {
        let value: u16;
        // SAFETY: as `in8`'s.
        unsafe { core::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags)) };
        value
    }

    /// What the device does with the byte is the caller's.
    pub fn out8(port: u16, value: u8) {
        // SAFETY: an `out` has no memory effect.
        unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)) };
    }

    pub fn out16(port: u16, value: u16) {
        // SAFETY: as `out8`'s.
        unsafe { core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags)) };
    }
}
