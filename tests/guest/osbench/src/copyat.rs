//! Measurement only, never lands: one `memcpy` of a buffer, timed by itself,
//! by where its two ends sit. AMD's Family 17h load/store unit judges whether
//! a load may take its bytes from an older in-flight store by address bits
//! 11:0 alone (Software Optimization Guide for AMD EPYC 7002, 56305 rev. 3.02,
//! §2.12), so a destination a short way above its source
//! modulo 4 KiB may stall each load behind a store it only appears to need.
//!
//! `copyat.<bytes>.<how>.s<a>.d<d>`: ns per copy from a source at `a` bytes
//! past a page to a destination `d` bytes above it modulo 4096, both in one
//! anonymous mapping. `<how>` is `libc` (the library's `memcpy`) or one of
//! `arch::copies`: `movsb`, `dstal` and `srcal` (`rep movsb` from the
//! destination's or the source's first 64-byte boundary), `up` and `down`
//! (SSE2, 64 bytes a step, lowest or highest first).
//! `downavx` is `down` 32 bytes a step. `copyat.<bytes>.heap`: `libc` between
//! two 64 KiB `Vec`s as the heap places them, with their offsets noted; and
//! `libc` at 1 MiB and 64 MiB, d = 0, 16 and 256, past the caches.

use std::time::Instant;

use crate::arch::copies;
use crate::report::{note, refuse, result};
use crate::sys::Mapping;

const ROUNDS: usize = 1000;
const WARMUP: usize = 100;
const MIB: usize = 1 << 20;
const DISTANCES: [usize; 16] = [0, 16, 32, 64, 256, 384, 512, 640, 768, 1024, 1536, 2048, 2560, 3072, 4032, 4080];

pub fn suite() {
    let mut map = Mapping::huge(4 * MIB);
    let base = map.ptr();
    let hows: [(&str, CopyFn); 7] = [
        ("libc", libc),
        ("movsb", copies::movsb),
        ("dstal", copies::movsb_dst_aligned),
        ("srcal", copies::movsb_src_aligned),
        ("up", copies::sse_up),
        ("down", copies::sse_down),
        ("downavx", copies::avx_down),
    ];
    for (i, w) in map.words()[..MIB / 8].iter_mut().enumerate() {
        *w = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    for size in [4096usize, 8192] {
        for (how, copy) in hows {
            if size == 4096 && how != "libc" {
                continue;
            }
            for at in [0usize, 16] {
                for d in DISTANCES {
                    // SAFETY: both ranges lie inside the 4 MiB mapping and are 1 MiB apart.
                    let (src, dst) = unsafe { (base.add(at), base.add(MIB + at + d)) };
                    measure(&format!("copyat.{size}.{how}.s{at}.d{d}"), copy, src, dst, size);
                }
            }
        }
    }
    let heap = vec![0u8; 64 * 1024];
    let mut dst = vec![0u8; 64 * 1024];
    let (s, d) = (heap.as_ptr(), dst.as_mut_ptr());
    note(
        "copyat.heap",
        &[
            ("src_page_offset", (s as usize % 4096).to_string()),
            ("dst_page_offset", (d as usize % 4096).to_string()),
            ("distance", ((d as usize).wrapping_sub(s as usize) % 4096).to_string()),
        ],
    );
    for size in [4096usize, 8192] {
        measure(&format!("copyat.{size}.heap"), libc, s, d, size);
    }
    drop(map);
    // Past the caches: 1 MiB and 64 MiB, the destination 64 MiB + 4 KiB + `d` above the source.
    let mut big = Mapping::huge(2 * LARGE + 8192);
    big.words().fill(3);
    let base = big.ptr();
    for (size, rounds) in [(MIB, 200), (LARGE, 20)] {
        for d in [0usize, 16, 256] {
            // SAFETY: both ranges lie inside the mapping and are 64 MiB + 4 KiB + `d` apart.
            let (src, dst) = unsafe { (base, base.add(LARGE + 4096 + d)) };
            measure_n(&format!("copyat.{size}.libc.s0.d{d}"), libc, src, dst, size, rounds);
        }
    }
}

const LARGE: usize = 64 * MIB;

type CopyFn = unsafe fn(*mut u8, *const u8, usize);

/// The library's `memcpy`; `black_box` keeps it a call.
unsafe fn libc(d: *mut u8, s: *const u8, n: usize) {
    // SAFETY: as every `CopyFn`'s caller promises.
    unsafe { std::ptr::copy_nonoverlapping(std::hint::black_box(s), std::hint::black_box(d), std::hint::black_box(n)) }
}

fn measure(bench: &str, copy: CopyFn, src: *const u8, dst: *mut u8, size: usize) {
    measure_n(bench, copy, src, dst, size, ROUNDS)
}

fn measure_n(bench: &str, copy: CopyFn, src: *const u8, dst: *mut u8, size: usize, rounds: usize) {
    let warmup = WARMUP.min(rounds / 10).max(1);
    // SAFETY: the destination range is `size` long and only this reaches it.
    unsafe { std::ptr::write_bytes(dst, 0xA5, size) };
    let mut ns = Vec::with_capacity(rounds);
    for r in 0..warmup + rounds {
        let t = Instant::now();
        // SAFETY: the caller's two ranges are `size` (at least 64) long and disjoint.
        unsafe { copy(dst, src, size) };
        if r >= warmup {
            ns.push(t.elapsed().as_nanos() as u64);
        }
    }
    // SAFETY: as above; read only after every copy has returned.
    if unsafe { std::slice::from_raw_parts(src, size) != std::slice::from_raw_parts(dst, size) } {
        return refuse(bench, "the copy differs from its source");
    }
    result(bench, "ns", &mut ns, &[]);
}
