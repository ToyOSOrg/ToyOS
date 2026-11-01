// Miscellaneous POSIX/C functions: process, signals, sysconf.

use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use toyos_abi::syscall;

use crate::sigreq::SigAction;
use crate::errcode::ENOSYS;
use crate::strtonum;

// Process

#[no_mangle]
pub unsafe extern "C" fn getpid() -> i32 {
    syscall::getpid().0 as i32
}

#[no_mangle]
pub unsafe extern "C" fn getppid() -> i32 {
    0 // not tracked
}

#[no_mangle]
pub unsafe extern "C" fn getuid() -> u32 { 0 }

#[no_mangle]
pub unsafe extern "C" fn geteuid() -> u32 { 0 }

#[no_mangle]
pub unsafe extern "C" fn getgid() -> u32 { 0 }

#[no_mangle]
pub unsafe extern "C" fn getegid() -> u32 { 0 }

#[no_mangle]
pub unsafe extern "C" fn fork() -> i32 {
    crate::errno::set(ENOSYS);
    -1
}

#[no_mangle]
pub unsafe extern "C" fn execvp(_file: *const u8, _argv: *const *const u8) -> i32 {
    crate::errno::set(ENOSYS);
    -1
}

// Exit / abort / atexit

/// A handler `exit` runs: `atexit`'s takes nothing, `__cxa_atexit`'s its object.
enum AtExit {
    Plain(unsafe extern "C" fn()),
    WithArg(unsafe extern "C" fn(*mut u8), *mut u8),
}

// SAFETY: the argument is the registrant's to hand to its own handler.
unsafe impl Send for AtExit {}

/// Every handler registered, in order; `exit` runs the last first.
static AT_EXIT: crate::pthread::Lock<Vec<AtExit>> = crate::pthread::Lock::new(Vec::new());

/// What names this image to `__cxa_atexit`: the address is the identity.
#[no_mangle]
pub static __dso_handle: usize = 0;

#[no_mangle]
pub unsafe extern "C" fn atexit(func: unsafe extern "C" fn()) -> i32 {
    AT_EXIT.lock().push(AtExit::Plain(func));
    0
}

#[no_mangle]
pub unsafe extern "C" fn __cxa_atexit(func: unsafe extern "C" fn(*mut u8), arg: *mut u8, _dso: *mut u8) -> i32 {
    AT_EXIT.lock().push(AtExit::WithArg(func, arg));
    0
}

/// Run the handlers, the last registered first, including any a handler
/// registers; none runs with the list locked.
unsafe fn run_atexit() {
    loop {
        let Some(handler) = AT_EXIT.lock().pop() else { return };
        match handler {
            AtExit::Plain(f) => unsafe { f() },
            AtExit::WithArg(f, arg) => unsafe { f(arg) },
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn exit(status: i32) -> ! {
    unsafe {
        crate::pthread::run_thread_dtors();
        run_atexit();
    }
    #[cfg(not(feature = "std-runtime"))]
    crate::runtime::fini();
    super::stdio::fflush(ptr::null_mut());
    _exit(status)
}

/// Out without `atexit` or `fflush`, but not without the log: a line either
/// stream holds unended in the SDK's sink goes out as the process leaves.
#[no_mangle]
pub unsafe extern "C" fn _exit(status: i32) -> ! {
    toyos::log::stdio::end(toyos::log::stdio::Stream::Out);
    toyos::log::stdio::end(toyos::log::stdio::Stream::Err);
    syscall::exit(status)
}

#[no_mangle]
pub unsafe extern "C" fn _Exit(status: i32) -> ! {
    _exit(status)
}

#[no_mangle]
pub unsafe extern "C" fn abort() -> ! {
    syscall::exit(134) // SIGABRT
}

// Signals: what is kept and where each goes is `sigreq`'s, the deliveries
// `signal`'s.

/// The default action's end: out with `code`, without `atexit` or `fflush`.
pub(crate) fn end(code: i32) -> ! {
    unsafe { _exit(code) }
}

/// A handler by its address: `SIG_DFL` and `SIG_IGN` are no functions. A
/// handler `signal` keeps restarts what it interrupts, as BSD's and glibc's
/// do.
#[no_mangle]
pub unsafe extern "C" fn signal(signum: i32, handler: *const u8) -> *const u8 {
    let action = SigAction { flags: crate::sigreq::SA_RESTART, ..SigAction::of(handler as usize) };
    match crate::signal::act(signum, Some(action)) {
        Ok(old) => old.handler as *const u8,
        Err(errno) => {
            crate::errno::set(errno);
            usize::MAX as *const u8 // SIG_ERR
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sigaction(signum: i32, act: *const SigAction, oldact: *mut SigAction) -> i32 {
    match crate::signal::act(signum, unsafe { act.as_ref() }.copied()) {
        Ok(old) => {
            if let Some(oldact) = unsafe { oldact.as_mut() } {
                *oldact = old;
            }
            0
        }
        Err(errno) => {
            crate::errno::set(errno);
            -1
        }
    }
}

/// The calling thread's mask, which is what POSIX's process mask is in a
/// process of one thread (`pthread_sigmask`).
#[no_mangle]
pub unsafe extern "C" fn sigprocmask(how: i32, set: *const u64, oldset: *mut u64) -> i32 {
    match unsafe { crate::pthread::pthread_sigmask(how, set, oldset) } {
        0 => 0,
        refused => {
            crate::errno::set(refused);
            -1
        }
    }
}

/// What is pending: raised while every thread blocks it.
#[no_mangle]
pub unsafe extern "C" fn sigpending(set: *mut u64) -> i32 {
    unsafe { *set = crate::signal::pending() };
    0
}

#[no_mangle]
pub unsafe extern "C" fn sigemptyset(set: *mut u64) -> i32 {
    unsafe { *set = 0 };
    0
}

#[no_mangle]
pub unsafe extern "C" fn sigfillset(set: *mut u64) -> i32 {
    unsafe { *set = crate::sigmask::FULL };
    0
}

/// `set` changed by `change`, or `-1` and `EINVAL` for a number no signal has.
unsafe fn change_set(set: *mut u64, change: Option<u64>) -> i32 {
    match change {
        Some(next) => {
            unsafe { *set = next };
            0
        }
        None => {
            crate::errno::set(crate::errcode::EINVAL);
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sigaddset(set: *mut u64, signo: i32) -> i32 {
    unsafe { change_set(set, crate::sigmask::with(*set, signo)) }
}

#[no_mangle]
pub unsafe extern "C" fn sigdelset(set: *mut u64, signo: i32) -> i32 {
    unsafe { change_set(set, crate::sigmask::without(*set, signo)) }
}

#[no_mangle]
pub unsafe extern "C" fn sigismember(set: *const u64, signo: i32) -> i32 {
    match crate::sigmask::with(0, signo) {
        Some(bit) => i32::from(unsafe { *set } & bit != 0),
        None => {
            crate::errno::set(crate::errcode::EINVAL);
            -1
        }
    }
}

/// Raise `sig` in the calling thread: its handler or its default action runs
/// before this returns, unless the thread blocks it (`sigreq::raised`).
#[no_mangle]
pub unsafe extern "C" fn raise(sig: i32) -> i32 {
    match crate::signal::raise(sig) {
        Ok(()) => 0,
        Err(errno) => {
            crate::errno::set(errno);
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn alarm(seconds: u32) -> u32 {
    crate::signal::alarm(seconds)
}

// sysconf

const _SC_ARG_MAX: i32 = 0;
const _SC_PAGESIZE: i32 = 30;
const _SC_GETPW_R_SIZE_MAX: i32 = 70;
const _SC_NPROCESSORS_ONLN: i32 = 84;
const _SC_CLK_TCK: i32 = 2;

#[no_mangle]
pub unsafe extern "C" fn sysconf(name: i32) -> i64 {
    match name {
        _SC_PAGESIZE => crate::memreq::PAGE as i64,
        _SC_NPROCESSORS_ONLN => syscall::cpu_count() as i64,
        _SC_CLK_TCK => 100,
        // What `SYS_SPAWN` takes of each of `argv` and the environment
        // (`kernel/src/user_ptr.rs`'s `MAX_USER_STR`), so both together fit.
        _SC_ARG_MAX => 64 * 1024,
        // POSIX's -1 with `errno` untouched: the `getpw*_r` lookups it bounds
        // are ones ToyOS does not have.
        _SC_GETPW_R_SIZE_MAX => -1,
        _ => {
            crate::errno::set(crate::errcode::EINVAL);
            -1
        }
    }
}

// Random

static RAND_STATE: AtomicU32 = AtomicU32::new(1);

#[no_mangle]
pub unsafe extern "C" fn srand(seed: u32) {
    RAND_STATE.store(seed, Ordering::Relaxed);
}

#[no_mangle]
pub unsafe extern "C" fn rand() -> i32 {
    // LCG — same as glibc
    let mut s = RAND_STATE.load(Ordering::Relaxed);
    s = s.wrapping_mul(1103515245).wrapping_add(12345);
    RAND_STATE.store(s, Ordering::Relaxed);
    ((s >> 16) & 0x7fff) as i32
}

// Sorting / searching

type CmpFn = unsafe extern "C" fn(*const u8, *const u8) -> i32;

#[no_mangle]
pub unsafe extern "C" fn qsort(
    base: *mut u8, nmemb: usize, size: usize, compar: CmpFn,
) {
    if nmemb <= 1 || size == 0 { return; }
    // Simple insertion sort — good enough for small arrays, correct for all
    let mut tmp = alloc::vec![0u8; size];
    for i in 1..nmemb {
        let mut j = i;
        while j > 0 {
            let a = base.add(j * size);
            let b = base.add((j - 1) * size);
            if compar(a, b) < 0 {
                ptr::copy_nonoverlapping(a, tmp.as_mut_ptr(), size);
                ptr::copy(b, a, size);
                ptr::copy_nonoverlapping(tmp.as_ptr(), b, size);
                j -= 1;
            } else {
                break;
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn bsearch(
    key: *const u8, base: *const u8, nmemb: usize, size: usize, compar: CmpFn,
) -> *mut u8 {
    let mut lo = 0usize;
    let mut hi = nmemb;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let elem = base.add(mid * size);
        let cmp = compar(key, elem);
        if cmp < 0 {
            hi = mid;
        } else if cmp > 0 {
            lo = mid + 1;
        } else {
            return elem as *mut u8;
        }
    }
    ptr::null_mut()
}

// String-to-number conversions: the grammar and the rounding are `strtonum`'s.

/// What C is told of `read`, a number read from `s`: its value, returned;
/// where it ends in `endptr`, or `s` itself when there was none; and a
/// refusal in `errno`.
pub(crate) unsafe fn answer<U, T>(s: *const U, read: strtonum::Read<T>, endptr: *mut *mut U) -> T {
    match read.refused {
        Some(strtonum::Refusal::Base) => crate::errno::set(crate::errcode::EINVAL),
        Some(strtonum::Refusal::Range) => crate::errno::set(crate::errcode::ERANGE),
        None => {}
    }
    if !endptr.is_null() {
        unsafe { *endptr = s.add(read.end).cast_mut() };
    }
    read.value
}

#[no_mangle]
pub unsafe extern "C" fn atoi(s: *const u8) -> i32 {
    strtol(s, ptr::null_mut(), 10) as i32
}

#[no_mangle]
pub unsafe extern "C" fn atol(s: *const u8) -> i64 {
    strtol(s, ptr::null_mut(), 10)
}

#[no_mangle]
pub unsafe extern "C" fn atoll(s: *const u8) -> i64 {
    strtol(s, ptr::null_mut(), 10)
}

#[no_mangle]
pub unsafe extern "C" fn strtol(s: *const u8, endptr: *mut *mut u8, base: i32) -> i64 {
    unsafe { answer(s, strtonum::signed(s, base), endptr) }
}

#[no_mangle]
pub unsafe extern "C" fn strtoul(s: *const u8, endptr: *mut *mut u8, base: i32) -> u64 {
    unsafe { answer(s, strtonum::unsigned(s, base), endptr) }
}

#[no_mangle]
pub unsafe extern "C" fn strtoll(s: *const u8, endptr: *mut *mut u8, base: i32) -> i64 {
    unsafe { strtol(s, endptr, base) }
}

#[no_mangle]
pub unsafe extern "C" fn strtoull(s: *const u8, endptr: *mut *mut u8, base: i32) -> u64 {
    unsafe { strtoul(s, endptr, base) }
}

#[no_mangle]
pub unsafe extern "C" fn strtoimax(s: *const u8, endptr: *mut *mut u8, base: i32) -> i64 {
    unsafe { strtol(s, endptr, base) }
}

#[no_mangle]
pub unsafe extern "C" fn strtoumax(s: *const u8, endptr: *mut *mut u8, base: i32) -> u64 {
    unsafe { strtoul(s, endptr, base) }
}

#[no_mangle]
pub unsafe extern "C" fn strtod(s: *const u8, endptr: *mut *mut u8) -> f64 {
    unsafe { answer(s, strtonum::float::<u8, f64>(s), endptr) }
}

#[no_mangle]
pub unsafe extern "C" fn strtof(s: *const u8, endptr: *mut *mut u8) -> f32 {
    unsafe { answer(s, strtonum::float::<u8, f32>(s), endptr) }
}

// abs and div

#[no_mangle]
pub unsafe extern "C" fn abs(j: i32) -> i32 {
    j.wrapping_abs()
}

#[no_mangle]
pub unsafe extern "C" fn labs(j: i64) -> i64 {
    j.wrapping_abs()
}

#[no_mangle]
pub unsafe extern "C" fn llabs(j: i64) -> i64 {
    j.wrapping_abs()
}

#[no_mangle]
pub unsafe extern "C" fn imaxabs(j: i64) -> i64 {
    j.wrapping_abs()
}

#[repr(C)]
pub struct Div<T> {
    quot: T,
    rem: T,
}

#[no_mangle]
pub unsafe extern "C" fn div(numer: i32, denom: i32) -> Div<i32> {
    Div { quot: numer / denom, rem: numer % denom }
}

#[no_mangle]
pub unsafe extern "C" fn ldiv(numer: i64, denom: i64) -> Div<i64> {
    Div { quot: numer / denom, rem: numer % denom }
}

#[no_mangle]
pub unsafe extern "C" fn lldiv(numer: i64, denom: i64) -> Div<i64> {
    Div { quot: numer / denom, rem: numer % denom }
}

#[no_mangle]
pub unsafe extern "C" fn imaxdiv(numer: i64, denom: i64) -> Div<i64> {
    Div { quot: numer / denom, rem: numer % denom }
}

// setjmp/longjmp (minimal stub — used by some C code)

// jmp_buf is 8 * 8 bytes on x86_64 (enough for callee-saved regs + rsp + rip)
// Real implementation would need assembly; this is a panic stub.
#[no_mangle]
pub unsafe extern "C" fn setjmp(_env: *mut u8) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn longjmp(_env: *mut u8, _val: i32) -> ! {
    panic!("longjmp not implemented")
}

// dlopen/dlsym/dlclose
//
// A C handle is the kernel's module index plus one: index 0 is a module, and
// NULL is dlopen's failure.

fn module_index(handle: *mut u8) -> Option<u64> {
    (handle as u64).checked_sub(1)
}

#[no_mangle]
pub unsafe extern "C" fn dlopen(path: *const u8, _flags: i32) -> *mut u8 {
    if path.is_null() { return ptr::null_mut(); }
    let path_bytes = super::posix_io::c_str_to_bytes(path);
    match toyos::files::dl_open(path_bytes) {
        Ok(index) => (index + 1) as *mut u8,
        Err(refused) => {
            crate::errno::set(match refused {
                toyos::files::DlOpenRefused::Image(refused) => crate::errcode::of_image(refused),
                toyos::files::DlOpenRefused::Load(e) => crate::errcode::of(e),
            });
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn dlsym(handle: *mut u8, symbol: *const u8) -> *mut u8 {
    if symbol.is_null() { return ptr::null_mut(); }
    let index = module_index(handle).expect("dlsym: RTLD_DEFAULT not implemented");
    let name = super::posix_io::c_str_to_bytes(symbol);
    // SAFETY: handle is from a prior dlopen, name is a valid C string
    match unsafe { syscall::dl_sym(index, name) } {
        Ok(addr) => addr as *mut u8,
        Err(_) => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn dlclose(handle: *mut u8) -> i32 {
    match module_index(handle) {
        Some(index) => syscall::dl_close(index) as i32,
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn dlerror() -> *const u8 {
    ptr::null()
}

/// `getentropy`: at most 256 bytes of the kernel's random source, all or an
/// error, as POSIX says.
#[no_mangle]
pub unsafe extern "C" fn getentropy(buffer: *mut u8, length: usize) -> i32 {
    if length > 256 {
        crate::errno::set(crate::errcode::EINVAL);
        return -1;
    }
    // Before the slice: `buffer` may be null when there is nothing to fill.
    if length == 0 {
        return 0;
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(buffer, length) };
    match syscall::random(buf) {
        Ok(()) => 0,
        Err(_) => {
            crate::errno::set(crate::errcode::EIO);
            -1
        }
    }
}
