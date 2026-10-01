//! std's C allocator, as C code in a Rust program reaches it: `aligned_alloc`
//! answers a block at a multiple of its alignment, and `realloc` and `free`
//! release each block at the layout it was allocated with. The allocator
//! under them, dlmalloc, asserts a released block's size, so a block released
//! at another layout ends this program rather than passing.

extern "C" {
    fn aligned_alloc(align: usize, size: usize) -> *mut u8;
    fn malloc(size: usize) -> *mut u8;
    fn realloc(ptr: *mut u8, size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
}

const MIB: usize = 1 << 20;

fn pattern(i: usize) -> u8 {
    (i as u8).wrapping_mul(31) ^ 0x5a
}

/// Fill `len` bytes at `block` with the pattern.
unsafe fn fill(block: *mut u8, len: usize) {
    for i in 0..len {
        unsafe { block.add(i).write(pattern(i)) };
    }
}

/// Whether `len` bytes at `block` still hold the pattern.
unsafe fn holds(block: *const u8, len: usize) -> bool {
    (0..len).all(|i| unsafe { block.add(i).read() } == pattern(i))
}

fn main() {
    let mut blocks = Vec::new();
    for align in [64usize, 4096] {
        // SAFETY: C11's aligned_alloc, with a power-of-two alignment.
        let block = unsafe { aligned_alloc(align, 100) };
        assert!(!block.is_null(), "aligned_alloc({align}, 100) answered null");
        assert_eq!(block.addr() % align, 0, "aligned_alloc({align}, 100) answered {block:p}");
        // SAFETY: 100 bytes the allocator just gave.
        unsafe { fill(block, 100) };
        println!("aligned_alloc({align}): a multiple of {align}");
        blocks.push((align, block));
    }

    // Grown past the arena its first allocation came from, so dlmalloc moves
    // it: the old block is released at the layout its header names.
    let (align, block) = blocks.pop().expect("the 4096 block");
    // SAFETY: a block aligned_alloc answered, not yet released.
    let grown = unsafe { realloc(block, MIB) };
    assert!(!grown.is_null(), "realloc of the {align} block to 1 MiB answered null");
    assert_eq!(grown.addr() % align, 0, "realloc of the {align} block answered {grown:p}");
    // SAFETY: at least 100 bytes of the block realloc answered.
    assert!(unsafe { holds(grown, 100) }, "realloc of the {align} block to 1 MiB lost its first bytes");
    println!("realloc({align} block, 1 MiB): still a multiple of {align}, its first 100 bytes carried");
    blocks.push((align, grown));

    // A block malloc answered, grown and shrunk, is released at its own too.
    // SAFETY: C's malloc and realloc, each answer checked before use.
    unsafe {
        let small = malloc(32);
        assert!(!small.is_null());
        fill(small, 32);
        let big = realloc(small, 4 * MIB);
        assert!(!big.is_null() && holds(big, 32), "realloc of a malloc block lost its bytes");
        let back = realloc(big, 16);
        assert!(!back.is_null() && holds(back, 16), "a shrinking realloc lost its bytes");
        free(back);
    }

    for (align, block) in blocks {
        // SAFETY: each a live block of this allocator.
        unsafe { free(block) };
        println!("free({align} block): released");
    }
    // SAFETY: C11's: a refusal, not a block.
    assert!(unsafe { aligned_alloc(24, 8) }.is_null(), "aligned_alloc of an alignment that is no power of two answered a block");
    println!("aligned_alloc(24): null");
}
