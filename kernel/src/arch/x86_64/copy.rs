//! The kernel's `memcpy`, in place of `compiler_builtins`' weak one, and the
//! rule and the descending copy its user copies share (`super::uaccess`): one
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
}

/// The shortest copy that ever goes down: below it the stall is a few loads.
const LEAST: usize = 64;

impl Rule {
    /// Intel's and an unmeasured vendor's: on Tiger Lake [`copy_down`] is
    /// slower than `rep movsb` at every distance and length measured.
    pub const NEVER: Self = Self { window: 0 };

    /// Zen 2's: there, `rep movsb` waits at every distance under 32 bytes,
    /// at every length measured from 4 to 64 KiB, and at none from 32 up.
    pub const AMD: Self = Self { window: 32 };

    /// Whether a copy of `n` bytes from `src` to `dest` goes down.
    pub fn goes_down(self, dest: usize, src: usize, n: usize) -> bool {
        let d = dest.wrapping_sub(src) % 4096;
        n >= LEAST && d != 0 && d < self.window
    }
}

/// The rule `memcpy` copies by, [`Rule::NEVER`] until [`set_rule`].
#[cfg(target_os = "none")]
static WINDOW: AtomicUsize = AtomicUsize::new(0);

/// Set the rule every later copy goes by, before another CPU runs.
#[cfg(target_os = "none")]
pub fn set_rule(rule: Rule) {
    WINDOW.store(rule.window, Ordering::Relaxed);
}

#[cfg(target_os = "none")]
pub(super) fn rule() -> Rule {
    Rule { window: WINDOW.load(Ordering::Relaxed) }
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

/// The descending copy's instructions, for `rdi` the destination, `rsi` the
/// source and `rdx` the length, at least 8: it keeps those three, uses no
/// stack and leaves `rax`, `rcx`, `r8`, `r9` and `r10` changed. The top 8
/// bytes go in one store, then 32 and 8 bytes a step down from the
/// destination's last 8-byte boundary, so no other store splits a line, and
/// the bottom 8 bytes last. Stores past the boundary overlap one another,
/// with the same bytes.
macro_rules! copy_down_asm {
    () => {
        concat!(
            "lea rcx, [rdi + rdx]\n",
            "and rcx, -8\n",
            "sub rcx, rdi\n",
            "mov rax, [rsi + rdx - 8]\n",
            "mov [rdi + rdx - 8], rax\n",
            "cmp rcx, 32\n",
            "jb 3f\n",
            "2:\n",
            "sub rcx, 32\n",
            "mov rax, [rsi + rcx + 24]\n",
            "mov r8, [rsi + rcx + 16]\n",
            "mov r9, [rsi + rcx + 8]\n",
            "mov r10, [rsi + rcx]\n",
            "mov [rdi + rcx + 24], rax\n",
            "mov [rdi + rcx + 16], r8\n",
            "mov [rdi + rcx + 8], r9\n",
            "mov [rdi + rcx], r10\n",
            "cmp rcx, 32\n",
            "jae 2b\n",
            "3:\n",
            "cmp rcx, 8\n",
            "jb 5f\n",
            "4:\n",
            "sub rcx, 8\n",
            "mov rax, [rsi + rcx]\n",
            "mov [rdi + rcx], rax\n",
            "cmp rcx, 8\n",
            "jae 4b\n",
            "5:\n",
            "mov rax, [rsi]\n",
            "mov [rdi], rax\n",
        )
    };
}
#[cfg(target_os = "none")]
pub(super) use copy_down_asm;

/// Copy `n` bytes, at least 8, between disjoint ranges, highest first
/// ([`copy_down_asm`]).
///
/// # Safety
/// As [`copy_by`], with `n` at least 8.
pub unsafe fn copy_down(dest: *mut u8, src: *const u8, n: usize) {
    // SAFETY: every access is inside `[0, n)` of its range, as the caller promises.
    unsafe {
        core::arch::asm!(
            copy_down_asm!(),
            in("rdi") dest,
            in("rsi") src,
            in("rdx") n,
            out("rax") _,
            out("rcx") _,
            out("r8") _,
            out("r9") _,
            out("r10") _,
            options(nostack),
        );
    }
}
