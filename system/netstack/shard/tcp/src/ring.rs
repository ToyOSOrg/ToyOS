//! A byte ring over storage of its own or a lent one. Its own grows, by doubling, with the
//! furthest offset written and never past the capacity, so a connection holds memory for what it
//! holds and not for what it may. A lent one another party holds and lays out: bytes are copied
//! into and out of it by stream position, counted from the first byte it was lent, never
//! borrowed from it, and a ring is lent at most once. The capacity only rises, and holds no more
//! than a lent ring holds now. Offsets count from the oldest byte held.

use alloc::boxed::Box;
use alloc::vec::Vec;

/// Storage another party holds: a ring's bytes, reached only by copies, at stream positions
/// counted from the first byte it was lent, which wrap with `usize`.
#[allow(clippy::len_without_is_empty, reason = "storage is never empty, and nothing asks")]
pub trait Lent: core::fmt::Debug {
    /// The bytes it holds now from the oldest one held on.
    fn len(&self) -> usize;
    /// The most [`Self::len`] ever rises to.
    fn most(&self) -> usize;
    /// Its holder is asked to let it hold `bytes`; [`Self::len`] rises once it does.
    fn want(&mut self, bytes: usize);
    /// Copies `data` in at position `at`; the ring holds no more than [`Self::len`] from its
    /// oldest byte.
    fn put(&mut self, at: usize, data: &[u8]);
    /// Copies `out.len()` bytes out from position `at`, as [`Self::put`] places them.
    fn take(&self, at: usize, out: &mut [u8]);
}

/// Copies the common prefix of `from` into `to`; the count is how much.
fn copy(to: &mut [u8], from: &[u8]) -> usize {
    let n = to.len().min(from.len());
    if let (Some(to), Some(from)) = (to.get_mut(..n), from.get(..n)) {
        to.copy_from_slice(from);
    }
    n
}

#[derive(Debug)]
enum Storage {
    Own(Vec<u8>),
    Lent(Box<dyn Lent>),
}

#[derive(Debug)]
pub struct Ring {
    bytes: Storage,
    capacity: usize,
    /// The oldest byte's index in its own storage, or its position in a lent one.
    head: usize,
    len: usize,
}

impl Ring {
    pub const fn new(capacity: usize) -> Self {
        Self { bytes: Storage::Own(Vec::new()), capacity, head: 0, len: 0 }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    /// The capacity, held to what a lent ring holds now.
    pub fn capacity(&self) -> usize {
        match &self.bytes {
            Storage::Own(_) => self.capacity,
            Storage::Lent(lent) => self.capacity.min(lent.len()),
        }
    }

    pub fn room(&self) -> usize {
        self.capacity().saturating_sub(self.len)
    }

    /// Raises the capacity to `capacity`, never past the most a lent ring holds, whose holder is
    /// asked for it; a lower one is no change.
    pub fn grow(&mut self, capacity: usize) {
        let capacity = match &mut self.bytes {
            Storage::Own(_) => capacity,
            Storage::Lent(lent) => {
                let capacity = capacity.min(lent.most());
                lent.want(capacity);
                capacity
            }
        };
        self.capacity = self.capacity.max(capacity);
    }

    /// The storage index of `offset`: below its own storage's length, or a lent ring's position.
    fn at(&self, offset: usize) -> usize {
        match &self.bytes {
            Storage::Own(bytes) => {
                let index = self.head.saturating_add(offset);
                index.checked_sub(bytes.len()).unwrap_or(index)
            }
            Storage::Lent(_) => self.head.wrapping_add(offset),
        }
    }

    /// Storage for every offset below `end`, at most the capacity: each byte stored keeps its
    /// offset, laid out again from the head. A lent ring has it already.
    fn reserve(&mut self, end: usize) {
        let Storage::Own(bytes) = &mut self.bytes else { return };
        if end > bytes.len() {
            bytes.rotate_left(self.head);
            self.head = 0;
            let len = end.max(bytes.len().saturating_mul(2)).min(self.capacity);
            bytes.resize(len, 0);
        }
    }

    /// Whether the ring can be lent storage of `len` bytes: it is not lent already, and
    /// stores no more.
    pub fn lendable(&self, len: usize) -> bool {
        matches!(&self.bytes, Storage::Own(bytes) if bytes.len() <= len)
    }

    /// The ring's bytes from here on are `lent`'s, at the offsets they had: everything stored is
    /// copied in, the oldest byte held at position 0, and the capacity is held to the most it
    /// holds. Only where [`Self::lendable`] says so.
    pub fn lend(&mut self, mut lent: Box<dyn Lent>) {
        let Storage::Own(bytes) = &self.bytes else { return };
        let (newer, older) = bytes.split_at(self.head.min(bytes.len()));
        lent.put(0, older);
        lent.put(older.len(), newer);
        self.capacity = self.capacity.min(lent.most());
        self.bytes = Storage::Lent(lent);
        self.head = 0;
    }

    /// Copies `data` in from `storage` index `start`, wrapping at the storage's end.
    fn put(&mut self, start: usize, data: &[u8]) -> usize {
        match &mut self.bytes {
            Storage::Own(bytes) => {
                let first = bytes.get_mut(start..).map_or(0, |to| copy(to, data));
                let (_, rest) = data.split_at(first);
                first.saturating_add(copy(bytes, rest))
            }
            Storage::Lent(lent) => {
                lent.put(start, data);
                data.len()
            }
        }
    }

    /// Stores `data` at `offset` without counting it held; what lies past the capacity is not stored.
    pub fn write_at(&mut self, offset: usize, data: &[u8]) -> usize {
        let fits = self.capacity().saturating_sub(offset).min(data.len());
        if fits == 0 {
            return 0;
        }
        self.reserve(offset.saturating_add(fits));
        let start = self.at(offset);
        let (data, _) = data.split_at(fits);
        self.put(start, data)
    }

    /// Counts `n` more bytes held: bytes [`write_at`](Self::write_at) placed at the end, or a
    /// lent ring's holder did. Answers how many fit the capacity.
    pub fn commit(&mut self, n: usize) -> usize {
        let n = n.min(self.room());
        self.len = self.len.saturating_add(n);
        n
    }

    pub fn push(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.room());
        let (data, _) = data.split_at(n);
        let written = self.write_at(self.len, data);
        self.commit(written)
    }

    pub fn consume(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = self.at(n);
        self.len = self.len.saturating_sub(n);
    }

    /// The held bytes in `[offset, offset + len)`, clamped to what is held: borrowed where they lie
    /// in one piece of a ring's own storage, and otherwise copied into `scratch`, which a lent
    /// ring's bytes always are.
    pub fn payload<'a>(&'a self, offset: usize, len: usize, scratch: &'a mut Vec<u8>) -> &'a [u8] {
        let offset = offset.min(self.len);
        let len = len.min(self.len.saturating_sub(offset));
        let start = self.at(offset);
        match &self.bytes {
            Storage::Own(bytes) if len <= bytes.len().saturating_sub(start) => bytes.get(start..start.saturating_add(len)).unwrap_or_default(),
            Storage::Own(bytes) => {
                let first = bytes.len().saturating_sub(start);
                scratch.clear();
                scratch.extend_from_slice(bytes.get(start..start.saturating_add(first)).unwrap_or_default());
                scratch.extend_from_slice(bytes.get(..len.saturating_sub(first)).unwrap_or_default());
                scratch
            }
            Storage::Lent(lent) => {
                scratch.clear();
                scratch.resize(len, 0);
                lent.take(start, scratch);
                scratch
            }
        }
    }

    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        let mut scratch = Vec::new();
        let copied = copy(out, self.payload(0, n, &mut scratch));
        self.consume(copied);
        copied
    }
}
