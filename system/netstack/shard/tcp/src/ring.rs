//! A byte ring whose storage grows, by doubling, with the furthest offset written and never past
//! the capacity, so a connection holds memory for what it holds and not for what it may. The
//! capacity only rises. Offsets count from the oldest byte held.

use alloc::vec::Vec;

/// Copies the common prefix of `from` into `to`; the count is how much.
fn copy(to: &mut [u8], from: &[u8]) -> usize {
    let n = to.len().min(from.len());
    if let (Some(to), Some(from)) = (to.get_mut(..n), from.get(..n)) {
        to.copy_from_slice(from);
    }
    n
}

#[derive(Debug)]
pub struct Ring {
    bytes: Vec<u8>,
    capacity: usize,
    head: usize,
    len: usize,
}

impl Ring {
    pub const fn new(capacity: usize) -> Self {
        Self { bytes: Vec::new(), capacity, head: 0, len: 0 }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    pub const fn room(&self) -> usize {
        self.capacity.saturating_sub(self.len)
    }

    /// Raises the capacity to `capacity`; a lower one is no change.
    pub fn grow(&mut self, capacity: usize) {
        self.capacity = self.capacity.max(capacity);
    }

    /// The physical index of `offset`, which is below the storage's length.
    fn at(&self, offset: usize) -> usize {
        let index = self.head.saturating_add(offset);
        index.checked_sub(self.bytes.len()).unwrap_or(index)
    }

    /// Storage for every offset below `end`, at most the capacity: each byte stored keeps its
    /// offset, laid out again from the head.
    fn reserve(&mut self, end: usize) {
        if end > self.bytes.len() {
            self.bytes.rotate_left(self.head);
            self.head = 0;
            let len = end.max(self.bytes.len().saturating_mul(2)).min(self.capacity);
            self.bytes.resize(len, 0);
        }
    }

    /// Stores `data` at `offset` without counting it held; what lies past the capacity is not stored.
    pub fn write_at(&mut self, offset: usize, data: &[u8]) -> usize {
        #[cfg(feature = "prof")]
        let t0 = toyos_net_wire::prof::now();
        let n = self.write_at_unprofiled(offset, data);
        #[cfg(feature = "prof")]
        {
            toyos_net_wire::prof::since(toyos_net_wire::prof::Slot::CyRingWrite, t0);
            toyos_net_wire::prof::add(toyos_net_wire::prof::Slot::RingWriteBytes, n as u64);
        }
        n
    }

    fn write_at_unprofiled(&mut self, offset: usize, data: &[u8]) -> usize {
        let fits = self.capacity.saturating_sub(offset).min(data.len());
        if fits == 0 {
            return 0;
        }
        self.reserve(offset.saturating_add(fits));
        let start = self.at(offset);
        let (data, _) = data.split_at(fits);
        let first = self.bytes.get_mut(start..).map_or(0, |to| copy(to, data));
        let (_, rest) = data.split_at(first);
        let second = copy(&mut self.bytes, rest);
        first.saturating_add(second)
    }

    /// Counts `n` more bytes held: bytes [`write_at`](Self::write_at) placed at the end.
    pub fn commit(&mut self, n: usize) {
        self.len = self.len.saturating_add(n).min(self.capacity);
    }

    pub fn push(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.room());
        let (data, _) = data.split_at(n);
        let written = self.write_at(self.len, data);
        self.commit(written);
        written
    }

    pub fn consume(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = self.at(n);
        self.len = self.len.saturating_sub(n);
    }

    /// The held bytes in `[offset, offset + len)`, clamped to what is held, in at most two parts.
    pub fn slices(&self, offset: usize, len: usize) -> (&[u8], &[u8]) {
        let offset = offset.min(self.len);
        let len = len.min(self.len.saturating_sub(offset));
        let start = self.at(offset);
        let first_len = len.min(self.bytes.len().saturating_sub(start));
        let first = self.bytes.get(start..start.saturating_add(first_len)).unwrap_or_default();
        let second = self.bytes.get(..len.saturating_sub(first_len)).unwrap_or_default();
        (first, second)
    }

    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let (first, second) = self.slices(0, out.len());
        let n = copy(out, first);
        let n = n.saturating_add(out.get_mut(n..).map_or(0, |out| copy(out, second)));
        self.consume(n);
        n
    }
}
