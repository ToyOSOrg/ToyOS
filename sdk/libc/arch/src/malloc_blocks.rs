//! C's allocator over a heap that checks it: every layout it is asked for is
//! recorded, every block is handed back at the layout it was given, and it
//! grants nothing larger than `ROOM`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::RefCell;
use std::collections::HashMap;

use crate::malloc::{self, Refusal};

const ROOM: usize = 1 << 24;

#[derive(Default)]
struct Heap {
    live: RefCell<HashMap<usize, Layout>>,
    asked: RefCell<Vec<Layout>>,
}

impl Heap {
    fn grant(&self, layout: Layout, raw: *mut u8) -> *mut u8 {
        if !raw.is_null() {
            self.live.borrow_mut().insert(raw.addr(), layout);
        }
        raw
    }

    fn take(&self, ptr: *mut u8, layout: Layout) {
        assert_eq!(self.live.borrow_mut().remove(&ptr.addr()), Some(layout), "{ptr:p} handed back at another layout");
    }

    fn asked(&self) -> Vec<Layout> {
        self.asked.take()
    }

    /// The layout of the one live allocation.
    fn only(&self) -> Layout {
        let live = self.live.borrow();
        assert_eq!(live.len(), 1, "{live:?}");
        *live.values().next().unwrap()
    }

    fn assert_empty(&self) {
        assert!(self.live.borrow().is_empty(), "leaked {:?}", self.live.borrow());
    }
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.asked.borrow_mut().push(layout);
        if layout.size() > ROOM {
            return std::ptr::null_mut();
        }
        let raw = unsafe { System.alloc(layout) };
        if !raw.is_null() {
            // What a fresh block may hold, so a block that must be zeroes is seen not to be.
            unsafe { raw.write_bytes(0xA5, layout.size()) };
        }
        self.grant(layout, raw)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.asked.borrow_mut().push(layout);
        if layout.size() > ROOM {
            return std::ptr::null_mut();
        }
        self.grant(layout, unsafe { System.alloc_zeroed(layout) })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.take(ptr, layout);
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = Layout::from_size_align(new_size, layout.align()).unwrap();
        self.asked.borrow_mut().push(new);
        assert_eq!(self.live.borrow().get(&ptr.addr()), Some(&layout), "{ptr:p} resized from another layout");
        if new_size > ROOM {
            return std::ptr::null_mut();
        }
        self.take(ptr, layout);
        self.grant(new, unsafe { System.realloc(ptr, layout, new_size) })
    }
}

fn layout(size: usize, align: usize) -> Layout {
    Layout::from_size_align(size, align).unwrap()
}

/// The largest block of `align` bytes' alignment whose allocation a `Layout`
/// holds: `align` for the header, and the whole rounded to `align` no more
/// than `isize::MAX`.
fn largest(align: usize) -> usize {
    isize::MAX as usize / align * align - align
}

#[test]
fn calloc_refuses_a_product_a_usize_cannot_hold_and_asks_the_heap_nothing() {
    let heap = Heap::default();
    let half = usize::MAX / 2;
    for (count, size) in [
        (usize::MAX, 2),
        (2, usize::MAX),
        (usize::MAX, usize::MAX),
        // Each wraps to a small product: 2, 2^32 and 1 << (BITS - 1) bytes.
        (half + 2, 2),
        ((1 << 32) + 1, 1 << 32),
        (half + 1, 3),
    ] {
        assert_eq!(malloc::calloc(&heap, count, size), Err(Refusal::NoMemory), "calloc({count:#x}, {size:#x})");
        assert_eq!(heap.asked(), [], "calloc({count:#x}, {size:#x})");
    }
    for (count, size) in [(0, usize::MAX), (usize::MAX, 0), (0, 0)] {
        assert_eq!(malloc::calloc(&heap, count, size), Ok(std::ptr::null_mut()));
        assert_eq!(heap.asked(), []);
    }
    let block = malloc::calloc(&heap, 3, 5).unwrap();
    assert_eq!(heap.asked(), [layout(16 + 15, 16)]);
    assert_eq!(unsafe { std::slice::from_raw_parts(block, 15) }, [0; 15]);
    unsafe { malloc::free(&heap, block) };
    heap.assert_empty();
}

/// Within the header of `usize::MAX`, and just past the largest allocation a
/// `Layout` holds, no size reaches the heap; the largest does, and is refused
/// for want of room.
#[test]
fn a_size_whose_header_does_not_fit_is_refused_before_the_heap() {
    let heap = Heap::default();
    let edge = largest(16);
    let past = (0..=32).map(|k| usize::MAX - k).chain((1..=32).map(|k| edge + k)).chain([isize::MAX as usize]);
    for size in past.clone() {
        assert_eq!(malloc::malloc(&heap, size), Err(Refusal::NoMemory), "malloc({size:#x})");
        assert_eq!(heap.asked(), [], "malloc({size:#x})");
        assert_eq!(malloc::aligned_alloc(&heap, 16, size), Err(Refusal::NoMemory), "aligned_alloc(16, {size:#x})");
        assert_eq!(heap.asked(), [], "aligned_alloc(16, {size:#x})");
    }
    assert_eq!(malloc::malloc(&heap, edge), Err(Refusal::NoMemory));
    assert_eq!(heap.asked(), [layout(16 + edge, 16)]);

    // A live block asked to grow past them is left as it was.
    let block = malloc::malloc(&heap, 8).unwrap();
    unsafe { block.write_bytes(0x5A, 8) };
    heap.asked();
    for size in past {
        assert_eq!(unsafe { malloc::realloc(&heap, block, size) }, Err(Refusal::NoMemory), "realloc to {size:#x}");
        assert_eq!(heap.asked(), [], "realloc to {size:#x}");
    }
    assert_eq!(heap.only(), layout(16 + 8, 16));
    assert_eq!(unsafe { std::slice::from_raw_parts(block, 8) }, [0x5A; 8]);
    unsafe { malloc::free(&heap, block) };
    heap.assert_empty();
}

#[test]
fn an_alignment_is_a_power_of_two_the_heap_is_asked_for_at_least_sixteen() {
    let heap = Heap::default();
    for align in [0, 3, 24, 48, usize::MAX] {
        assert_eq!(malloc::aligned_alloc(&heap, align, 8), Err(Refusal::Alignment), "aligned_alloc({align:#x}, 8)");
        assert_eq!(heap.asked(), []);
    }
    for (align, size) in [(1, 8), (2, 1), (8, 0), (16, 100), (32, 1), (4096, 4096), (1 << 21, 3)] {
        let block = malloc::aligned_alloc(&heap, align, size).unwrap();
        let at = align.max(16);
        assert_eq!(block.addr() % at, 0, "aligned_alloc({align:#x}, {size})");
        assert_eq!(heap.only(), layout(at + size, at), "aligned_alloc({align:#x}, {size})");
        unsafe { malloc::free(&heap, block) };
        heap.assert_empty();
    }
    heap.asked();
    // Alignments a `Layout` cannot hold with a header in front.
    for align in [1 << (usize::BITS - 2), 1 << (usize::BITS - 1)] {
        assert_eq!(malloc::aligned_alloc(&heap, align, 1), Err(Refusal::NoMemory), "aligned_alloc({align:#x}, 1)");
        assert_eq!(heap.asked(), []);
    }
    let edge = largest(4096);
    assert_eq!(malloc::aligned_alloc(&heap, 4096, edge + 1), Err(Refusal::NoMemory));
    assert_eq!(heap.asked(), []);
    assert_eq!(malloc::aligned_alloc(&heap, 4096, edge), Err(Refusal::NoMemory));
    assert_eq!(heap.asked(), [layout(4096 + edge, 4096)]);
}

#[test]
fn realloc_keeps_a_blocks_alignment_and_its_bytes() {
    let heap = Heap::default();
    for align in [16, 64, 4096, 1 << 16] {
        let block = malloc::aligned_alloc(&heap, align, 100).unwrap();
        for i in 0..100 {
            unsafe { block.add(i).write(i as u8) };
        }
        let mut block = block;
        let mut kept = 100;
        for size in [10_000, 50, 70_000] {
            block = unsafe { malloc::realloc(&heap, block, size) }.unwrap();
            assert_eq!(block.addr() % align, 0, "align {align:#x} to {size}");
            assert_eq!(heap.only(), layout(align + size, align), "align {align:#x} to {size}");
            kept = kept.min(size);
            let bytes = unsafe { std::slice::from_raw_parts(block, kept) };
            assert!(bytes.iter().enumerate().all(|(i, &b)| b == i as u8), "align {align:#x} to {size}");
        }
        assert_eq!(unsafe { malloc::realloc(&heap, block, 0) }, Ok(std::ptr::null_mut()));
        heap.assert_empty();
    }
    let block = unsafe { malloc::realloc(&heap, std::ptr::null_mut(), 24) }.unwrap();
    assert_eq!(heap.only(), layout(16 + 24, 16));
    unsafe { malloc::free(&heap, block) };
    unsafe { malloc::free(&heap, std::ptr::null_mut()) };
    assert_eq!(malloc::malloc(&heap, 0), Ok(std::ptr::null_mut()));
    heap.assert_empty();
}
