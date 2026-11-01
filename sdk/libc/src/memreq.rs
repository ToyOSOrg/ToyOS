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
/// unmaps for it. `None` for no bytes, and for a length whose pages a `usize`
/// cannot count.
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

/// `MADV_DONTNEED`, which Linux's `madvise`, the interface this one
/// implements, answers by discarding the range, so that it reads back as the
/// file or as zeros.
const MADV_DONTNEED: i32 = 4;

/// What Linux's `madvise` does with one of its advice values.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Advice {
    /// A hint, which its man page, `madvise(2)`, lets a kernel ignore.
    Hint,
    /// `MADV_DONTNEED`: the range's pages are discarded.
    Discard,
}

/// What `madvise` of `advice` at `addr` does; `None` (`EINVAL`) for an
/// address off a page, or advice other than the five `posix_madvise` shares
/// with it.
pub(crate) fn madvise(addr: usize, advice: i32) -> Option<Advice> {
    if !addr.is_multiple_of(PAGE) || !is_advice(advice) {
        return None;
    }
    Some(if advice == MADV_DONTNEED { Advice::Discard } else { Advice::Hint })
}

/// What a `PROT_*` word asks of pages.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Access {
    None,
    Read,
    /// Writing, which on every machine ToyOS runs on brings reading with it.
    ReadWrite,
    /// Executing, which no call maps yet: `ENOTSUP`.
    Exec,
}

/// What `prot` asks; `None` (`EINVAL`) for a bit `PROT_*` does not define.
pub(crate) fn access(prot: i32) -> Option<Access> {
    if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return None;
    }
    Some(if prot & PROT_EXEC != 0 {
        Access::Exec
    } else if prot & PROT_WRITE != 0 {
        Access::ReadWrite
    } else if prot & PROT_READ != 0 {
        Access::Read
    } else {
        Access::None
    })
}
