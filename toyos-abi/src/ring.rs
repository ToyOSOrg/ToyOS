//! Lock-free SPSC ring buffer for shared-memory pipes.
//!
//! Layout: a `RingHeader` followed by `capacity` bytes of data.
//!
//! The region is one page the kernel allocates and `SYS_PIPE_MAP` maps into a
//! process **writable**, so the split here is a trust boundary, not a
//! convenience: `Ring` is the kernel-side owner and holds every value the
//! copies are bounded by, in kernel memory; `RingHeader` holds only what
//! userland reads. Nothing in the header is read back by the kernel — same
//! rule `kernel/src/inbox.rs` states for its own tail.
//!
//! **The data region is never a `&[u8]` or `&mut [u8]`** — plain bytes in that
//! mapping — so [`Src`] and [`Dst`] describe the copies instead. A reference
//! over the *header* is sound and `header` takes one: one `AtomicU32`, atomics.

use core::marker::PhantomData;
use core::sync::atomic::{AtomicU32, Ordering};

pub const RING_WRITER_CLOSED: u32 = 1;
pub const RING_READER_CLOSED: u32 = 2;

/// What a process sees at offset 0 of a mapped ring.
///
/// Every field is writable by any process holding the pipe, so nothing the
/// kernel indexes, bounds or divides by may live here. `flags` is a
/// publication netd polls to notice a peer that went away; the kernel only
/// ever stores to it, and answers "is the other end gone?" from its own
/// refcounts.
#[repr(C, align(64))]
pub struct RingHeader {
    pub flags: AtomicU32,
}

impl RingHeader {
    pub fn is_writer_closed(&self) -> bool {
        self.flags.load(Ordering::Acquire) & RING_WRITER_CLOSED != 0
    }

    pub fn is_reader_closed(&self) -> bool {
        self.flags.load(Ordering::Acquire) & RING_READER_CLOSED != 0
    }
}

/// One contiguous run of a ring's data region, handed to [`Ring::read`]'s sink:
/// a pointer and a length, and no reference at all — the rule
/// `kernel::user_ptr` already states for a user buffer, for the same reason.
pub struct Src<'a> {
    ptr: *const u8,
    len: usize,
    _scope: PhantomData<&'a ()>,
}

impl Src<'_> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The run's first byte. Raw, and it stays raw: see this type's doc.
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    /// Copy the whole run into `dst`, which must be exactly as long.
    pub fn copy_to(&self, dst: &mut [u8]) {
        assert_eq!(dst.len(), self.len, "a ring run copied into a {}-byte destination", dst.len());
        // SAFETY: `Ring::read` built this run inside its own data region, and
        // `dst` is an owned `&mut [u8]` of the same length — a reference, so it
        // cannot alias the run.
        unsafe { core::ptr::copy_nonoverlapping(self.ptr, dst.as_mut_ptr(), self.len) };
    }
}

/// [`Src`] for [`Ring::write`]'s fill: the same pointer and length, in the
/// other direction, and never a `&mut [u8]` for the same reason.
pub struct Dst<'a> {
    ptr: *mut u8,
    len: usize,
    _scope: PhantomData<&'a mut ()>,
}

impl Dst<'_> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    /// Copy `src`, which must be exactly as long, over the whole run.
    pub fn copy_from(&mut self, src: &[u8]) {
        assert_eq!(src.len(), self.len, "a {}-byte source copied into a ring run", src.len());
        // SAFETY: as [`Src::copy_to`], mirrored.
        unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), self.ptr, self.len) };
    }
}

/// Kernel-side owner of one ring region: the cursors, the capacity, and the
/// base of the region they address.
///
/// Every access is serialized by the caller's lock (`kernel::pipe`'s `PIPES`),
/// which is why the cursors are plain integers rather than atomics — the
/// header's were only ever atomic because they lived in shared memory.
pub struct Ring {
    base: *mut u8,
    capacity: u32,
    write_cursor: u32,
    read_cursor: u32,
}

impl Ring {
    /// Claim `total_size` bytes at `base` as a ring, initializing the header
    /// the mapping process will see.
    ///
    /// # Safety
    /// `base` must be writable and aligned for `RingHeader`, and the region
    /// must stay valid for the life of the `Ring`.
    pub unsafe fn new(base: *mut u8, total_size: usize) -> Self {
        let capacity = total_size - core::mem::size_of::<RingHeader>();
        assert!(
            capacity > 0 && capacity < (1usize << 31),
            "ring capacity {capacity} does not leave room for the cursor modulus"
        );
        // SAFETY: the caller's contract above (`# Safety`) is exactly this
        // write's precondition — `base` writable and aligned for
        // `RingHeader` — and the `capacity` assert just above proves
        // `total_size >= size_of::<RingHeader>()`, so the write lands inside
        // the caller-promised region.
        unsafe {
            (base as *mut RingHeader).write(RingHeader {
                flags: AtomicU32::new(0),
            })
        };
        Self {
            base,
            capacity: capacity as u32,
            write_cursor: 0,
            read_cursor: 0,
        }
    }

    fn header(&self) -> &RingHeader {
        // SAFETY: `new`'s caller promised `base` writable and aligned for
        // `RingHeader` for the life of this `Ring`, and `new` wrote a valid
        // one there. The header is racily writable by any process holding
        // the pipe (its own doc comment says so), which is why its one field
        // is an `AtomicU32` accessed only through atomic ops below — never
        // read or written as a plain `u32` — so a concurrent scribble is a
        // torn or stale load, never a reference to invalid data.
        unsafe { &*(self.base as *const RingHeader) }
    }

    fn data_ptr(&self) -> *mut u8 {
        // SAFETY: `capacity = total_size - size_of::<RingHeader>()` was
        // asserted `> 0` in `new`, so this offset is strictly inside the
        // `total_size`-byte region `new`'s caller promised — never dereferenced
        // here, only computed.
        unsafe { self.base.add(core::mem::size_of::<RingHeader>()) }
    }

    /// The cursors count modulo this, and a stream byte's ring offset is its
    /// cursor value modulo `capacity`.
    ///
    /// Those two are consistent only if `capacity` divides the modulus, which
    /// is why it is `2 * capacity` and not the `u32`'s own `2^32`: `capacity`
    /// is whatever a 2 MiB page leaves after a 64-byte header, and it does not
    /// divide `2^32`. Where the modulus is not a multiple of `capacity`, the
    /// two cursor values naming the same stream byte on either side of the
    /// wrap map to *different* offsets, and an access straddling the wrap
    /// lands on the wrong bytes.
    ///
    /// Twice, rather than once, so that full stays distinguishable from empty:
    /// `available` ranges over `0..=capacity` and both ends are representable.
    fn modulus(&self) -> u64 {
        self.capacity as u64 * 2
    }

    fn advance(&self, cursor: u32, by: usize) -> u32 {
        ((cursor as u64 + by as u64) % self.modulus()) as u32
    }

    pub fn available(&self) -> u32 {
        let w = self.write_cursor as u64;
        let r = self.read_cursor as u64;
        let m = self.modulus();
        ((w + m - r) % m) as u32
    }

    pub fn space(&self) -> u32 {
        self.capacity - self.available()
    }

    /// Read up to `want` bytes, handing `sink` each contiguous run of them with
    /// its offset in the destination. Returns the number of bytes read.
    ///
    /// The run is a [`Src`] and not a `&[u8]`: the page is mapped into a
    /// process writable. One or two runs, never more: the second is the ring's
    /// own wrap.
    pub fn read(&mut self, want: usize, mut sink: impl FnMut(usize, Src<'_>)) -> usize {
        let avail = self.available() as usize;
        if avail == 0 {
            return 0;
        }
        let count = want.min(avail);
        let cap = self.capacity as usize;
        let offset = self.read_cursor as usize % cap;
        let data = self.data_ptr();

        let first = count.min(cap - offset);
        // SAFETY: `offset < cap` and `first = count.min(cap - offset)`, so
        // `data.add(offset)..+first` stays inside the `cap`-byte data region
        // `data_ptr` computed, and the wrap run `data..+(count - first)` does
        // too since `count - first <= cap`.
        let (head, tail) = unsafe { (data.add(offset), data) };
        sink(0, Src { ptr: head, len: first, _scope: PhantomData });
        if first < count {
            sink(first, Src { ptr: tail, len: count - first, _scope: PhantomData });
        }

        self.read_cursor = self.advance(self.read_cursor, count);
        count
    }

    /// Write up to `want` bytes, asking `fill` for each contiguous run of them
    /// by its offset in the source. Returns the number of bytes written.
    ///
    /// The mirror of [`Self::read`], and the same reason for [`Dst`].
    pub fn write(&mut self, want: usize, mut fill: impl FnMut(usize, Dst<'_>)) -> usize {
        let free = self.space() as usize;
        if free == 0 {
            return 0;
        }
        let count = want.min(free);
        let cap = self.capacity as usize;
        let offset = self.write_cursor as usize % cap;
        let data = self.data_ptr();

        let first = count.min(cap - offset);
        // SAFETY: the same bounds argument as `read`'s matching block.
        let (head, tail) = unsafe { (data.add(offset), data) };
        fill(0, Dst { ptr: head, len: first, _scope: PhantomData });
        if first < count {
            fill(first, Dst { ptr: tail, len: count - first, _scope: PhantomData });
        }

        self.write_cursor = self.advance(self.write_cursor, count);
        count
    }

    pub fn open_writer(&self) {
        self.header().flags.fetch_and(!RING_WRITER_CLOSED, Ordering::Release);
    }

    pub fn open_reader(&self) {
        self.header().flags.fetch_and(!RING_READER_CLOSED, Ordering::Release);
    }

    pub fn close_writer(&self) {
        self.header().flags.fetch_or(RING_WRITER_CLOSED, Ordering::Release);
    }

    pub fn close_reader(&self) {
        self.header().flags.fetch_or(RING_READER_CLOSED, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{alloc_zeroed, dealloc, Layout};

    /// The ring `kernel::pipe` builds: one 2 MiB page, header at offset 0.
    const PIPE_TOTAL: usize = 2 * 1024 * 1024;

    struct Backing {
        ptr: *mut u8,
        layout: Layout,
    }

    impl Backing {
        fn new(total: usize) -> Self {
            let layout = Layout::from_size_align(total, core::mem::align_of::<RingHeader>()).unwrap();
            // SAFETY: every caller passes a `total` (`PIPE_TOTAL` or a
            // `const TOTAL`) that is a non-zero, hard-coded byte count, so
            // `layout` has non-zero size.
            let ptr = unsafe { alloc_zeroed(layout) };
            assert!(!ptr.is_null(), "test backing allocation failed");
            Self { ptr, layout }
        }

        fn ring(&self) -> Ring {
            // SAFETY: `ptr` was just allocated above with `layout`, whose
            // alignment is `align_of::<RingHeader>()` and whose size is
            // `layout.size()` — exactly `Ring::new`'s two preconditions —
            // and `self` (and the memory it owns) outlives every `Ring` this
            // method hands out, since nothing here ever drops `self` early.
            unsafe { Ring::new(self.ptr, self.layout.size()) }
        }
    }

    impl Drop for Backing {
        fn drop(&mut self) {
            // SAFETY: `ptr`/`layout` are exactly what `new` allocated them
            // with, and this is the only place that frees them.
            unsafe { dealloc(self.ptr, self.layout) }
        }
    }

    /// The slice forms the kernel cannot have: here the destination is an
    /// ordinary allocation, so describing it buys nothing.
    fn read_slice(ring: &mut Ring, buf: &mut [u8]) -> usize {
        let want = buf.len();
        ring.read(want, |off, src| src.copy_to(&mut buf[off..off + src.len()]))
    }

    fn write_slice(ring: &mut Ring, buf: &[u8]) -> usize {
        ring.write(buf.len(), |off, mut dst| dst.copy_from(&buf[off..off + dst.len()]))
    }

    /// Byte at absolute stream position `pos`. Every aligned 16-byte group
    /// carries its own index, so a misread shows up whether it is off by a
    /// multiple of 16 (wrong stamp) or not (wrong filler).
    fn stream_byte(pos: u64) -> u8 {
        let group = (pos >> 4) as u32;
        match pos & 15 {
            k @ 0..=3 => (group >> (8 * k)) as u8,
            _ => 0xC3,
        }
    }

    /// `stream_byte` over a range, touching only the four stamped bytes of
    /// each group: the test moves gibibytes and runs unoptimised, so a
    /// per-byte closure is most of its runtime.
    fn fill(buf: &mut [u8], start: u64) {
        buf.fill(0xC3);
        let end = start + buf.len() as u64;
        let mut pos = start;
        while pos < end {
            let k = pos & 15;
            if k < 4 {
                buf[(pos - start) as usize] = stream_byte(pos);
                pos += 1;
            } else {
                pos += 16 - k;
            }
        }
    }

    /// The premise, computed rather than asserted from memory: `capacity` is
    /// whatever the header's alignment leaves of a 2 MiB page, and it does not
    /// divide the `u32` cursor space.
    #[test]
    fn the_pipe_capacity_does_not_divide_the_u32_cursor_space() {
        let header = core::mem::size_of::<RingHeader>();
        let capacity = (PIPE_TOTAL - header) as u64;
        let remainder = (1u64 << 32) % capacity;
        assert_ne!(
            remainder, 0,
            "header {header} B, capacity {capacity} B: a cursor wrapping at 2^32 \
             would be sound only if capacity divided it"
        );
        assert_eq!(
            (header, capacity, remainder),
            (64, 2_097_088, 131_072),
            "the pipe ring's shape moved — re-derive the wrap argument before \
             changing this"
        );
    }

    /// The fast gate: a read split across the ring's own wrap point must
    /// return what was written. Seeded, because reaching the wrap honestly
    /// costs the gibibytes the next test spends.
    #[test]
    fn a_read_split_across_the_cursor_wrap_returns_what_was_written() {
        let backing = Backing::new(PIPE_TOTAL);
        let mut ring = backing.ring();

        let seed = (ring.modulus() - 16) as u32;
        ring.write_cursor = seed;
        ring.read_cursor = seed;

        let sent: [u8; 32] = core::array::from_fn(|i| stream_byte(i as u64));
        assert_eq!(write_slice(&mut ring, &sent), 32);

        let mut got = [0u8; 32];
        assert_eq!(read_slice(&mut ring, &mut got[..16]), 16);
        assert_eq!(
            ring.read_cursor, 0,
            "the seed did not put the wrap between the two halves"
        );
        assert_eq!(read_slice(&mut ring, &mut got[16..]), 16);
        assert_eq!(
            got, sent,
            "a read straddling the cursor wrap did not return what was written"
        );
    }

    /// That the seeded state above is reachable at all: a long-lived pipe
    /// carries a cursor there by ordinary traffic. Slow on purpose — this is
    /// the proof the fast gate is guarding a state a real pipe reaches, and
    /// with the modulus at 2*capacity it also crosses ~33k wraps on the way.
    #[test]
    fn a_stream_survives_the_cursor_reaching_two_to_the_thirty_two() {
        const TOTAL: usize = 64 * 1024;
        let backing = Backing::new(TOTAL);
        let mut ring = backing.ring();
        let cap = ring.capacity as u64;
        let target = (1u64 << 32) + 4 * cap;

        // Coprime-ish with the capacity, so accesses straddle the buffer end
        // and the cursor wrap rather than lining up on either.
        let mut wbuf = std::vec![0u8; 4093];
        let mut rbuf = std::vec![0u8; 3571];
        let mut expect = std::vec![0u8; 3571];

        let mut written = 0u64;
        let mut read = 0u64;
        while read < target {
            while written < target {
                let n = wbuf.len().min((target - written) as usize);
                fill(&mut wbuf[..n], written);
                let put = write_slice(&mut ring, &wbuf[..n]);
                written += put as u64;
                if put < n {
                    break;
                }
            }
            let got = read_slice(&mut ring, &mut rbuf);
            assert!(got > 0, "ring stalled at {read} of {target}");
            fill(&mut expect[..got], read);
            if rbuf[..got] != expect[..got] {
                let off = (0..got).position(|i| rbuf[i] != expect[i]).unwrap();
                panic!(
                    "stream byte {} came back {:#04x}, want {:#04x} — this read \
                     spans {}..{}, and the cursor modulus is {}",
                    read + off as u64,
                    rbuf[off],
                    expect[off],
                    read,
                    read + got as u64,
                    ring.modulus(),
                );
            }
            read += got as u64;
        }
        assert_eq!(read, target);
    }

    /// The header is in the page `SYS_PIPE_MAP` maps writable. Whatever a
    /// process puts there, the stream must be unaffected — nothing the copies
    /// are bounded by is in that page.
    #[test]
    fn a_scribbled_header_leaves_the_stream_exact() {
        const TOTAL: usize = 64 * 1024;
        let backing = Backing::new(TOTAL);
        let mut ring = backing.ring();
        let mut pos = 0u64;
        // The third value is where `capacity` used to sit; zero was the
        // divisor, and 0xFF.. the 4 GiB pointer offset.
        for pattern in [0x00u8, 0xFF, 0xA5] {
            // SAFETY: `backing.ptr` is `TOTAL` bytes from `Backing::new`, and
            // `TOTAL` (64 KiB) is far larger than `size_of::<RingHeader>()`
            // (64 bytes), so the write stays inside the allocation.
            unsafe { core::ptr::write_bytes(backing.ptr, pattern, core::mem::size_of::<RingHeader>()) };
            let mut sent = std::vec![0u8; 3571];
            fill(&mut sent, pos);
            assert_eq!(write_slice(&mut ring, &sent), sent.len());
            let mut got = std::vec![0u8; sent.len()];
            assert_eq!(read_slice(&mut ring, &mut got), sent.len());
            assert_eq!(got, sent, "header pattern {pattern:#04x} changed the stream");
            pos += sent.len() as u64;
        }
    }
}
