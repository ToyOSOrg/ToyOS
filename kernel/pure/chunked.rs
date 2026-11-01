//! A fixed-length array kept in pieces of at most [`CHUNK_BYTES`] each, so a
//! table sized by a caller is never one allocation larger than the kernel
//! heap takes, and is refused rather than aborted where the heap has no room.

#![forbid(unsafe_code)]

use alloc::vec::Vec;

/// The most bytes one piece holds, 1 MiB less 8 KiB: under half the kernel
/// heap's largest allocation (2 MiB less 4 KiB), so two pieces and their
/// allocator headers share one 2 MiB superframe and none takes one alone.
pub const CHUNK_BYTES: usize = (1 << 20) - (8 << 10);

pub struct Chunked<T> {
    chunks: Vec<Vec<T>>,
    per: usize,
    len: usize,
}

impl<T> Chunked<T> {
    /// `len` elements, each `fill()`; `None` where the heap refuses a piece,
    /// or where the list of pieces would itself pass [`CHUNK_BYTES`].
    pub fn new(len: usize, mut fill: impl FnMut() -> T) -> Option<Self> {
        let per = (CHUNK_BYTES / core::mem::size_of::<T>().max(1)).max(1);
        if len.div_ceil(per) * core::mem::size_of::<Vec<T>>() > CHUNK_BYTES {
            return None;
        }
        let mut chunks = Vec::new();
        chunks.try_reserve_exact(len.div_ceil(per)).ok()?;
        let mut left = len;
        while left > 0 {
            let n = left.min(per);
            let mut chunk = Vec::new();
            chunk.try_reserve_exact(n).ok()?;
            chunk.extend((0..n).map(|_| fill()));
            chunks.push(chunk);
            left -= n;
        }
        Some(Self { chunks, per, len })
    }

    /// The heap `len` elements take.
    pub fn heap_bytes(len: usize) -> usize {
        let per = (CHUNK_BYTES / core::mem::size_of::<T>().max(1)).max(1);
        len * core::mem::size_of::<T>() + len.div_ceil(per) * core::mem::size_of::<Vec<T>>()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.chunks.iter_mut().flatten()
    }
}

impl<T> core::ops::Index<usize> for Chunked<T> {
    type Output = T;

    fn index(&self, i: usize) -> &T {
        assert!(i < self.len, "chunked: {i} of {}", self.len);
        &self.chunks[i / self.per][i % self.per]
    }
}

impl<T> core::ops::IndexMut<usize> for Chunked<T> {
    fn index_mut(&mut self, i: usize) -> &mut T {
        assert!(i < self.len, "chunked: {i} of {}", self.len);
        &mut self.chunks[i / self.per][i % self.per]
    }
}

/// A stack of at most a fixed count, on [`Chunked`] storage made at once, so
/// a push never allocates.
pub struct Stack<T> {
    items: Chunked<T>,
    len: usize,
}

impl<T: Copy + Default> Stack<T> {
    /// Room for `capacity`; `None` where the heap refuses it.
    pub fn new(capacity: usize) -> Option<Self> {
        Some(Self { items: Chunked::new(capacity, T::default)?, len: 0 })
    }

    pub fn heap_bytes(capacity: usize) -> usize {
        Chunked::<T>::heap_bytes(capacity)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Push `item`; past the capacity is the caller's bug.
    pub fn push(&mut self, item: T) {
        assert!(self.len < self.items.len(), "stack: a push past its {}", self.items.len());
        self.items[self.len] = item;
        self.len += 1;
    }

    pub fn pop(&mut self) -> Option<T> {
        self.len = self.len.checked_sub(1)?;
        Some(self.items[self.len])
    }

    pub fn get(&self, i: usize) -> T {
        assert!(i < self.len, "stack: {i} of {}", self.len);
        self.items[i]
    }

    /// Take the item at `i`, the last moving into its place.
    pub fn swap_remove(&mut self, i: usize) -> T {
        let item = self.get(i);
        self.len -= 1;
        self.items[i] = self.items[self.len];
        item
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No piece is larger than the bound, and every element is reached.
    #[test]
    fn a_long_array_is_pieces_none_past_the_bound() {
        let len = 3 * (CHUNK_BYTES / 8) + 5;
        let mut a: Chunked<u64> = Chunked::new(len, || 7).unwrap();
        assert!(a.chunks.iter().all(|c| c.len() * 8 <= CHUNK_BYTES));
        assert_eq!(a.chunks.len(), 4);
        a[len - 1] = 9;
        assert_eq!((a[0], a[len - 1], a.len()), (7, 9, len));
        assert_eq!(a.iter_mut().count(), len);
        let pieces = CHUNK_BYTES / core::mem::size_of::<Vec<u8>>();
        assert!(Chunked::<[u8; 4096]>::new(pieces * 255 + 1, || [0; 4096]).is_none(), "a list of pieces past the bound");
    }

    #[test]
    fn a_stack_pushes_pops_and_removes_in_place() {
        let mut s: Stack<u32> = Stack::new(3).unwrap();
        s.push(1);
        s.push(2);
        s.push(3);
        assert_eq!(s.swap_remove(0), 1);
        assert_eq!((s.get(0), s.len()), (3, 2));
        assert_eq!((s.pop(), s.pop(), s.pop()), (Some(2), Some(3), None));
    }
}
