//! The kernel's own symbols: [`SymbolTable`] is read from the fault handler,
//! panic handler and double fault, so it never allocates, locks or does I/O.
//! `toyos-symbols` holds the pure byte-level decisions; this module holds only
//! what must run in that context, plus the boot-time global and `log!`
//! integration. The kernel names no program's frame: a user frame is recorded
//! as file and offset (`process::record_user_frame`).

use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use alloc::boxed::Box;
use toyos_elf::sym::SymTab;
use toyos_symbols::demangled;

/// Zero-allocation symbol table: raw pointers into the kernel image's ELF sections, safe to call from any context including panic/double-fault.
pub struct SymbolTable {
    symtab: *const u8,
    symtab_len: usize,
    strtab: *const u8,
    strtab_len: usize,
    base: u64,
}

// SAFETY: the pointers name only the kernel image in the direct map, mapped
// for the life of the machine; `SymbolTable` is `!Send` only because
// `*const u8` is.
unsafe impl Send for SymbolTable {}
// SAFETY: no method writes through either pointer, so concurrent reads from
// multiple CPUs alias only immutable bytes.
unsafe impl Sync for SymbolTable {}

impl SymbolTable {
    // A failed `locate` returns an empty table rather than an error: a kernel that cannot find its symbols still boots.
    fn from_elf(data: &[u8], base: u64) -> Self {
        let (symtab, strtab) = toyos_symbols::locate(data).unwrap_or((&[], &[]));
        Self {
            symtab: symtab.as_ptr(),
            symtab_len: symtab.len(),
            strtab: strtab.as_ptr(),
            strtab_len: strtab.len(),
            base,
        }
    }

    fn tables(&self) -> SymTab<'_> {
        // SAFETY: both ranges are slices `locate` borrowed out of the kernel
        // image in the direct map, mapped for the life of the machine, or the
        // dangling-but-aligned pointer of an empty slice with length 0.
        unsafe {
            SymTab::new(
                core::slice::from_raw_parts(self.symtab, self.symtab_len),
                core::slice::from_raw_parts(self.strtab, self.strtab_len),
            )
        }
    }

    /// Resolve an address to (mangled name, offset); no allocation, lock or panic.
    pub fn resolve(&self, addr: u64) -> Option<(&str, u64)> {
        self.tables().resolve(addr.checked_sub(self.base)?)
    }

    /// [`resolve`](Self::resolve) for a return address: steps back one byte
    /// first, since a return address can land one past the callee's last byte.
    pub fn resolve_return(&self, return_addr: u64) -> Option<(&str, u64)> {
        let (name, offset) = self.resolve(return_addr.saturating_sub(1))?;
        Some((name, offset + 1))
    }
}

// Kernel symbols — set once at boot, lock-free reads forever after.
static KERNEL_SYMS: AtomicPtr<SymbolTable> = AtomicPtr::new(core::ptr::null_mut());
static KERNEL_BASE: AtomicU64 = AtomicU64::new(0);

/// Set the kernel base address for crash diagnostics.
pub fn set_kernel_base(base: u64) {
    KERNEL_BASE.store(base, Ordering::Release);
}

/// Load kernel symbols from raw ELF bytes in the direct map; called once at boot.
pub fn load_kernel(data: &[u8], base: u64) {
    let table = SymbolTable::from_elf(data, base);
    let count = table.tables().count();
    KERNEL_SYMS.store(Box::into_raw(Box::new(table)), Ordering::Release);
    log!("symbols: loaded {} kernel symbols", count);
}

/// Resolve and log an address against kernel symbols; safe from any context
/// including panic, double fault, NMI.
pub fn resolve_kernel(addr: u64) -> Option<u64> {
    log_kernel(addr, |table| table.resolve(addr))
}

/// [`resolve_kernel`] for a backtrace frame's return address — see [`SymbolTable::resolve_return`].
pub fn resolve_kernel_return(return_addr: u64) -> Option<u64> {
    log_kernel(return_addr, |table| table.resolve_return(return_addr))
}

/// The kernel symbol `addr` falls inside, and how far in, without saying a word.
///
/// [`resolve_kernel`] writes its answer as a log record, which is the one thing
/// an NMI handler may not do: the interrupted context may be mid-publish of its
/// own. This is the same lookup with the record left to the caller, and it is
/// what the hard-lockup report writes into the black box.
pub fn kernel_symbol(addr: u64) -> Option<(&'static str, u64)> {
    let ptr = KERNEL_SYMS.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the same as `log_kernel`'s — `KERNEL_SYMS` is written once, from
    // a `Box::into_raw` that is never reclaimed, so a non-null pointer names a
    // `SymbolTable` that lives as long as the machine does.
    let table: &'static SymbolTable = unsafe { &*ptr };
    table.resolve(addr)
}

fn log_kernel(addr: u64, lookup: impl FnOnce(&SymbolTable) -> Option<(&str, u64)>) -> Option<u64> {
    let ptr = KERNEL_SYMS.load(Ordering::Acquire);
    if ptr.is_null() {
        log!("    {:#x}", addr);
        return None;
    }
    // SAFETY: `KERNEL_SYMS` is written exactly once, in `load_kernel`, via a
    // `Box::into_raw` that is never reclaimed, paired with the `Acquire`
    // above, so a non-null pointer here names a fully constructed
    // `SymbolTable` that stays valid for the rest of boot.
    let table = unsafe { &*ptr };
    if let Some((raw, offset)) = lookup(table) {
        log!("    {:#x}  {}+{:#x}", addr, demangled(raw), offset);
        Some(offset)
    } else {
        let kb = KERNEL_BASE.load(Ordering::Relaxed);
        if kb != 0 && addr >= kb {
            log!("    {:#x}  [kernel+{:#x}]", addr, addr - kb);
        } else {
            log!("    {:#x}", addr);
        }
        None
    }
}

/// Walk the frame-pointer chain from `start_fp`, resolving each return
/// address. Every architecture this kernel builds for lays a frame record out
/// the same way under `-Cforce-frame-pointers=yes`: the caller's frame pointer,
/// then the return address one word up.
pub(crate) fn kernel_backtrace(start_fp: u64, max_frames: usize) {
    let mut fp = start_fp;
    for _ in 0..max_frames {
        if fp == 0 || !fp.is_multiple_of(8) || !crate::mm::is_kernel_addr(fp) { break; }
        // SAFETY: `fp` is checked non-zero, 8-aligned and a kernel address, so
        // both reads land in the direct map, mapped for the life of the machine.
        //
        // Not `read_volatile` like the double-fault path's reads of memory
        // another CPU may still be writing: this walks the faulting thread's
        // own frame chain from its handler.
        let saved_fp = unsafe { *(fp as *const u64) };
        // SAFETY: same as above, for the return address one word up.
        let return_addr = unsafe { *((fp + 8) as *const u64) };
        if return_addr == 0 || !crate::mm::is_kernel_addr(return_addr) { break; }
        resolve_kernel_return(return_addr);
        fp = saved_fp;
    }
}
