//! Raw-pointer half of a kernel backtrace frame: [`SymbolTable`] is read from
//! the fault handler, panic handler and double fault, so it never allocates,
//! locks or does I/O. `toyos-symbols` holds the pure byte-level decisions;
//! this module holds only what must run in that context, plus the boot-time
//! globals and `log!` integration.

use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use alloc::boxed::Box;
use toyos_elf::sym::SymTab;
use toyos_symbols::symbol_text;

use crate::process::PageAlloc;

/// Zero-allocation symbol table: raw pointers into ELF sections, safe to call from any context including panic/double-fault.
pub struct SymbolTable {
    /// `None` for kernel tables borrowed from the direct map; `Some` for owned process pages.
    pages: Option<PageAlloc>,
    symtab: *const u8,
    symtab_len: usize,
    strtab: *const u8,
    strtab_len: usize,
    base: u64,
    prog_base: u64,
    prog_end: u64,
    stack_base: u64,
    stack_end: u64,
}

// SAFETY: the pointers name only the static kernel image or pages this table
// owns and frees, so moving it carries the only reference to that memory;
// `SymbolTable` is `!Send` only because `*const u8` is.
unsafe impl Send for SymbolTable {}
// SAFETY: no method writes through either pointer, so concurrent reads from
// multiple CPUs alias only immutable bytes.
unsafe impl Sync for SymbolTable {}

impl SymbolTable {
    pub fn empty() -> Self {
        Self::empty_with_bounds(0, 0, 0, 0)
    }

    pub fn empty_with_bounds(
        prog_base: u64, prog_end: u64,
        stack_base: u64, stack_end: u64,
    ) -> Self {
        Self {
            pages: None,
            symtab: core::ptr::null(),
            symtab_len: 0,
            strtab: core::ptr::null(),
            strtab_len: 0,
            base: 0,
            prog_base, prog_end, stack_base, stack_end,
        }
    }

    /// A process's tables in pages it owns: `.symtab` (`symtab_len` bytes) followed immediately by `.strtab`.
    // Eight arguments: four are address-range bounds from the one caller that already holds them.
    #[allow(clippy::too_many_arguments)]
    pub fn from_pages(
        pages: PageAlloc,
        symtab_len: usize,
        strtab_len: usize,
        base: u64,
        prog_base: u64, prog_end: u64,
        stack_base: u64, stack_end: u64,
    ) -> Self {
        let start = pages.ptr();
        Self {
            symtab: start,
            symtab_len,
            // SAFETY: the caller sized the allocation to `symtab_len +
            // strtab_len` and wrote `.strtab` right after `.symtab`, so
            // `symtab_len` is a valid offset inside `pages`.
            strtab: unsafe { start.add(symtab_len) },
            strtab_len,
            pages: Some(pages),
            base,
            prog_base, prog_end, stack_base, stack_end,
        }
    }

    // A failed `locate` returns an empty table rather than an error: a kernel that cannot find its symbols still boots.
    fn from_elf(data: &[u8], base: u64) -> Self {
        let Some((symtab, strtab)) = toyos_symbols::locate(data) else { return Self::empty() };

        Self {
            pages: None,
            symtab: symtab.as_ptr(),
            symtab_len: symtab.len(),
            strtab: strtab.as_ptr(),
            strtab_len: strtab.len(),
            base,
            prog_base: 0,
            prog_end: 0,
            stack_base: 0,
            stack_end: 0,
        }
    }

    fn tables(&self) -> SymTab<'_> {
        if self.symtab.is_null() || self.strtab.is_null() {
            return SymTab::empty();
        }
        // SAFETY: both pointers are non-null by the guard above, both ranges
        // are either the kernel image in the direct map or pages this table
        // owns and frees in `Drop`, and both lengths were bounded at
        // construction.
        unsafe {
            SymTab::new(
                core::slice::from_raw_parts(self.symtab, self.symtab_len),
                core::slice::from_raw_parts(self.strtab, self.strtab_len),
            )
        }
    }

    /// How much memory this table's bytes occupy, for the spawn log.
    pub fn resident_bytes(&self) -> usize {
        self.pages.as_ref().map_or(0, PageAlloc::size)
    }

    pub fn is_valid_user_addr(&self, addr: u64) -> bool {
        (addr >= self.prog_base && addr < self.prog_end)
            || (addr >= self.stack_base && addr < self.stack_end)
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

    pub fn prog_base(&self) -> u64 {
        self.prog_base
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
        log!("    {:#x}  {}+{:#x}", addr, symbol_text(rustc_demangle::demangle(raw)), offset);
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

/// Resolve and log a user address against a process's symbol table; returns
/// whether it was identified.
pub fn resolve_user(syms: &SymbolTable, addr: u64) -> bool {
    log_user(syms, addr, syms.resolve(addr))
}

/// [`resolve_user`] for a backtrace frame's return address — see [`SymbolTable::resolve_return`].
pub fn resolve_user_return(syms: &SymbolTable, return_addr: u64) -> bool {
    log_user(syms, return_addr, syms.resolve_return(return_addr))
}

fn log_user(syms: &SymbolTable, addr: u64, resolved: Option<(&str, u64)>) -> bool {
    if let Some((name, offset)) = resolved {
        log!("    {:#x}  {}+{:#x}", addr, symbol_text(rustc_demangle::demangle(name)), offset);
        true
    } else if syms.is_valid_user_addr(addr) {
        let base_offset = addr.saturating_sub(syms.prog_base());
        log!("    {:#x}  [exe+{:#x}]", addr, base_offset);
        true
    } else {
        false
    }
}
