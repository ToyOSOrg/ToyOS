//! `__tls_get_addr`, the general-dynamic TLS model's call: a shared object
//! reaches a module's thread-locals through a GOT pair `[module id, offset]`
//! its loader filled, and the module's block through the calling thread's DTV
//! (`toyos::tls`), which the TCB at `fs:8` names. AArch64 shared objects
//! address thread-locals through TLS descriptors, which the loader refuses.

#[cfg(target_arch = "x86_64")]
#[inline(never)]
unsafe extern "C" fn __tls_get_addr_slow(module: usize, offset: usize) -> *mut u8 {
    toyos::tls::block(module).wrapping_add(offset)
}

/// The fast path: the DTV's slot for the module, when it is in range and its
/// block is made; anything else is the slow path's.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn __tls_get_addr(ti: *const [usize; 2]) -> *mut u8 {
    core::arch::naked_asm!(
        "mov rsi, [rdi + 8]",
        "mov rdi, [rdi]",
        "test rdi, rdi",
        "jz 2f",
        "mov rax, fs:[8]",
        "cmp rdi, [rax + {capacity}]",
        "ja 2f",
        "mov rax, [rax + rdi * 8 + {slots} - 8]",
        "cmp rax, -1",
        "je 2f",
        "add rax, rsi",
        "ret",
        "2:",
        "jmp {slow}",
        capacity = const toyos::tls::DTV_CAPACITY,
        slots = const toyos::tls::DTV_SLOTS,
        slow = sym __tls_get_addr_slow,
    );
}
