//! Applying relocations to a loaded module.
//!
//! Every write goes through [`LoadedLib::write_at`]; every offset given to it
//! was parsed against the module's writable window by `load_shared_lib`, so
//! `write_at`'s asserts are kernel-bug asserts, not refusals. Every value
//! written is derived from a parsed relocation — never read back out of the
//! image — so what a slot holds before its write decides nothing.
//!
//! Unresolved symbols are logged and left unresolved, never fatal: a `.so`
//! naming an undefined symbol is untrusted input, not a kernel bug, and the
//! process faults on the slot only if it later uses it. A resolved TLS
//! reference whose `S + A` leaves the defining module's segment is refused.

use super::{occupied_tls, CachedRelocs, LibMemory, LoadedLib, TlsModule, TlsModuleInfo};
use crate::UserAddr;
use toyos_elf::sym::{Sym, SymTab};
use toyos_elf::{ImageOffset, Op, RelocError, SymIndex, TlsOffset, TlsRef, TlsSegment};

impl LoadedLib {
    /// Write a value at a byte offset within this module's kernel mapping.
    ///
    /// # Safety
    /// Caller must be the sole writer of this module's image (or `rw_alloc`) for the duration of the call.
    pub(super) unsafe fn write_at<T: Copy>(&self, offset: u64, value: T) {
        let end = (offset as usize)
            .checked_add(core::mem::size_of::<T>())
            .expect("LoadedLib::write_at: r_offset + width overflows");
        // Each arm asserts the bound protecting its own destination.
        match &self.memory {
            LibMemory::Owned(_) => {
                assert!(
                    end <= self.image.size(),
                    "LoadedLib::write_at: r_offset {:#x} outside image of {:#x}",
                    offset,
                    self.image.size()
                );
                self.image.write(offset as usize, value)
            }
            LibMemory::Shared { rw_alloc, rw_offset, rw_delta, .. } => {
                assert!(
                    offset as usize >= *rw_offset && end <= *rw_offset + rw_alloc.size(),
                    "LoadedLib::write_at: r_offset {:#x} outside the writable window [{:#x}, {:#x})",
                    offset,
                    rw_offset,
                    rw_offset + rw_alloc.size()
                );
                let ptr = (self.image.base().add(offset as usize) as i64 + rw_delta) as *mut T;
                ptr.write_unaligned(value);
            }
        }
    }

    /// Every slot of one kind and what it names — from the cache when a clone
    /// holds one, else scanned off the image, the two never both present.
    fn entries<'a, T: Copy + 'a>(
        &'a self,
        of: fn(Op) -> Option<T>,
        pick: fn(&CachedRelocs) -> &alloc::vec::Vec<(u64, T)>,
    ) -> impl Iterator<Item = (u64, T)> + 'a {
        let cached = self.cached_relocs.as_ref().map(|r| pick(r).iter().copied());
        let scanned = self.cached_relocs.is_none().then(move || {
            self.relocations().filter_map(move |r| Some((r.offset(), of(r.op())?)))
        });
        cached.into_iter().flatten().chain(scanned.into_iter().flatten())
    }

    fn bind_entries(&self) -> impl Iterator<Item = (u64, SymIndex)> + '_ {
        self.entries(|op| match op { Op::Bind(sym) => Some(sym), _ => None }, |c| &c.bind)
    }

    fn dtpmod_entries(&self) -> impl Iterator<Item = (u64, Option<SymIndex>)> + '_ {
        self.entries(|op| match op { Op::DtpMod64(sym) => Some(sym), _ => None }, |c| &c.dtpmod64)
    }

    /// Every `RELATIVE` slot and the position in the image it points at.
    fn relative_entries(&self) -> impl Iterator<Item = (u64, ImageOffset)> + '_ {
        self.relocations().filter_map(|r| match r.op() {
            Op::Relative(target) => Some((r.offset(), target)),
            _ => None,
        })
    }
}

/// Point every `R_X86_64_RELATIVE` slot into the image at `lib.user_base`.
///
/// Each value is the image's address plus the slot's parsed target, a position
/// inside the image — never the slot's old contents, which a module mapped into
/// a running process may already have changed.
pub fn rebase_relative_relocs(lib: &LoadedLib) {
    for (offset, target) in lib.relative_entries() {
        // SAFETY: see write_at's `# Safety`.
        unsafe { lib.write_at::<u64>(offset, (lib.user_base + target.get()).raw()) };
    }
}

/// Bind a `dlopen`ed module's `GLOB_DAT`/`JUMP_SLOT` slots to symbols the
/// process already has.
pub fn resolve_dlopen_relocs(lib: &LoadedLib, other_libs: &[LoadedLib]) {
    let symbols = lib.symbols();
    let mut resolved = 0u64;
    let mut unresolved = 0u64;
    for (offset, sym) in lib.bind_entries() {
        let name = symbols.name(sym.get());
        match other_libs.iter().find_map(|other| other.resolve(name)) {
            Some(addr) => {
                // SAFETY: rela::tables_outside_window refused any image whose tables meet the window these writes land in.
                unsafe { lib.write_at::<u64>(offset, addr.raw()) };
                resolved += 1;
            }
            None => {
                if unresolved < 5 {
                    log!("dlopen: unresolved: {}", name);
                }
                unresolved += 1;
            }
        }
    }
    log!("dlopen: resolved {} relocs, {} unresolved", resolved, unresolved);
}

/// Bind a startup library's `GLOB_DAT`/`JUMP_SLOT` slots, preferring the
/// executable's own exports.
pub fn resolve_lib_bind_relocs(
    lib: &LoadedLib,
    exe_sym_map: &alloc::collections::BTreeMap<&str, UserAddr>,
    libs: &[LoadedLib],
) {
    let symbols = lib.symbols();
    for (offset, sym) in lib.bind_entries() {
        let name = symbols.name(sym.get());
        let resolved = exe_sym_map
            .get(name)
            .copied()
            .or_else(|| libs.iter().find_map(|other| other.resolve(name)));
        match resolved {
            // SAFETY: rela::tables_outside_window refused any image whose tables meet the window these writes land in.
            Some(addr) => unsafe { lib.write_at::<u64>(offset, addr.raw()) },
            None => log!("dynamic: lib unresolved symbol: {}", name),
        }
    }
}

/// Apply `R_X86_64_TPOFF64` and `R_X86_64_TPOFF32`: the initial-exec TLS
/// model, a fixed offset from the thread pointer.
///
/// Every value is resolved before the first is written, so a refused module is
/// left as it was found.
pub fn apply_tpoff_relocs(
    lib: &LoadedLib,
    lib_base_offset: usize,
    tls: toyos_elf::tls::Static,
    tls_info: &TlsModuleInfo,
) -> Result<(), RelocError> {
    let tpoff64 = |op| match op {
        Op::Tpoff64(t) => Some(t),
        _ => None,
    };
    let tpoff32 = |op| match op {
        Op::Tpoff32(t) => Some(t),
        _ => None,
    };
    let tpoff = |r| compute_tpoff(r, lib_base_offset, lib.tls(), lib.symbols(), tls, tls_info);
    for (_, r) in lib.entries(tpoff64, |c| &c.tpoff64) {
        tpoff(r)?;
    }
    for (_, r) in lib.entries(tpoff32, |c| &c.tpoff32) {
        tpoff32_value(tpoff(r)?)?;
    }

    let mut count64 = 0u64;
    for (offset, r) in lib.entries(tpoff64, |c| &c.tpoff64) {
        let value = tpoff(r)?;
        // SAFETY: see write_at's `# Safety`.
        unsafe { lib.write_at::<i64>(offset, value) };
        count64 += 1;
    }
    let mut count32 = 0u64;
    for (offset, r) in lib.entries(tpoff32, |c| &c.tpoff32) {
        let value = tpoff32_value(tpoff(r)?)?;
        // SAFETY: see write_at's `# Safety`.
        unsafe { lib.write_at::<i32>(offset, value) };
        count32 += 1;
    }
    if count64 > 0 || count32 > 0 {
        log!(
            "dlopen: applied {} TPOFF64 + {} TPOFF32 relocs (base_offset={}, total_memsz={})",
            count64, count32, lib_base_offset, tls.total_memsz()
        );
    }
    Ok(())
}

/// A `TPOFF32`'s field is 32 bits the instruction sign-extends; a value outside
/// them names some other address than the one resolved.
pub fn tpoff32_value(tpoff: i64) -> Result<i32, RelocError> {
    i32::try_from(tpoff).map_err(|_| RelocError::TpoffOverflows)
}

/// Apply `R_X86_64_DTPMOD64`: the general-dynamic TLS model's module id.
pub fn apply_dtpmod_relocs(lib: &LoadedLib, module_id: u64, tls_info: &TlsModuleInfo) {
    let mut count = 0u64;
    for (offset, sym) in lib.dtpmod_entries() {
        let mid = resolve_dtpmod(lib, sym, module_id, tls_info);
        // SAFETY: see write_at's `# Safety`.
        unsafe { lib.write_at::<u64>(offset, mid) };
        count += 1;
    }
    if count > 0 {
        log!("dlopen: applied {} DTPMOD64 relocs (module_id={})", count, module_id);
    }
}

/// Apply `R_X86_64_DTPOFF64`: the general-dynamic TLS model's offset within the
/// defining module's block. Every value is resolved before the first is
/// written, so a refused module is left as it was found.
pub fn apply_dtpoff_relocs(lib: &LoadedLib, tls_info: &TlsModuleInfo) -> Result<(), RelocError> {
    let dtpoff = |op| match op {
        Op::DtpOff64(t) => Some(t),
        _ => None,
    };
    let resolve = |r| resolve_tls_ref(r, 0, lib.tls(), lib.symbols(), tls_info);
    for (_, r) in lib.entries(dtpoff, |c| &c.dtpoff64) {
        resolve(r)?;
    }
    let mut count = 0u64;
    for (offset, r) in lib.entries(dtpoff, |c| &c.dtpoff64) {
        let value = resolve(r)?.map_or(0, |(_, at)| at.get());
        // SAFETY: see write_at's `# Safety`.
        unsafe { lib.write_at::<u64>(offset, value) };
        count += 1;
    }
    if count > 0 {
        log!("dlopen: applied {} DTPOFF64 relocs", count);
    }
    Ok(())
}

/// The module in `tls_info` that defines `name`, its `PT_TLS`, and the symbol
/// as it defines it, or `None` if none does.
pub fn defining_module<'a>(name: &str, tls_info: &'a TlsModuleInfo) -> Option<(&'a TlsModule, TlsSegment, Sym)> {
    for lib in tls_info.libs {
        let Some(segment) = occupied_tls(lib.tls()) else {
            continue;
        };
        if let Some(sym) = lib.symbols().find_tls(name) {
            // Template pointer is unique per module: each points into a distinct image.
            // No matching module here means inconsistent tables; treated as unresolved, not a bug.
            let module = tls_info
                .modules
                .iter()
                .find(|m| m.template == lib.tls_template)?;
            return Some((module, segment, sym));
        }
    }
    None
}

fn resolve_dtpmod(lib: &LoadedLib, sym: Option<SymIndex>, self_module_id: u64, tls_info: &TlsModuleInfo) -> u64 {
    let Some(sym) = sym else { return self_module_id };
    let symbols = lib.symbols();
    if symbols.get(sym.get()).is_some_and(|s| s.is_defined()) {
        return self_module_id;
    }
    let name = symbols.name(sym.get());
    match defining_module(name, tls_info) {
        Some((module, _, _)) => module.module_id,
        None => {
            log!("dtpmod: unresolved TLS symbol: {}", name);
            self_module_id
        }
    }
}

/// `S + A` for one TLS reference, and the static-block offset of the module it
/// lies in: the referencing module's own (`own_base_offset`, `own_tls`), or
/// the module defining the symbol. `None` is a symbol no module defines, which
/// is logged; a sum outside the defining module's segment refuses the module.
///
/// `symbols` is the referencing module's table — a library's in-image one, or
/// the executable's read off the file — so both loaders resolve through this
/// one function.
fn resolve_tls_ref(
    r: TlsRef,
    own_base_offset: usize,
    own_tls: Option<TlsSegment>,
    symbols: SymTab<'_>,
    tls_info: &TlsModuleInfo,
) -> Result<Option<(usize, TlsOffset)>, RelocError> {
    let s = match r {
        TlsRef::Own(at) => return Ok(Some((own_base_offset, at))),
        TlsRef::Symbol(s) => s,
    };
    let sym = symbols.at(s.sym())?;
    if sym.is_defined() {
        let segment = own_tls.ok_or(RelocError::TlsOutsideSegment)?;
        let at = sym.tls_offset(s.addend(), segment).ok_or(RelocError::TlsOutsideSegment)?;
        return Ok(Some((own_base_offset, at)));
    }
    let name = sym.name_in(symbols.strings());
    match defining_module(name, tls_info) {
        Some((module, segment, defined)) => {
            let at = defined.tls_offset(s.addend(), segment).ok_or(RelocError::TlsOutsideSegment)?;
            Ok(Some((module.base_offset, at)))
        }
        None => {
            log!("tls: unresolved TLS symbol: {}", name);
            Ok(None)
        }
    }
}

/// `S + A - tp` for one initial-exec reference, or `0` for a symbol no module
/// defines: the one rule for a library's relocations and the executable's.
pub fn compute_tpoff(
    r: TlsRef,
    own_base_offset: usize,
    own_tls: Option<TlsSegment>,
    symbols: SymTab<'_>,
    tls: toyos_elf::tls::Static,
    tls_info: &TlsModuleInfo,
) -> Result<i64, RelocError> {
    match resolve_tls_ref(r, own_base_offset, own_tls, symbols, tls_info)? {
        Some((base_offset, at)) => tls.tpoff(base_offset, at).ok_or(RelocError::TpoffOverflows),
        None => Ok(0),
    }
}
