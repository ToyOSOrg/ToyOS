//! The kernel's `memcpy`, in place of `compiler_builtins`' weak one: one
//! `rep movsb`, or a descending copy where a forward one would stall.
//!
//! A load matching an older in-flight store on address bits 11:0 alone waits
//! on it: AMD's Family 17h judges store-to-load forwarding by those bits (56305
//! rev. 3.02, §2.12), and Intel calls it 4K aliasing (Intel 64 and IA-32
//! Architectures Optimization Reference Manual). A forward copy whose
//! destination is `d` bytes above its source modulo 4 KiB, `0 < d < n`, loads
//! each byte `d` bytes after storing one with the same bits; copied highest
//! first, the nearest older store with those bits is 4 KiB minus `d` back.
//! The kernel touches no vector register (`build/fpgate.rs`), so the
//! descending copy moves 8 bytes a store.
//!
//! The file uses nothing but `core`, so `kernel-copies` tests it on the host;
//! only the freestanding kernel exports the symbol.

#[cfg(target_os = "none")]
use core::sync::atomic::{AtomicUsize, Ordering};

/// Which copies go highest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Distances below this, modulo 4 KiB, go down.
    pub window: usize,
    /// The longest copy that does.
    pub most: usize,
}

/// The shortest copy that ever goes down: below it the stall is a few loads.
const LEAST: usize = 64;

impl Rule {
    pub const NEVER: Self = Self { window: 0, most: 0 };

    /// Zen 2 loads and stores 32 bytes at a time (56305 §2.12), and only a
    /// load overlapping the store before it in part waits, at every length.
    pub const AMD: Self = Self { window: 32, most: usize::MAX };

    /// Tiger Lake's loads wait on stores hundreds of bytes back, less the
    /// farther; [`copy_down`], a store a cycle, beats `rep movsb` under 192.
    /// Past its L1 cache the wait fades, and the fast string, which need not
    /// read a line it writes whole, outruns [`copy_down`].
    pub const INTEL: Self = Self { window: 192, most: 16 * 1024 };

    /// Whether a copy of `n` bytes from `src` to `dest` goes down.
    pub fn goes_down(self, dest: usize, src: usize, n: usize) -> bool {
        let d = dest.wrapping_sub(src) % 4096;
        (LEAST..=self.most).contains(&n) && d != 0 && d < self.window.min(n)
    }
}

/// The rule `memcpy` copies by, [`Rule::NEVER`] until [`set_rule`].
#[cfg(target_os = "none")]
static WINDOW: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_os = "none")]
static MOST: AtomicUsize = AtomicUsize::new(0);

/// Set the rule every later copy goes by, before another CPU runs.
#[cfg(target_os = "none")]
pub fn set_rule(rule: Rule) {
    WINDOW.store(rule.window, Ordering::Relaxed);
    MOST.store(rule.most, Ordering::Relaxed);
}

#[cfg(target_os = "none")]
fn rule() -> Rule {
    Rule { window: WINDOW.load(Ordering::Relaxed), most: MOST.load(Ordering::Relaxed) }
}

/// Copy `n` bytes from `src` to `dest` as `rule` says.
///
/// # Safety
/// `src` is readable and `dest` writable for `n` bytes, and the two ranges
/// are disjoint or identical, as LLVM's `memcpy` allows.
#[inline(always)]
pub unsafe fn copy_by(dest: *mut u8, src: *const u8, n: usize, rule: Rule) {
    // SAFETY: as this function's caller promises; `goes_down` holds `n` to at least 8.
    unsafe {
        if rule.goes_down(dest as usize, src as usize, n) {
            copy_down(dest, src, n);
        } else {
            movsb(dest, src, n);
        }
    }
}

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
unsafe extern "C" fn memcpy(dest: *mut core::ffi::c_void, src: *const core::ffi::c_void, n: usize) -> *mut core::ffi::c_void {
    // SAFETY: `memcpy`'s contract is `copy_by`'s.
    unsafe { copy_by(dest.cast(), src.cast(), n, rule()) };
    dest
}

/// Copy `n` bytes, lowest first.
///
/// # Safety
/// As [`copy_by`].
#[inline(always)]
pub unsafe fn movsb(dest: *mut u8, src: *const u8, n: usize) {
    // SAFETY: as this function's caller promises; every Ring 0 entry clears DF.
    unsafe {
        core::arch::asm!(
            "rep movsb",
            inout("rdi") dest => _,
            inout("rsi") src => _,
            inout("rcx") n => _,
            options(nostack, preserves_flags),
        );
    }
}

/// Copy `n` bytes, at least 8, between disjoint ranges, highest first: the
/// top 8 bytes in one store, then down 32 and 8 bytes a step from the
/// destination's last 8-byte boundary, so no other store splits a line, and
/// the bottom 8 bytes last. Stores past the boundary overlap one another,
/// with the same bytes.
///
/// # Safety
/// As [`copy_by`], with `n` at least 8.
pub unsafe fn copy_down(dest: *mut u8, src: *const u8, n: usize) {
    let aligned_end = ((dest as usize).wrapping_add(n) & !7).wrapping_sub(dest as usize);
    // SAFETY: every access is inside `[0, n)` of its range, as the caller promises.
    unsafe {
        core::arch::asm!(
            "mov {a}, [{s} + {n} - 8]",
            "mov [{d} + {n} - 8], {a}",
            "cmp {p}, 32",
            "jb 3f",
            "2:",
            "sub {p}, 32",
            "mov {a}, [{s} + {p} + 24]",
            "mov {b}, [{s} + {p} + 16]",
            "mov {c}, [{s} + {p} + 8]",
            "mov {e}, [{s} + {p}]",
            "mov [{d} + {p} + 24], {a}",
            "mov [{d} + {p} + 16], {b}",
            "mov [{d} + {p} + 8], {c}",
            "mov [{d} + {p}], {e}",
            "cmp {p}, 32",
            "jae 2b",
            "3:",
            "cmp {p}, 8",
            "jb 5f",
            "4:",
            "sub {p}, 8",
            "mov {a}, [{s} + {p}]",
            "mov [{d} + {p}], {a}",
            "cmp {p}, 8",
            "jae 4b",
            "5:",
            "mov {a}, [{s}]",
            "mov [{d}], {a}",
            s = in(reg) src,
            d = in(reg) dest,
            n = in(reg) n,
            p = inout(reg) aligned_end => _,
            a = out(reg) _,
            b = out(reg) _,
            c = out(reg) _,
            e = out(reg) _,
            options(nostack),
        );
    }
}
