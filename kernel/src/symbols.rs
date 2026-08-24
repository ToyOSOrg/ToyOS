//! The raw-pointer half of a kernel backtrace frame, and the boot-time and
//! per-process state that owns it.
//!
//! `toyos-symbols` holds the other half — where an ELF's symbol tables live
//! in its bytes, and how much of a record's budget a demangled name gets —
//! as pure functions tested on the host against a real binary. What stays
//! here is what cannot move there: [`SymbolTable`] is read from the fault
//! handler, the panic handler and a double fault, so it may not allocate,
//! take a lock or do I/O, and its two tables are raw pointers rather than
//! borrowed slices because nothing with a lifetime can be threaded through
//! that path. The boot-time globals (`KERNEL_SYMS`, `load_kernel`) and the
//! `log!` integration are boot policy, not a decision about bytes, so they
//! stay here too.

use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use alloc::boxed::Box;
use toyos_elf::sym::SymTab;
use toyos_symbols::symbol_text;

use crate::process::PageAlloc;

/// Zero-allocation symbol table. Points directly into ELF sections in memory.
/// Resolution is a linear scan over raw Elf64_Sym entries — O(n) but lock-free,
/// allocation-free, and safe to call from any context including panic/double-fault.
///
/// **That property is the reason for the raw pointers**, and it is what decides
/// where the bytes may come from: the resolve path is reached from the fault
/// handler and from the panic handler, so it may not allocate, may not take a
/// lock and may not do I/O. Everything expensive happens once, when the table
/// is built.
pub struct SymbolTable {
    /// What keeps the bytes below mapped, when this table owns them.
    ///
    /// `None` for the kernel's own tables: they point into the ELF the
    /// bootloader left in the direct map, which outlives every reader. A
    /// process's tables are read off its file into these pages, so they have to
    /// die with it — and the pointers survive the struct moving, because 2 MiB
    /// physical pages do not move when a `Vec` header does.
    pages: Option<PageAlloc>,
    /// Raw `.symtab` section data in memory, and its length in *bytes* — the
    /// entry count is `SymTab`'s to derive, because 24 is the only width an
    /// `Elf64_Sym` has and a file that declares another one is a file whose
    /// count and whose stride would disagree.
    symtab: *const u8,
    symtab_len: usize,
    /// Raw .strtab section data in memory.
    strtab: *const u8,
    strtab_len: usize,
    /// ELF load base address.
    base: u64,
    prog_base: u64,
    prog_end: u64,
    stack_base: u64,
    stack_end: u64,
}

// SAFETY: the bytes are either the kernel image, mapped for the machine's
// lifetime, or pages this table owns and frees — so moving the table between
// CPUs moves the whole of what the two raw pointers name, and nothing is left
// behind. `SymbolTable` is `!Send` only because `*const u8` is.
//
// Irreducible: the pointers exist so that a symbol lookup borrows the tables
// instead of copying them, and a `&'static [u8]` cannot be written for the
// half that is a `PageAlloc` this value owns and frees.
unsafe impl Send for SymbolTable {}
// SAFETY: same reasoning as `Send`, plus what `Sync` adds: every method here
// takes `&self` and none of them writes through either pointer — the bytes are
// fixed when `from_pages`/`from_elf` builds the table and are read-only for
// its whole life, so concurrent lookups from several CPUs (which is what a
// multi-CPU panic is) read the same immutable bytes.
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

    /// A process's tables, in pages it owns: `symtab_len` bytes of `.symtab` at
    /// the start, `strtab_len` bytes of `.strtab` immediately after.
    ///
    /// Laid out by one caller, [`crate::loader::symbols::read_backtrace_table`],
    /// which is also what read them — so the two halves cannot be given
    /// separately and cannot disagree about where the second one starts.
    // Eight arguments, and the paragraph above is why: four are the two address
    // ranges a backtrace is checked against, and every one arrives from the one
    // caller that already holds it.
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
            // SAFETY: `read_backtrace_table` is the one caller, and it is also
            // what laid these pages out — it sized the allocation to hold
            // `symtab_len + strtab_len` and wrote `.strtab` immediately after
            // `.symtab`, so `symtab_len` is an offset inside `pages`. The
            // doc comment above says why the two halves cannot be handed in
            // separately and so cannot disagree about this offset.
            //
            // Irreducible: the two tables share one allocation on purpose (one
            // `PageAlloc`, one `Drop`), and splitting a `*mut u8` at a run-time
            // offset is pointer arithmetic. What is *not* discharged here is
            // that the allocation is big enough — that lives at the caller,
            // where the size was chosen.
            strtab: unsafe { start.add(symtab_len) },
            strtab_len,
            pages: Some(pages),
            base,
            prog_base, prog_end, stack_base, stack_end,
        }
    }

    /// Find `.symtab` and its `.strtab` in an ELF already in memory, and point
    /// at them. No copying — only pointers into `data`.
    ///
    /// Located through `toyos-symbols::locate`, which decodes through
    /// `toyos-elf` in turn — the tree's one ELF decoder: this file used to
    /// hold the second, on crates.io `elf` 0.8, reached from two lines of the
    /// whole kernel. A refusal from `locate` answers with a table that names
    /// nothing, because a kernel that cannot find its symbols still boots —
    /// it prints bare addresses.
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

    /// The two tables as one view, for the resolve path.
    ///
    /// The one `unsafe` left in this file's lookup, and it is the whole of
    /// what the raw pointers cost: the bytes are either the kernel image in the
    /// direct map, which outlives the machine, or pages this table owns and
    /// frees in its own `Drop`, and both lengths were bounded against the file
    /// they were read from. Everything past this line is `toyos-elf`, which
    /// forbids `unsafe` and indexes nothing unchecked.
    fn tables(&self) -> SymTab<'_> {
        if self.symtab.is_null() || self.strtab.is_null() {
            return SymTab::empty();
        }
        // SAFETY: the doc comment above is the argument in full — both
        // pointers are non-null by the guard, both ranges are either the
        // kernel image in the direct map (live for the machine's lifetime) or
        // pages this `SymbolTable` owns and frees in its own `Drop`, and both
        // lengths were bounded against the file they were read from. The
        // returned slices borrow `self`, so they cannot outlive that `Drop`,
        // and nothing ever writes through either pointer (see the `Sync` impl
        // above), so no `&mut` can alias them.
        //
        // Irreducible: it is the raw-pointer-pair-to-slice conversion, which
        // is the only way to hand `toyos-elf` — a crate that `forbid`s
        // `unsafe` and indexes nothing unchecked — anything to look at.
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

    /// Resolve an address to (mangled_name, offset). Linear scan — no
    /// allocation, no lock, no panic.
    ///
    /// The scan itself is [`SymTab::resolve`], whose rules — `STT_FUNC` only,
    /// the nearest symbol below, the sized winner of an alias pair, and no
    /// answer past a sized symbol's last byte — are argued and tested there.
    /// An address below the load base belongs to no symbol in this module.
    pub fn resolve(&self, addr: u64) -> Option<(&str, u64)> {
        self.tables().resolve(addr.checked_sub(self.base)?)
    }

    /// [`resolve`](Self::resolve) for a *return address*.
    ///
    /// A return address is the instruction after the `call`, so when the call
    /// is the last instruction of its function — every call to a diverging
    /// function, and any tail position — it lands one byte past the symbol's
    /// last byte and `resolve` correctly refuses it. Every backtrace frame but
    /// the innermost is a return address, which is why panic reports read
    /// `[kernel+0x…]` far more often than the symbol table's coverage explains.
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

/// Load kernel symbols from raw ELF bytes in the direct map. Called once at boot.
/// Stores pointers into the ELF data — the only allocation is the ~72-byte SymbolTable struct.
pub fn load_kernel(data: &[u8], base: u64) {
    let table = SymbolTable::from_elf(data, base);
    let count = table.tables().count();
    KERNEL_SYMS.store(Box::into_raw(Box::new(table)), Ordering::Release);
    log!("symbols: loaded {} kernel symbols", count);
}

/// Resolve and log an address against kernel symbols. Lock-free, allocation-free.
/// Safe to call from any context including panic, double fault, NMI.
pub fn resolve_kernel(addr: u64) -> Option<u64> {
    log_kernel(addr, |table| table.resolve(addr))
}

/// [`resolve_kernel`] for a backtrace frame's return address — see
/// [`SymbolTable::resolve_return`].
pub fn resolve_kernel_return(return_addr: u64) -> Option<u64> {
    log_kernel(return_addr, |table| table.resolve_return(return_addr))
}

fn log_kernel(addr: u64, lookup: impl FnOnce(&SymbolTable) -> Option<(&str, u64)>) -> Option<u64> {
    let ptr = KERNEL_SYMS.load(Ordering::Acquire);
    if ptr.is_null() {
        log!("    {:#x}", addr);
        return None;
    }
    // SAFETY: `KERNEL_SYMS` is written exactly once, in `load_kernel`, from a
    // `Box::into_raw` that is never reclaimed — the table is leaked on purpose
    // so a panic at any later instant can read it — and the `Release` there
    // pairs with the `Acquire` above, so a non-null pointer here names a fully
    // constructed `SymbolTable`. The guard above is what rules out the null it
    // holds before boot reaches `load_kernel`.
    //
    // Irreducible: a `static` that is initialized after boot and read
    // lock-free from panic, double-fault and NMI context is an `AtomicPtr`;
    // `OnceLock` and friends need an allocator-free constructor this cannot
    // give, and any lock here would be one a double fault could find held.
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

/// Resolve and log a user address against a process's symbol table.
/// Returns true if the address could be identified.
pub fn resolve_user(syms: &SymbolTable, addr: u64) -> bool {
    log_user(syms, addr, syms.resolve(addr))
}

/// [`resolve_user`] for a backtrace frame's return address — see
/// [`SymbolTable::resolve_return`].
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
