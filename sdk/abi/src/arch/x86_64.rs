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

/// The counter the clock page's two words describe: the time-stamp counter,
/// read after every earlier load has completed, so a stamp taken after reading
/// another writer's record cannot be older than that record's.
#[inline]
pub fn counter() -> u64 {
    // SAFETY: `lfence` has no operands; `rdtsc` is unprivileged while CR4.TSD
    // is clear, which the kernel's control-register declaration makes true on
    // every CPU.
    unsafe {
        core::arch::x86_64::_mm_lfence();
        core::arch::x86_64::_rdtsc()
    }
}

/// Whether the processor is Intel's, by CPUID leaf 0's vendor string,
/// `GenuineIntel` in EBX, EDX and ECX (Intel SDM Vol. 2A, "CPUID"): the
/// vendor whose interface a processor object's `_OSC` speaks under Intel's
/// UUID (Intel 302223-007 §2.2).
pub fn processor_is_intel() -> bool {
    let (ebx, ecx, edx): (u64, u32, u32);
    // SAFETY: CPUID is unprivileged, and the kernel enables no CPUID
    // faulting; RBX, which LLVM reserves, is put back as it was.
    unsafe {
        core::arch::asm!(
            "mov {rbx}, rbx",
            "cpuid",
            "xchg {rbx}, rbx",
            rbx = out(reg) ebx,
            inout("eax") 0u32 => _,
            inout("ecx") 0u32 => ecx,
            out("edx") edx,
            options(nomem, nostack, preserves_flags),
        );
    }
    [ebx as u32, edx, ecx] == [*b"Genu", *b"ineI", *b"ntel"].map(u32::from_le_bytes)
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
