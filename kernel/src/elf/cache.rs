//! The shared-object cache: one image in memory, one private writable window
//! per process.
//!
//! A cached module's read-only pages are mapped into every process that loads
//! it and its base address never moves, so its `R_X86_64_RELATIVE` relocations
//! need no rework. Only the writable window is copied.
//!
//! **Nothing is ever removed, and both refusals follow from that.** An entry is
//! mapped into every process that loaded it and no address space is reachable
//! from here, so a changed file cannot be answered by reloading and a full
//! budget cannot be answered by evicting. Both are refused instead.

use alloc::string::String;
use alloc::vec::Vec;
use toyos_abi::syscall::SyscallError;

use super::{LibMemory, LoadedLib};
use crate::mm::{KernelSlice, MAX_HEAP_ALLOC};
use crate::process::PageAlloc;
use crate::sync::Lock;
use crate::vfs::BackingId;
use crate::UserAddr;
use toyos_elf::{RelaCounts, RelocKind};

/// A module's non-`RELATIVE` relocations, extracted once at cache time.
#[derive(Clone)]
pub struct CachedRelocs {
    /// `GLOB_DAT` and `JUMP_SLOT`: (offset, symbol).
    pub bind: Vec<(u64, u32)>,
    pub tpoff64: Vec<(u64, u32, i64)>,
    pub tpoff32: Vec<(u64, u32, i64)>,
    /// The kernel writes a module id here.
    pub dtpmod64: Vec<(u64, u32, i64)>,
    /// The kernel writes a TLS offset within the module here.
    pub dtpoff64: Vec<(u64, u32, i64)>,
}

// Extracts every non-`RELATIVE` entry, or `None` if it would not fit one kernel allocation.
fn prescan_relocs(lib: &LoadedLib) -> Option<CachedRelocs> {
    let counts = RelaCounts::of(lib.relocations());
    let widest = core::mem::size_of::<(u64, u32, i64)>();
    // Excludes `Relative`: bounding on it would refuse to cache nearly every library.
    let kept = [RelocKind::GlobDat, RelocKind::Tpoff64, RelocKind::Tpoff32,
        RelocKind::DtpMod64, RelocKind::DtpOff64];
    if counts.max_of(&kept).checked_mul(widest).is_none_or(|b| b > MAX_HEAP_ALLOC) {
        log!("dlopen: prescan {:?} will not fit one allocation, not caching", counts);
        return None;
    }
    // Capacities are reserved exactly from `counts`; growing them could allocate past the bound just checked.
    let mut relocs = CachedRelocs {
        bind: Vec::with_capacity(counts.bind),
        tpoff64: Vec::with_capacity(counts.tpoff64),
        tpoff32: Vec::with_capacity(counts.tpoff32),
        dtpmod64: Vec::with_capacity(counts.dtpmod64),
        dtpoff64: Vec::with_capacity(counts.dtpoff64),
    };
    for r in lib.relocations() {
        match r.kind {
            RelocKind::GlobDat | RelocKind::JumpSlot => relocs.bind.push((r.offset, r.sym)),
            RelocKind::Tpoff64 => relocs.tpoff64.push((r.offset, r.sym, r.addend)),
            RelocKind::Tpoff32 => relocs.tpoff32.push((r.offset, r.sym, r.addend)),
            RelocKind::DtpMod64 => relocs.dtpmod64.push((r.offset, r.sym, r.addend)),
            RelocKind::DtpOff64 => relocs.dtpoff64.push((r.offset, r.sym, r.addend)),
            _ => {}
        }
    }
    Some(relocs)
}

// Fields identical between the cache entry and every clone: only memory ownership, user base and relocations differ.
#[derive(Clone, Copy)]
struct Snapshot {
    image: KernelSlice,
    dynsym: Option<KernelSlice>,
    dynstr: Option<KernelSlice>,
    tls_template: Option<KernelSlice>,
    tls_memsz: usize,
    tls_align: usize,
    rela: Option<KernelSlice>,
    jmprel: Option<KernelSlice>,
    gnu_hash: Option<KernelSlice>,
    eh_frame_hdr_vaddr: u64,
    eh_frame_hdr_size: u64,
    init_array_vaddr: u64,
    init_array_size: u64,
    span: u64,
    rw_lo: u64,
    rw_hi: u64,
}

impl Snapshot {
    fn of(lib: &LoadedLib) -> Snapshot {
        Snapshot {
            image: lib.image,
            dynsym: lib.dynsym,
            dynstr: lib.dynstr,
            tls_template: lib.tls_template,
            tls_memsz: lib.tls_memsz,
            tls_align: lib.tls_align,
            rela: lib.rela,
            jmprel: lib.jmprel,
            gnu_hash: lib.gnu_hash,
            eh_frame_hdr_vaddr: lib.eh_frame_hdr_vaddr,
            eh_frame_hdr_size: lib.eh_frame_hdr_size,
            init_array_vaddr: lib.init_array_vaddr,
            init_array_size: lib.init_array_size,
            span: lib.span,
            rw_lo: lib.rw_lo,
            rw_hi: lib.rw_hi,
        }
    }

    fn into_lib(
        self,
        memory: LibMemory,
        user_base: UserAddr,
        cached_relocs: Option<CachedRelocs>,
    ) -> LoadedLib {
        LoadedLib {
            memory,
            user_base,
            phys_base: self.image.phys(),
            image: self.image,
            dynsym: self.dynsym,
            dynstr: self.dynstr,
            tls_template: self.tls_template,
            tls_memsz: self.tls_memsz,
            tls_align: self.tls_align,
            rela: self.rela,
            jmprel: self.jmprel,
            gnu_hash: self.gnu_hash,
            cached_relocs,
            eh_frame_hdr_vaddr: self.eh_frame_hdr_vaddr,
            eh_frame_hdr_size: self.eh_frame_hdr_size,
            init_array_vaddr: self.init_array_vaddr,
            init_array_size: self.init_array_size,
            span: self.span,
            rw_lo: self.rw_lo,
            rw_hi: self.rw_hi,
        }
    }
}

/// An immortal image, used as the template every later load clones from.
struct CachedLib {
    alloc: PageAlloc,
    snapshot: Snapshot,
    rw_offset: usize,
    rw_size: usize,
    relocs: CachedRelocs,
    /// The file this image was built from, as the mount described it at insert.
    id: BackingId,
}

// Entries are pushed only, never removed: `clone_from_cache`'s SAFETY depends on `cached.alloc` staying live forever.
static SO_CACHE: Lock<Vec<(String, CachedLib)>> = Lock::new(Vec::new());

/// The most physical memory every cached image may hold between them.
///
/// **A policy number**: nothing derives it. For scale, the largest shared
/// object this tree builds loads a span of 144,760,832 bytes, a 146,800,640
/// byte allocation — so this admits one of those and refuses a second.
const BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// `so-cache-tiny`s number, within reach of the 2 MiB libraries a guest can
/// build. Only the magnitude moves; the comparison and the refusal are shipped.
const TINY_BUDGET_BYTES: usize = 8 * 1024 * 1024;

fn budget_bytes() -> usize {
    if crate::actuator::so_cache_tiny() {
        TINY_BUDGET_BYTES
    } else {
        BUDGET_BYTES
    }
}

/// What a lookup found under a path.
pub enum Cached {
    /// A clone of the cached image; the file behind the path still matches it.
    Fresh(LoadedLib),
    /// An image is cached here and the file behind the path no longer matches it.
    /// The caller refuses by name: the old image cannot be freed while a process
    /// has it mapped, so a reload would map the library twice.
    Stale,
    /// Nothing usable — no entry, or a clone that found no memory.
    Absent,
}

/// Every cached image's allocation, summed. The caller holds the lock.
fn held_bytes(cache: &[(String, CachedLib)]) -> usize {
    cache.iter().map(|(_, c)| c.alloc.size()).sum()
}

/// Takes ownership of `lib` and returns a clone in `Shared` mode with a private writable window; returns it unchanged if it cannot be cached.
/// `Err` is the budget alone: an image that merely cannot be cached is still a working image, and only one over the budget is refused outright.
pub fn cache_loaded_lib(
    path: &str,
    id: BackingId,
    lib: LoadedLib,
    rw_offset: usize,
    rw_size: usize,
) -> Result<LoadedLib, SyscallError> {
    if !matches!(lib.memory, LibMemory::Owned(_)) {
        return Ok(lib);
    }
    let snapshot = Snapshot::of(&lib);
    let user_base = lib.user_base;
    // Must scan before `lib.memory` moves out: the scan reads the tables through `lib`.
    let scanned = prescan_relocs(&lib);
    let LibMemory::Owned(alloc) = lib.memory else {
        unreachable!("the check above established this")
    };

    // A lib without prescanned relocs keeps the scan-every-table path: the cache always stores what `cached_relocs` describes.
    let owned = |alloc| snapshot.into_lib(LibMemory::Owned(alloc), user_base, None);
    let Some(relocs) = scanned else {
        return Ok(owned(alloc));
    };
    let Some(rw_alloc) = PageAlloc::new(rw_size, crate::mm::pmm::Category::Elf) else {
        return Ok(owned(alloc));
    };
    let alloc_ptr = alloc.ptr();
    // SAFETY: `rw_offset`/`rw_size` are `load_shared_lib`'s validated window, so `alloc_ptr.add(rw_offset)` stays inside `alloc`; `rw_alloc` is a fresh, distinct allocation, so the ranges cannot overlap.
    unsafe {
        core::ptr::copy_nonoverlapping(alloc_ptr.add(rw_offset), rw_alloc.ptr(), rw_size);
    }
    let rw_delta = rw_alloc.ptr() as i64 - (alloc_ptr as i64 + rw_offset as i64);

    let mut cache = SO_CACHE.lock();
    // Asked again under the lock that publishes: `try_clone_cached` released it
    // before the load. Entries are never removed, so a second one for a name would
    // strand a whole library forever — the loser clones the winner's instead.
    if let Some(idx) = cache.iter().position(|(p, _)| p == path) {
        let cloned = clone_from_cache(&cache[idx].1);
        drop(cache);
        return Ok(cloned.unwrap_or_else(|| owned(alloc)));
    }
    // Under the lock that publishes, so two concurrent loads cannot both find
    // room for the last image and push anyway.
    let budget = budget_bytes();
    let (held, entries) = (held_bytes(&cache), cache.len());
    let Some(after) = held.checked_add(alloc.size()).filter(|b| *b <= budget) else {
        drop(cache);
        // Both allocations drop here, so the refusal gives back what the load took.
        log!(
            "dlopen: {} would take the shared-object cache to {} bytes over {} entries, past its \
             {}-byte budget; refused, and nothing is evicted for it",
            path, held.saturating_add(alloc.size()), entries + 1, budget
        );
        return Err(SyscallError::ResourceExhausted);
    };
    cache.push((
        String::from(path),
        CachedLib { alloc, snapshot, rw_offset, rw_size, relocs: relocs.clone(), id },
    ));
    drop(cache);
    log!(
        "dlopen: cached {} with {} bind + {} tpoff64 + {} tpoff32 + {} dtpmod64 + {} dtpoff64 pre-scanned relocs, cache now {} of {} bytes",
        path, relocs.bind.len(), relocs.tpoff64.len(), relocs.tpoff32.len(),
        relocs.dtpmod64.len(), relocs.dtpoff64.len(), after, budget
    );

    Ok(snapshot.into_lib(
        LibMemory::Shared {
            rw_alloc,
            cached_image: snapshot.image,
            rw_offset,
            rw_delta,
        },
        user_base,
        Some(relocs),
    ))
}

/// Clone what is cached under `path`, if `id` still describes the file it came from.
pub fn try_clone_cached(path: &str, id: BackingId) -> Cached {
    let cache = SO_CACHE.lock();
    let Some(idx) = cache.iter().position(|(p, _)| p == path) else {
        return Cached::Absent;
    };
    if cache[idx].1.id != id {
        return Cached::Stale;
    }
    match clone_from_cache(&cache[idx].1) {
        Some(lib) => Cached::Fresh(lib),
        None => Cached::Absent,
    }
}

// Base address stays the cache's: `RELATIVE` relocations need no fixup until spawn/dlopen assigns a user address.
fn clone_from_cache(cached: &CachedLib) -> Option<LoadedLib> {
    let t0 = crate::clock::nanos_since_boot();

    let rw_alloc = PageAlloc::new(cached.rw_size, crate::mm::pmm::Category::Elf)?;
    // SAFETY: `rw_offset + rw_size` was validated inside `cached.alloc` when this `CachedLib` was built; `CachedLib` is immortal once cached, so `cached.alloc` is still live.
    let src = unsafe { cached.alloc.ptr().add(cached.rw_offset) };
    // SAFETY: `src` is valid for `cached.rw_size` bytes per the `SAFETY` above; `rw_alloc` is a fresh, distinct allocation, so the ranges cannot overlap.
    unsafe {
        core::ptr::copy_nonoverlapping(src, rw_alloc.ptr(), cached.rw_size);
    }

    let t1 = crate::clock::nanos_since_boot();
    let rw_delta = rw_alloc.ptr() as i64 - (cached.alloc.ptr() as i64 + cached.rw_offset as i64);
    let image = cached.snapshot.image;
    let phys_base = image.phys();

    log!(
        "dlopen: cache hit (shared), base={:#x} {}MB total, {}MB private RW, copy={}ms",
        phys_base,
        image.size() / (1024 * 1024),
        cached.rw_size / (1024 * 1024),
        (t1 - t0) / 1_000_000
    );

    Some(cached.snapshot.into_lib(
        LibMemory::Shared {
            rw_alloc,
            cached_image: image,
            rw_offset: cached.rw_offset,
            rw_delta,
        },
        UserAddr::new(phys_base),
        Some(cached.relocs.clone()),
    ))
}
