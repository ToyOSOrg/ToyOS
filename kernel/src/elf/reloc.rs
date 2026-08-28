//! Applying relocations to a loaded module.
//!
//! Every write goes through [`LoadedLib::write_at`]; every offset given to it
//! was already validated against the module's writable window by
//! `load_shared_lib`, so `write_at`'s asserts are kernel-bug asserts, not
//! refusals. Unresolved symbols are logged and left unresolved, never fatal:
//! a `.so` naming an undefined symbol is untrusted input, not a kernel bug,
//! and the process faults on the slot only if it later uses it.

use super::{CachedRelocs, LibMemory, LoadedLib, TlsModule, TlsModuleInfo};
use crate::UserAddr;
use toyos_elf::RelocKind;

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

    fn bind_entries(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        let cached = self.cached_relocs.as_ref().map(|r| r.bind.iter().copied());
        let scanned = self.cached_relocs.is_none().then(|| {
            self.relocations()
                .filter(|r| r.kind.is_bind())
                .map(|r| (r.offset, r.sym))
        });
        cached.into_iter().flatten().chain(scanned.into_iter().flatten())
    }

    fn typed_entries(
        &self,
        kind: RelocKind,
        pick: fn(&CachedRelocs) -> &alloc::vec::Vec<(u64, u32, i64)>,
    ) -> impl Iterator<Item = (u64, u32, i64)> + '_ {
        let cached = self.cached_relocs.as_ref().map(|r| pick(r).iter().copied());
        let scanned = self.cached_relocs.is_none().then(move || {
            self.relocations()
                .filter(move |r| r.kind == kind)
                .map(|r| (r.offset, r.sym, r.addend))
        });
        cached.into_iter().flatten().chain(scanned.into_iter().flatten())
    }
}

/// Add `delta` to every `R_X86_64_RELATIVE` slot.
///
/// Reads the old value from the shared image, not the private window, because
/// this only runs on a freshly cloned window that's still byte-identical to it.
pub fn rebase_relative_relocs(lib: &LoadedLib, delta: i64) {
    for r in lib.relocations() {
        if r.kind == RelocKind::Relative {
            // SAFETY: this window was just cloned; nothing else writes to it yet.
            let old = unsafe { lib.image.read::<u64>(r.offset as usize) };
            // SAFETY: see write_at's `# Safety`.
            unsafe { lib.write_at::<u64>(r.offset, (old as i64 + delta) as u64) };
        }
    }
}

/// Bind a `dlopen`ed module's `GLOB_DAT`/`JUMP_SLOT` slots to symbols the
/// process already has.
pub fn resolve_dlopen_relocs(lib: &LoadedLib, other_libs: &[LoadedLib]) {
    let symbols = lib.symbols();
    let mut resolved = 0u64;
    let mut unresolved = 0u64;
    for (offset, sym) in lib.bind_entries() {
        let name = symbols.name(sym as usize);
        match other_libs.iter().find_map(|other| other.resolve(name)) {
            Some(addr) => {
                // SAFETY: see write_at's `# Safety`.
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
    exe_sym_map: &hashbrown::HashMap<&str, UserAddr>,
    libs: &[LoadedLib],
) {
    let symbols = lib.symbols();
    for (offset, sym) in lib.bind_entries() {
        let name = symbols.name(sym as usize);
        let resolved = exe_sym_map
            .get(name)
            .copied()
            .or_else(|| libs.iter().find_map(|other| other.resolve(name)));
        match resolved {
            // SAFETY: see write_at's `# Safety`.
            Some(addr) => unsafe { lib.write_at::<u64>(offset, addr.raw()) },
            None => log!("dynamic: lib unresolved symbol: {}", name),
        }
    }
}

/// Apply `R_X86_64_TPOFF64` and `R_X86_64_TPOFF32`: the initial-exec TLS
/// model, a fixed offset from the thread pointer.
pub fn apply_tpoff_relocs(
    lib: &LoadedLib,
    lib_base_offset: usize,
    total_memsz: usize,
    tls_info: &TlsModuleInfo,
) {
    let mut count64 = 0u64;
    for (offset, sym, addend) in lib.typed_entries(RelocKind::Tpoff64, |r| &r.tpoff64) {
        let tpoff = compute_tpoff(lib, sym, addend, lib_base_offset, total_memsz, tls_info);
        // SAFETY: see write_at's `# Safety`.
        unsafe { lib.write_at::<u64>(offset, tpoff as u64) };
        count64 += 1;
    }
    let mut count32 = 0u64;
    for (offset, sym, addend) in lib.typed_entries(RelocKind::Tpoff32, |r| &r.tpoff32) {
        let tpoff = compute_tpoff(lib, sym, addend, lib_base_offset, total_memsz, tls_info);
        // SAFETY: see write_at's `# Safety`.
        unsafe { lib.write_at::<i32>(offset, tpoff as i32) };
        count32 += 1;
    }
    if count64 > 0 || count32 > 0 {
        log!(
            "dlopen: applied {} TPOFF64 + {} TPOFF32 relocs (base_offset={}, total_memsz={})",
            count64, count32, lib_base_offset, total_memsz
        );
    }
}

/// Apply `R_X86_64_DTPMOD64` and `R_X86_64_DTPOFF64`: the general-dynamic TLS
/// model resolved through the DTV.
pub fn apply_dtpmod_relocs(lib: &LoadedLib, module_id: u64, tls_info: &TlsModuleInfo) {
    let mut count_mod = 0u64;
    for (offset, sym, _) in lib.typed_entries(RelocKind::DtpMod64, |r| &r.dtpmod64) {
        let mid = resolve_dtpmod(lib, sym, module_id, tls_info);
        // SAFETY: see write_at's `# Safety`.
        unsafe { lib.write_at::<u64>(offset, mid) };
        count_mod += 1;
    }
    let mut count_off = 0u64;
    for (offset, sym, addend) in lib.typed_entries(RelocKind::DtpOff64, |r| &r.dtpoff64) {
        let value = resolve_dtpoff(lib, sym, addend, tls_info);
        // SAFETY: see write_at's `# Safety`.
        unsafe { lib.write_at::<u64>(offset, value as u64) };
        count_off += 1;
    }
    if count_mod > 0 || count_off > 0 {
        log!(
            "dlopen: applied {} DTPMOD64 + {} DTPOFF64 relocs (module_id={})",
            count_mod, count_off, module_id
        );
    }
}

/// The module in `tls_info` that defines `name`, or `None` if none does.
pub fn defining_module<'a>(name: &str, tls_info: &'a TlsModuleInfo) -> Option<(&'a TlsModule, u64)> {
    for lib in tls_info.libs {
        if lib.tls_memsz == 0 {
            continue;
        }
        if let Some(sym_offset) = lib.resolve_tls(name) {
            // Template pointer is unique per module: each points into a distinct image.
            // No matching module here means inconsistent tables; treated as unresolved, not a bug.
            let module = tls_info
                .modules
                .iter()
                .find(|m| m.template == lib.tls_template)?;
            return Some((module, sym_offset));
        }
    }
    None
}

fn resolve_dtpmod(lib: &LoadedLib, r_sym: u32, self_module_id: u64, tls_info: &TlsModuleInfo) -> u64 {
    if r_sym == 0 {
        return self_module_id;
    }
    let symbols = lib.symbols();
    if symbols.get(r_sym as usize).is_some_and(|s| s.is_defined()) {
        return self_module_id;
    }
    let name = symbols.name(r_sym as usize);
    match defining_module(name, tls_info) {
        Some((module, _)) => module.module_id,
        None => {
            log!("dtpmod: unresolved TLS symbol: {}", name);
            self_module_id
        }
    }
}

fn resolve_dtpoff(lib: &LoadedLib, r_sym: u32, r_addend: i64, tls_info: &TlsModuleInfo) -> i64 {
    if r_sym == 0 {
        return r_addend;
    }
    let symbols = lib.symbols();
    if let Some(sym) = symbols.get(r_sym as usize).filter(|s| s.is_defined()) {
        return sym.value as i64 + r_addend;
    }
    let name = symbols.name(r_sym as usize);
    match defining_module(name, tls_info) {
        Some((_, sym_offset)) => sym_offset as i64 + r_addend,
        None => {
            log!("dtpoff: unresolved TLS symbol: {}", name);
            r_addend
        }
    }
}

/// `S + A - tp` for one initial-exec reference: `S`'s place in the block and
/// the addend go through `tls::tpoff`, which folds in `A` on every branch.
fn compute_tpoff(
    lib: &LoadedLib,
    r_sym: u32,
    r_addend: i64,
    lib_base_offset: usize,
    total_memsz: usize,
    tls_info: &TlsModuleInfo,
) -> i64 {
    if r_sym == 0 {
        return toyos_elf::tls::tpoff(lib_base_offset as u64, r_addend, total_memsz);
    }
    let symbols = lib.symbols();
    if let Some(sym) = symbols.get(r_sym as usize).filter(|s| s.is_defined()) {
        return toyos_elf::tls::tpoff(lib_base_offset as u64 + sym.value, r_addend, total_memsz);
    }
    let name = symbols.name(r_sym as usize);
    match defining_module(name, tls_info) {
        Some((module, sym_offset)) => {
            toyos_elf::tls::tpoff(module.base_offset as u64 + sym_offset, r_addend, total_memsz)
        }
        None => {
            log!("tpoff: unresolved TLS symbol: {}", name);
            0
        }
    }
}
