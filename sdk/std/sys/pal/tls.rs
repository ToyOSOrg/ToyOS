/// DTV-based TLS access for shared libraries.
///
/// Called by shared library code when accessing `#[thread_local]` variables of
/// a module whose TLS block is found through the dynamic thread vector (DTV).
///
/// DTV layout:
///   [0x00] generation: u64
///   [0x08] len: u64
///   [0x10] entries[0]: u64 (module_id=1)
///   [0x18] entries[1]: u64 (module_id=2)
///   ...
///
/// Entry value is the base address of that module's TLS block,
/// or DTV_UNALLOCATED (!0) if not yet allocated.
///
/// x86-64 (TLS variant II, GD/LD model): the linker preserves
/// `call __tls_get_addr` in .so files and emits R_X86_64_DTPMOD64/DTPOFF64 GOT
/// slot pairs. At load time, the kernel fills:
///   GOT[0] = module_id (DTV index, 1-based)
///   GOT[1] = offset within module's TLS segment
///
/// TCB layout (at fs_base / TP):
///   fs:[0x00] = self_ptr
///   fs:[0x08] = dtv_ptr
///
/// __tls_get_addr receives a pointer to TlsIndex {module_id, offset} in %rdi,
/// returns the address of the TLS variable in %rax.
///
/// AArch64 (TLS variant I). TCB layout (at TPIDR_EL0 / TP):
///   [TP+0x00] = dtv_ptr
///   [TP+0x08] = reserved
/// followed by the executable's TLS block at TP + align_up(16, p_align).
/// A shared library's thread-locals need a TLS descriptor resolver, which
/// AArch64 does not have yet; the loader refuses R_AARCH64_TLSDESC.

/// Slow path: the DTV entry is unallocated or out of range.
/// Calls SYS_TLS_ALLOC_BLOCK to allocate the TLS block on demand.
#[cfg(target_arch = "x86_64")]
#[inline(never)]
unsafe extern "C" fn __tls_get_addr_slow(module_id: u64, offset: u64) -> *mut u8 {
    // The caller's ABI is an address and there is nobody to return an error
    // to: a refusal added to `offset` is a pointer near the top of the address
    // space that the caller would then dereference.
    match toyos_abi::syscall::tls_alloc_block(module_id) {
        Ok(block) => core::ptr::without_provenance_mut((block + offset) as usize),
        Err(_) => rtabort!("no TLS block for a dlopen'd module"),
    }
}

/// Fast path: naked asm reads DTV directly from fs:[8], checks bounds and allocation,
/// falls through to slow path only when needed.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn __tls_get_addr(ti: *const [u64; 2]) -> *mut u8 {
    core::arch::naked_asm!(
        // ti is in %rdi: [module_id, offset]
        "mov rsi, [rdi + 8]",   // rsi = offset
        "mov rdi, [rdi]",       // rdi = module_id

        // module_id == 0 guard (shouldn't happen, but be safe)
        "test rdi, rdi",
        "jz 2f",

        // Load DTV pointer from TCB: fs:[8]
        "mov rax, fs:[8]",

        // Bounds check: module_id <= dtv[1] (len)
        "cmp rdi, [rax + 8]",
        "ja 2f",

        // Load DTV entry: dtv[2 + (module_id - 1)]
        "lea rcx, [rdi - 1]",
        "mov rax, [rax + rcx * 8 + 16]",

        // Check for DTV_UNALLOCATED (!0)
        "cmp rax, -1",
        "je 2f",

        // Fast path: return entry + offset
        "add rax, rsi",
        "ret",

        // Slow path
        "2:",
        "jmp {slow}",
        slow = sym __tls_get_addr_slow,
    );
}
