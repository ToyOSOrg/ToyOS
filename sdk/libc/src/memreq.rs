//! What libc's memory calls refuse before the kernel or the allocator is
//! asked. It reads nothing but what it is handed, so the host tests it
//! (`toyos-libc-copies`).

pub(crate) const PROT_READ: i32 = 0x1;
pub(crate) const PROT_WRITE: i32 = 0x2;
pub(crate) const PROT_EXEC: i32 = 0x4;
pub(crate) const MAP_SHARED: i32 = 0x01;
pub(crate) const MAP_PRIVATE: i32 = 0x02;
pub(crate) const MAP_FIXED: i32 = 0x10;
pub(crate) const MAP_ANONYMOUS: i32 = 0x20;

/// The page `sysconf(_SC_PAGESIZE)` answers.
pub(crate) const PAGE: usize = 4096;

/// `len` as the whole pages it reaches into, which is what POSIX maps and
/// unmaps for it. `mmap` asks the kernel for this length and `munmap` names
/// the mapping by it, the kernel taking only the length it was asked for: so
/// a `len` that ends in a mapping's last page names all of it, and one that
/// ends short of that page names a part and is refused. `None` for no bytes,
/// and for a length whose pages a `usize` cannot count.
pub(crate) fn whole_pages(len: usize) -> Option<usize> {
    len.checked_next_multiple_of(PAGE).filter(|&bytes| bytes != 0)
}

/// Why `mmap` cannot give what it was asked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MapRefusal {
    /// No bytes, a fixed place off a page boundary, or not exactly one of
    /// `MAP_SHARED` and `MAP_PRIVATE`: `EINVAL`.
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
    let disposition = flags & (MAP_SHARED | MAP_PRIVATE);
    if len == 0
        || (flags & MAP_FIXED != 0 && !addr.is_multiple_of(PAGE))
        || !(disposition == MAP_SHARED || disposition == MAP_PRIVATE)
    {
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

/// Whether `advice` is one of `posix_madvise`'s five, which `madvise` numbers alike.
pub(crate) fn is_advice(advice: i32) -> bool {
    (0..=4).contains(&advice)
}

/// `MADV_DONTNEED`, which Linux's `madvise` answers by discarding the range,
/// so that it reads back as the file or as zeros.
const MADV_DONTNEED: i32 = 4;

/// Why `madvise` refuses `advice` at `addr`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AdviceRefusal {
    /// An address off a page, or advice other than the five `posix_madvise`
    /// shares with it: `EINVAL`.
    Invalid,
    /// `MADV_DONTNEED`'s discard, which nothing here can do: `ENOSYS`.
    Discard,
}

/// Why Linux's `madvise` of `advice` at `addr` is refused, if it is. Its four
/// hints are taken, as its manual lets a kernel ignore each.
pub(crate) fn madvise_refusal(addr: usize, advice: i32) -> Option<AdviceRefusal> {
    if !addr.is_multiple_of(PAGE) || !is_advice(advice) {
        return Some(AdviceRefusal::Invalid);
    }
    (advice == MADV_DONTNEED).then_some(AdviceRefusal::Discard)
}
