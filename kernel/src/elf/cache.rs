//! The shared-object cache: one image in memory, one private writable window
//! per process.
//!
//! A cached module's read-only pages are mapped into every process that loads
//! it and its base address never moves, so its `R_X86_64_RELATIVE` relocations
//! need no rework. Only the writable window is copied.
//!
//! **Nothing is ever removed, and both refusals follow from that**, as does
//! `clone_from_cache`'s SAFETY: no address space is reachable from here, so
//! neither a changed file nor a full budget can be answered by taking an entry
//! back, and both are refused instead.

use alloc::string::String;
use alloc::vec::Vec;
use toyos_abi::syscall::SyscallError;

use super::{LibMemory, LoadedLib};
use crate::mm::{KernelSlice, MAX_HEAP_ALLOC};
use crate::process::PageAlloc;
use crate::sync::Lock;
use crate::vfs::BackingId;
use crate::UserAddr;
use toyos_elf::dynamic::InitArray;
use toyos_elf::rela::Rules;
use toyos_elf::{ImageRange, Op, RelaCounts, RelocKind, SymIndex, TlsRef};

/// A module's non-`RELATIVE` relocations, parsed and extracted once at cache
/// time, each as `(r_offset, what it names)`.
#[derive(Clone)]
pub struct CachedRelocs {
    /// `GLOB_DAT` and `JUMP_SLOT`.
    pub bind: Vec<(u64, SymIndex)>,
    pub tpoff64: Vec<(u64, TlsRef)>,
    pub tpoff32: Vec<(u64, TlsRef)>,
    /// The kernel writes a module id here; `None` names the module itself.
    pub dtpmod64: Vec<(u64, Option<SymIndex>)>,
    /// The kernel writes a TLS offset within the module here.
    pub dtpoff64: Vec<(u64, TlsRef)>,
}

// Extracts every non-`RELATIVE` entry, or `None` if it would not fit one kernel allocation.
fn prescan_relocs(lib: &LoadedLib) -> Option<CachedRelocs> {
    let counts = RelaCounts::of(lib.raw_relocations());
    let widest = core::mem::size_of::<(u64, TlsRef)>().max(core::mem::size_of::<(u64, Option<SymIndex>)>());
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
        match r.op() {
            Op::Bind(sym) => relocs.bind.push((r.offset(), sym)),
            Op::Tpoff64(t) => relocs.tpoff64.push((r.offset(), t)),
            Op::Tpoff32(t) => relocs.tpoff32.push((r.offset(), t)),
            Op::DtpMod64(sym) => relocs.dtpmod64.push((r.offset(), sym)),
            Op::DtpOff64(t) => relocs.dtpoff64.push((r.offset(), t)),
            Op::Relative(_) => {}
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
    rela: Option<KernelSlice>,
    jmprel: Option<KernelSlice>,
    gnu_hash: Option<KernelSlice>,
    rules: Rules,
    eh_frame_hdr: Option<ImageRange>,
    init_array: Option<InitArray>,
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
            rela: lib.rela,
            jmprel: lib.jmprel,
            gnu_hash: lib.gnu_hash,
            rules: lib.rules,
            eh_frame_hdr: lib.eh_frame_hdr,
            init_array: lib.init_array,
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
            rela: self.rela,
            jmprel: self.jmprel,
            gnu_hash: self.gnu_hash,
            cached_relocs,
            rules: self.rules,
            eh_frame_hdr: self.eh_frame_hdr,
            init_array: self.init_array,
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

static SO_CACHE: Lock<Vec<(String, CachedLib)>> = Lock::new(Vec::new());

/// The most physical memory every cached image may hold between them. **A policy
/// number**: nothing derives it. For scale, the largest shared object this tree
/// builds loads a span of 144,760,832 bytes — a 146,800,640-byte allocation, so
/// this admits one of those and refuses a second.
const BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// `so-cache-tiny`'s number, in reach of a guest. Only the magnitude moves.
const TINY_BUDGET_BYTES: usize = 8 * 1024 * 1024;

fn budget_bytes() -> usize {
    if crate::actuator::so_cache_tiny() {
        TINY_BUDGET_BYTES
    } else {
        BUDGET_BYTES
    }
}

/// Every cached image's allocation, summed. The caller holds the lock.
fn held_bytes(cache: &[(String, CachedLib)]) -> usize {
    cache.iter().map(|(_, c)| c.alloc.size()).sum()
}

/// What the cache holds for `path`, judged against the file `id` came from.
/// **The lookup and the publish-time recheck both ask here**, so the two cannot
/// disagree about what matches: a recheck comparing less than `id` would hand a
/// loader whose open straddled a rewrite an image of a file it never opened.
fn entry_for<'a>(
    cache: &'a [(String, CachedLib)],
    path: &str,
    id: BackingId,
) -> Result<Option<&'a CachedLib>, SyscallError> {
    let Some(idx) = cache.iter().position(|(p, _)| p == path) else {
        return Ok(None);
    };
    let entry = &cache[idx].1;
    if entry.id != id {
        return Err(SyscallError::NotSupported);
    }
    Ok(Some(entry))
}

/// Takes ownership of `lib` and returns a clone in `Shared` mode with a private writable window; returns it unchanged if it cannot be cached.
/// `Err` is the budget alone: an image that merely cannot be cached still works.
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
    // strand a whole library forever — the loser clones the winner's instead, and
    // is refused when the winner's file is not the one this caller opened.
    let published = match entry_for(&cache, path, id) {
        Err(e) => Some(Err(e)),
        Ok(Some(entry)) => Some(Ok(clone_from_cache(entry))),
        Ok(None) => None,
    };
    if let Some(outcome) = published {
        drop(cache);
        return outcome.map(|cloned| cloned.unwrap_or_else(|| owned(alloc)));
    }
    // Under the lock that publishes: two concurrent loads must not both find room.
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

/// Clone what is cached under `path`, if `id` still describes the file it came
/// from. `Ok(None)` is nothing usable — no entry, or a clone that found no
/// memory — and the caller's own load path answers it. `Err` is the refusal:
/// the file changed under an image no address space here can take back, so a
/// reload would map the library twice.
pub fn try_clone_cached(
    path: &str,
    id: BackingId,
) -> Result<Option<LoadedLib>, SyscallError> {
    let cache = SO_CACHE.lock();
    Ok(entry_for(&cache, path, id)?.and_then(clone_from_cache))
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
