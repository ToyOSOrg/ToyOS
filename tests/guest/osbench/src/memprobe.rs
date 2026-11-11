//! MEASUREMENT ONLY, never lands: the mapping rows under the probe kernel,
//! candidate short copies beside each system's `memcpy`, and the large copies
//! over 4 KiB pages and over huge pages where the system gives them.

use std::time::Instant;

use crate::ns_since;
use crate::report::result;
use crate::sys;

const MIB: usize = 1 << 20;

pub fn suite() {
    crate::mprobe::calibrate();
    for (row, run) in [
        ("mem.map_unmap_4k", crate::system::map_unmap_small as fn()),
        ("mem.mprotect.4k", crate::mem::protect),
        ("mem.munmap_part.4k", crate::mem::unmap_part),
        ("mem.munmap_touched_remote.4k", crate::mem::unmap_touched_remote),
    ] {
        crate::mprobe::begin();
        run();
        crate::mprobe::end(row);
    }
    #[cfg(target_arch = "x86_64")]
    short_copies();
    for (name, map) in [("4k", sys::map_anon_4k as fn(usize) -> *mut u8), ("2m", sys::map_anon_huge)] {
        large(name, map);
    }
}

type Copy = unsafe fn(*mut u8, *const u8, usize);

#[cfg(target_arch = "x86_64")]
mod copies {
    use core::arch::x86_64::*;

    #[inline(never)]
    pub unsafe fn libc(d: *mut u8, s: *const u8, n: usize) {
        unsafe { core::ptr::copy_nonoverlapping(s, d, n) }
    }

    #[inline(never)]
    pub unsafe fn rep_movsb(d: *mut u8, s: *const u8, n: usize) {
        unsafe {
            core::arch::asm!("rep movsb", inout("rdi") d => _, inout("rsi") s => _, inout("rcx") n => _, options(nostack));
        }
    }

    /// Whole words by `rep movsq`, then the bytes left one at a time.
    #[inline(never)]
    pub unsafe fn rep_movsq(d: *mut u8, s: *const u8, n: usize) {
        unsafe {
            core::arch::asm!(
                "rep movsq",
                "test edx, edx",
                "jz 3f",
                "2:",
                "movsb",
                "dec edx",
                "jnz 2b",
                "3:",
                inout("rdi") d => _, inout("rsi") s => _, inout("rcx") n >> 3 => _, inout("rdx") n & 7 => _,
                options(nostack),
            );
        }
    }

    /// 16 bytes at a time, the last 16 overlapping what came before; `n >= 16`.
    #[inline(never)]
    pub unsafe fn sse_loop(d: *mut u8, s: *const u8, n: usize) {
        unsafe {
            let mut i = 0;
            while i + 16 < n {
                _mm_storeu_si128(d.add(i).cast(), _mm_loadu_si128(s.add(i).cast()));
                i += 16;
            }
            _mm_storeu_si128(d.add(n - 16).cast(), _mm_loadu_si128(s.add(n - 16).cast()));
        }
    }

    /// 32 bytes at a time, the last 32 overlapping; `n >= 32`.
    #[inline(never)]
    #[target_feature(enable = "avx")]
    pub unsafe fn avx_loop(d: *mut u8, s: *const u8, n: usize) {
        unsafe {
            let mut i = 0;
            while i + 32 < n {
                _mm256_storeu_si256(d.add(i).cast(), _mm256_loadu_si256(s.add(i).cast()));
                i += 32;
            }
            _mm256_storeu_si256(d.add(n - 32).cast(), _mm256_loadu_si256(s.add(n - 32).cast()));
        }
    }

    /// What a short path in front of `rep movsb` would be: 16-byte moves to
    /// 256 bytes, the string instruction past them.
    #[inline(never)]
    pub unsafe fn short_then_rep(d: *mut u8, s: *const u8, n: usize) {
        unsafe {
            if (16..=256).contains(&n) {
                let mut i = 0;
                while i + 16 < n {
                    _mm_storeu_si128(d.add(i).cast(), _mm_loadu_si128(s.add(i).cast()));
                    i += 16;
                }
                _mm_storeu_si128(d.add(n - 16).cast(), _mm_loadu_si128(s.add(n - 16).cast()));
            } else {
                core::arch::asm!("rep movsb", inout("rdi") d => _, inout("rsi") s => _, inout("rcx") n => _, options(nostack));
            }
        }
    }
}

/// Each candidate copy at 64 B to 4 KiB between the buffers `mem.memcpy.*`
/// uses, timed as that sweep times: MB/s per round of 64 MiB, 15 after one.
#[cfg(target_arch = "x86_64")]
fn short_copies() {
    const LEN: usize = 64 * MIB;
    let mut src = sys::Mapping::huge(LEN);
    let mut dst = sys::Mapping::huge(LEN);
    for (i, w) in src.words().iter_mut().enumerate() {
        *w = i as u64;
    }
    dst.words().fill(1);
    let (s, d) = (src.ptr(), dst.ptr());
    let mut kinds: Vec<(&str, Copy)> = vec![
        ("libc", copies::libc),
        ("rep_movsb", copies::rep_movsb),
        ("rep_movsq", copies::rep_movsq),
        ("sse_loop", copies::sse_loop),
        ("short_then_rep", copies::short_then_rep),
    ];
    if std::is_x86_feature_detected!("avx") {
        kinds.push(("avx_loop", copies::avx_loop));
    }
    for size in [64usize, 128, 256, 512, 1024, 4096] {
        for &(name, copy) in &kinds {
            let copy = std::hint::black_box(copy);
            let copies = LEN / size;
            let round = || {
                let t = Instant::now();
                for _ in 0..copies {
                    // SAFETY: both mappings are `LEN` long and distinct.
                    unsafe { copy(std::hint::black_box(d), std::hint::black_box(s), std::hint::black_box(size)) };
                }
                (copies * size) as u64 * 1000 / ns_since(t).max(1)
            };
            round();
            crate::clock::settle();
            let mut mbps: Vec<u64> = (0..15).map(|_| round()).collect();
            result(&format!("memprobe.copy.{name}.{size}"), "MB/s", &mut mbps, &[]);
        }
    }
}

/// `mem.memcpy.*` from 1 MiB up and `mem.bw.copy`, between buffers `map` made.
fn large(name: &str, map: fn(usize) -> *mut u8) {
    const LEN: usize = 64 * MIB;
    let (s, d) = (map(LEN), map(LEN));
    // SAFETY: two fresh mappings of `LEN` bytes, whole words.
    let (sw, dw) = unsafe { (std::slice::from_raw_parts_mut(s.cast::<u64>(), LEN / 8), std::slice::from_raw_parts_mut(d.cast::<u64>(), LEN / 8)) };
    for (i, w) in sw.iter_mut().enumerate() {
        *w = i as u64;
    }
    dw.fill(1);
    let huge = [("huge_pages", sys::huge_backing())];
    let mut size = MIB;
    while size <= LEN {
        let copies = LEN / size;
        let round = || {
            let t = Instant::now();
            for _ in 0..copies {
                // SAFETY: both mappings are `LEN` long and distinct.
                let (from, to) = unsafe {
                    (std::slice::from_raw_parts(std::hint::black_box(s), size), std::slice::from_raw_parts_mut(std::hint::black_box(d), size))
                };
                to.copy_from_slice(from);
            }
            (copies * size) as u64 * 1000 / ns_since(t).max(1)
        };
        round();
        crate::clock::settle();
        let mut mbps: Vec<u64> = (0..15).map(|_| round()).collect();
        result(&format!("memprobe.memcpy.{name}.{size}"), "MB/s", &mut mbps, &huge);
        size *= 4;
    }
    let mut round = || {
        let t = Instant::now();
        for (d, s) in dw.iter_mut().zip(sw.iter()) {
            *d = *s ^ 0x9e37_79b9_7f4a_7c15;
        }
        std::hint::black_box(d);
        (LEN as u64) * 1000 / ns_since(t).max(1)
    };
    round();
    crate::clock::settle();
    let mut mbps: Vec<u64> = (0..7).map(|_| round()).collect();
    result(&format!("memprobe.bw.copy.{name}"), "MB/s", &mut mbps, &huge);
    // SAFETY: the whole mappings, unused from here.
    unsafe {
        sys::unmap(s, LEN);
        sys::unmap(d, LEN);
    }
}
