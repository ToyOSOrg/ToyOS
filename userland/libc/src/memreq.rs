//! What libc's memory calls refuse before the kernel or the allocator is
//! asked. It reads nothing but what it is handed, so the host tests it
//! (`toyos-libc-copies`).

pub(crate) const PROT_READ: i32 = 0x1;
pub(crate) const PROT_WRITE: i32 = 0x2;
pub(crate) const PROT_EXEC: i32 = 0x4;
pub(crate) const MAP_FIXED: i32 = 0x10;
pub(crate) const MAP_ANONYMOUS: i32 = 0x20;

/// The page `sysconf(_SC_PAGESIZE)` answers.
const PAGE: usize = 4096;

/// Why `mmap` cannot give what it was asked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MapRefusal {
    /// No bytes, or a fixed place off a page boundary: `EINVAL`.
    Invalid,
    /// A file's bytes, which the kernel maps for no process: `ENODEV`.
    File,
    /// Executable memory, which the kernel does not map: `ENOTSUP`.
    Exec,
}

/// Why `mmap` of `len` bytes at `addr` with `prot` and `flags` is refused, if
/// it is. An anonymous `MAP_SHARED` is not: with no `fork`, no other process
/// can map it, so a private mapping is all that sharing it could mean.
pub(crate) fn mmap_refusal(addr: usize, len: usize, prot: i32, flags: i32) -> Option<MapRefusal> {
    if len == 0 || (flags & MAP_FIXED != 0 && !addr.is_multiple_of(PAGE)) {
        return Some(MapRefusal::Invalid);
    }
    if flags & MAP_ANONYMOUS == 0 {
        return Some(MapRefusal::File);
    }
    if prot & PROT_EXEC != 0 {
        return Some(MapRefusal::Exec);
    }
    None
}

/// Whether `advice` is one of `posix_madvise`'s five.
pub(crate) fn is_advice(advice: i32) -> bool {
    (0..=4).contains(&advice)
}

/// Whether `aligned_alloc` beside std can align to `align`: std's `malloc`
/// aligns every block to 16, and its `free` knows no other alignment.
#[cfg(feature = "std-runtime")]
pub(crate) fn std_aligns(align: usize) -> bool {
    align.is_power_of_two() && align <= 16
}
