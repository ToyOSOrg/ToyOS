//! x86-64: the entry, the copies and the fill, and SSE2's square roots.

use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Entry point for C programs, on an empty stack. The image relocates itself
/// first, from its own header's address.
#[cfg(not(feature = "std-runtime"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "lea rdi, [rip + __ehdr_start]",
        "call {relocate}",
        "call {start_c}",
        "ud2",
        relocate = sym toyos::relocate::relocate_self,
        start_c = sym crate::runtime::start_c,
    );
}

/// Copy `n` bytes from `src` to `dest`, which may overlap `src` from below,
/// and answer `dest`.
#[inline(always)]
pub(crate) unsafe fn copy_forward(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    let moves_below = MOVES_BELOW.load(Relaxed);
    if n < moves_below {
        if moves_below == UNASKED {
            // SAFETY: the caller's.
            return unsafe { first_copy(dest, src, n) };
        }
        // SAFETY: the caller's, and `n` is at most `SHORT`.
        unsafe { copy_short(dest, src, n) };
    } else {
        unsafe {
            core::arch::asm!(
                "rep movsb",
                inout("rdi") dest => _,
                inout("rsi") src => _,
                inout("rcx") n => _,
                options(nostack),
            );
        }
    }
    dest
}

/// The longest copy [`copy_short`] makes where a short `rep movsb` is slow.
const SHORT: usize = 256;

/// Copies shorter than this are [`copy_short`]'s, longer ones one `rep movsb`:
/// 0 where the CPU declares fast short `rep movsb`, CPUID.(EAX=7,ECX=0):EDX[4]
/// (FSRM; Intel SDM vol. 2A and AMD APM vol. 3, `CPUID`), its vendor's
/// statement that a short one starts fast; past [`SHORT`] elsewhere; and
/// [`UNASKED`] until a process's first copy asks.
static MOVES_BELOW: AtomicUsize = AtomicUsize::new(UNASKED);
const UNASKED: usize = usize::MAX;

/// A process's first copy: out of line, so that no copy after it pays for the
/// call.
#[cold]
#[inline(never)]
unsafe fn first_copy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    use core::arch::x86_64::{__cpuid, __cpuid_count};
    let fsrm = __cpuid(0).eax >= 7 && __cpuid_count(7, 0).edx & (1 << 4) != 0;
    MOVES_BELOW.store(if fsrm { 0 } else { SHORT + 1 }, Relaxed);
    // SAFETY: the caller's.
    unsafe { copy_forward(dest, src, n) }
}

/// `n` bytes, `16 * k <= n <= 32 * k` for the `k` indices given, as `k`
/// 16-byte moves from each end. Spelled out rather than looped: a loop over
/// them is a copy the compiler may lower to a call to `memcpy`, which this is.
/// Eight from each end are every SSE register.
macro_rules! vectors {
    ($d:ident, $s:ident, $n:ident, $($i:tt)+) => {{
        use core::arch::x86_64::{_mm_loadu_si128 as load, _mm_storeu_si128 as store};
        let t = $n - 16 * [$($i),+].len();
        let head = ($(load($s.add(16 * $i).cast()),)+);
        let tail = ($(load($s.add(t + 16 * $i).cast()),)+);
        $(store($d.add(16 * $i).cast(), head.$i);)+
        $(store($d.add(t + 16 * $i).cast(), tail.$i);)+
    }};
}

/// Copy `n` bytes, at most [`SHORT`], as moves from each end that overlap in
/// the middle: every byte is loaded before any is stored, so any overlap of
/// `dest` and `src` is copied right.
#[inline(always)]
pub(crate) unsafe fn copy_short(dest: *mut u8, src: *const u8, n: usize) {
    // SAFETY: each arm reads and writes inside the `n` bytes, by its range.
    unsafe {
        match n {
            0 => {}
            1 => dest.write(src.read()),
            2..=3 => ends::<u16>(dest, src, n),
            4..=7 => ends::<u32>(dest, src, n),
            8..=15 => ends::<u64>(dest, src, n),
            16..=32 => vectors!(dest, src, n, 0),
            33..=64 => vectors!(dest, src, n, 0 1),
            65..=128 => vectors!(dest, src, n, 0 1 2 3),
            _ => vectors!(dest, src, n, 0 1 2 3 4 5 6 7),
        }
    }
}

/// `n` bytes, `size_of::<T>() <= n <= 2 * size_of::<T>()`, as one `T` from
/// each end.
#[inline(always)]
unsafe fn ends<T>(dest: *mut u8, src: *const u8, n: usize) {
    let tail = n - core::mem::size_of::<T>();
    unsafe {
        let (a, b) = (src.cast::<T>().read_unaligned(), src.add(tail).cast::<T>().read_unaligned());
        dest.cast::<T>().write_unaligned(a);
        dest.add(tail).cast::<T>().write_unaligned(b);
    }
}

/// Copy `n` bytes from `src` to `dest`, highest address first: the order a
/// `dest` overlapping `src` from above needs.
pub(crate) unsafe fn copy_backward(dest: *mut u8, src: *const u8, n: usize) {
    if n == 0 {
        return;
    }
    // The direction flag is set only across this one `rep`; every Ring 0 entry
    // clears it for the kernel's own sake.
    unsafe {
        core::arch::asm!(
            "std",
            "rep movsb",
            "cld",
            inout("rdi") dest.add(n - 1) => _,
            inout("rsi") src.add(n - 1) => _,
            inout("rcx") n => _,
            options(nostack),
        );
    }
}

/// Set `n` bytes at `dest` to `byte`.
pub(crate) unsafe fn fill(dest: *mut u8, byte: u8, n: usize) {
    unsafe {
        core::arch::asm!(
            "rep stosb",
            inout("rdi") dest => _,
            in("al") byte,
            inout("rcx") n => _,
            options(nostack),
        );
    }
}

pub(crate) fn sqrt_f64(x: f64) -> f64 {
    let result: f64;
    unsafe { core::arch::asm!("sqrtsd {0}, {0}", inout(xmm_reg) x => result, options(pure, nomem, nostack)) };
    result
}

pub(crate) fn sqrt_f32(x: f32) -> f32 {
    let result: f32;
    unsafe { core::arch::asm!("sqrtss {0}, {0}", inout(xmm_reg) x => result, options(pure, nomem, nostack)) };
    result
}

/// `wchar_t`, which is `int` here.
pub(crate) type WChar = i32;

// What the `long double` readers below widen, named as C names them, since
// this module is also compiled on its own (`toyos-libc-copies`).
unsafe extern "C" {
    fn strtod(s: *const u8, endptr: *mut *mut u8) -> f64;
    fn wcstod(s: *const WChar, endptr: *mut *mut WChar) -> f64;
}

/// `strtod`'s number as `long double`, x87 extended precision in `st0`: read
/// to `double`'s precision and widened, which is exact.
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn strtold() {
    core::arch::naked_asm!(
        "sub rsp, 8",
        "call {strtod}",
        "movsd qword ptr [rsp], xmm0",
        "fld qword ptr [rsp]",
        "add rsp, 8",
        "ret",
        strtod = sym strtod,
    );
}

/// `strtold`, in the one locale there is.
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn strtold_l() {
    core::arch::naked_asm!("jmp {strtold}", strtold = sym strtold);
}

/// `wcstod`'s number as `long double`, as [`strtold`] widens `strtod`'s.
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn wcstold() {
    core::arch::naked_asm!(
        "sub rsp, 8",
        "call {wcstod}",
        "movsd qword ptr [rsp], xmm0",
        "fld qword ptr [rsp]",
        "add rsp, 8",
        "ret",
        wcstod = sym wcstod,
    );
}
