//! Measurement only, never lands: the kernel's copy by where its two ends
//! sit, and the instruction sequences it chooses between.
//!
//! `kcopyat.read.<bytes>.d<d>`: ns per `seek` and `read` of `<bytes>` from
//! the start of a file in `/tmp`, which the kernel's tmpfs serves from page
//! frames, into a buffer `d` bytes past a page: the kernel copies with its
//! destination `d` above its source modulo 4096.
//!
//! `kcopyat.<how>.<bytes>.d<d>`: ns per copy in this process, from a page to
//! `d` above the page past its end: `movsb` (one `rep movsb`), `down` (the
//! kernel's descending copy, `kernel_copy`, a copy of its source) and `back` (`std`, `rep movsq`,
//! `cld`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::Instant;

use crate::report::{refuse, result};
use crate::sys::Mapping;

use crate::kernel_copy;

const ROUNDS: usize = 1000;
const WARMUP: usize = 100;
const DISTANCES: [usize; 19] = [0, 8, 16, 24, 31, 32, 48, 64, 96, 128, 160, 191, 192, 224, 256, 384, 512, 1024, 2048];

pub fn suite() {
    reads();
    copies();
}

fn reads() {
    let path = "/tmp/kcopyat";
    let source: Vec<u8> = (0..16384u32).map(|i| (i * 7 + i / 251) as u8).collect();
    let made = File::create(path).and_then(|mut f| f.write_all(&source));
    if let Err(e) = made {
        return refuse("kcopyat.read", e);
    }
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return refuse("kcopyat.read", e),
    };
    let map = Mapping::huge(1 << 20);
    for size in [4096usize, 8192] {
        for d in DISTANCES {
            let bench = format!("kcopyat.read.{size}.d{d}");
            // SAFETY: `d + size` is inside the mapping, which only this reaches.
            let buf = unsafe { std::slice::from_raw_parts_mut(map.ptr().add(4096 + d), size) };
            let mut ns = Vec::with_capacity(ROUNDS);
            let mut failed = None;
            for r in 0..WARMUP + ROUNDS {
                let t = Instant::now();
                if let Err(e) = file.seek(SeekFrom::Start(0)).and_then(|_| file.read_exact(buf)) {
                    failed = Some(e);
                    break;
                }
                if r >= WARMUP {
                    ns.push(t.elapsed().as_nanos() as u64);
                }
            }
            if let Some(e) = failed {
                refuse(&bench, e);
                continue;
            }
            if buf[..] != source[..size] {
                refuse(&bench, "the read differs from the file");
                continue;
            }
            result(&bench, "ns", &mut ns, &[]);
        }
    }
    let _ = std::fs::remove_file(path);
}

type CopyFn = unsafe fn(*mut u8, *const u8, usize);

unsafe fn movsb(d: *mut u8, s: *const u8, n: usize) {
    // SAFETY: as every `CopyFn`'s caller promises.
    unsafe { kernel_copy::movsb(d, s, n) }
}

unsafe fn down(d: *mut u8, s: *const u8, n: usize) {
    // SAFETY: as every `CopyFn`'s caller promises; `n` is at least 4096.
    unsafe { kernel_copy::copy_down(d, s, n) }
}

/// `n` a multiple of 8.
unsafe fn back(d: *mut u8, s: *const u8, n: usize) {
    // SAFETY: as every `CopyFn`'s caller promises; DF is set across this one `rep`.
    unsafe {
        std::arch::asm!(
            "std",
            "rep movsq",
            "cld",
            inout("rdi") d.add(n - 8) => _,
            inout("rsi") s.add(n - 8) => _,
            inout("rcx") n / 8 => _,
            options(nostack),
        );
    }
}

fn copies() {
    const MIB: usize = 1 << 20;
    let mut map = Mapping::huge(4 * MIB);
    for (i, w) in map.words()[..MIB / 8].iter_mut().enumerate() {
        *w = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    let base = map.ptr();
    let hows: [(&str, CopyFn); 3] = [("movsb", movsb), ("down", down), ("back", back)];
    for size in [4096usize, 8192, 16384, 65536] {
        for (how, copy) in hows {
            for d in DISTANCES {
                // SAFETY: both ranges lie inside the mapping, 1 MiB apart.
                let (src, dst) = unsafe { (base as *const u8, base.add(MIB + d)) };
                measure(&format!("kcopyat.{how}.{size}.d{d}"), copy, src, dst, size);
            }
        }
    }
}

fn measure(bench: &str, copy: CopyFn, src: *const u8, dst: *mut u8, size: usize) {
    // SAFETY: the destination range is `size` long and only this reaches it.
    unsafe { std::ptr::write_bytes(dst, 0xA5, size) };
    let mut ns = Vec::with_capacity(ROUNDS);
    for r in 0..WARMUP + ROUNDS {
        let t = Instant::now();
        // SAFETY: the two ranges are `size`, at least 4096, long and disjoint.
        unsafe { copy(dst, src, size) };
        if r >= WARMUP {
            ns.push(t.elapsed().as_nanos() as u64);
        }
    }
    // SAFETY: as above; read only after every copy has returned.
    if unsafe { std::slice::from_raw_parts(src, size) != std::slice::from_raw_parts(dst, size) } {
        return refuse(bench, "the copy differs from its source");
    }
    result(bench, "ns", &mut ns, &[]);
}
