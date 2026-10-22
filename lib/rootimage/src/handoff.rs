//! Whether an extent the loader hands the kernel is memory the kernel may
//! keep: whole blocks, every byte of it inside one descriptor of the type the
//! loader allocated it as, and no byte of it another handed extent's. The
//! kernel then keeps it out of its allocator by address, as it keeps the black
//! box's page.

/// `[start, end)` of UEFI memory type `ty`, as a memory map describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub ty: u32,
    pub start: u64,
    pub end: u64,
}

/// `[at, at + len)`, when `at` and `len` are whole `block`s and the extent lies
/// inside one descriptor in `map` of type `ty`; `None` otherwise.
///
/// # Panics
/// When `block` is zero: that is the caller's constant, not the loader's word.
pub fn held(
    map: impl IntoIterator<Item = Descriptor>,
    ty: u32,
    at: u64,
    len: u64,
    block: u64,
) -> Option<core::ops::Range<u64>> {
    assert!(block != 0, "a zero-byte block");
    let end = at.checked_add(len)?;
    if !at.is_multiple_of(block) || !len.is_multiple_of(block) {
        return None;
    }
    map.into_iter()
        .any(|entry| entry.ty == ty && entry.start <= at && end <= entry.end)
        .then_some(at..end)
}

/// The first two of `extents` that share a byte, by index; `None` when no two
/// do. An empty extent shares nothing.
pub fn overlapping(extents: &[core::ops::Range<u64>]) -> Option<(usize, usize)> {
    let shares = |a: &core::ops::Range<u64>, b: &core::ops::Range<u64>| {
        !a.is_empty() && !b.is_empty() && a.start < b.end && b.start < a.end
    };
    (0..extents.len()).find_map(|i| (i + 1..extents.len()).find(|&j| shares(&extents[i], &extents[j])).map(|j| (i, j)))
}
