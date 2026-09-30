// Miscellaneous POSIX/C functions: environment, process, signals, sysconf.

use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use toyos_abi::syscall;

use crate::strtonum;

const ENOSYS: i32 = 38;
const ECHILD: i32 = 10;

// Environment variables

// Cached environment block from kernel. Format: "KEY=VALUE\0KEY=VALUE\0\0"
static mut ENV_BUF: [u8; 8192] = [0; 8192];
static mut ENV_INITED: bool = false;

unsafe fn ensure_env() {
    let inited = ptr::addr_of!(ENV_INITED).read_volatile();
    if !inited {
        let buf = ptr::addr_of_mut!(ENV_BUF).as_mut().unwrap();
        let n = syscall::get_env(buf);
        if n < buf.len() {
            buf[n] = 0;
        }
        ptr::addr_of_mut!(ENV_INITED).write_volatile(true);
    }
}

#[no_mangle]
pub unsafe extern "C" fn getenv(name: *const u8) -> *mut u8 {
    if name.is_null() { return ptr::null_mut(); }
    ensure_env();

    let name_len = super::string::strlen(name);
    let name_slice = core::slice::from_raw_parts(name, name_len);

    let buf = &*ptr::addr_of!(ENV_BUF);
    let buf_mut = ptr::addr_of_mut!(ENV_BUF).cast::<u8>();
    let mut i = 0;
    while i < buf.len() && buf[i] != 0 {
        // Find end of this entry
        let start = i;
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        let entry = &buf[start..i];
        // Check if entry starts with name=
        if entry.len() > name_len && entry[name_len] == b'='
            && entry[..name_len] == *name_slice
        {
            return buf_mut.add(start + name_len + 1);
        }
        i += 1; // skip null terminator
    }
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn setenv(name: *const u8, value: *const u8, _overwrite: i32) -> i32 {
    // Not implemented — env is read-only from kernel
    let _ = (name, value);
    0
}

#[no_mangle]
pub unsafe extern "C" fn unsetenv(_name: *const u8) -> i32 {
    0
}

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

/// POSIX `waitpid`, for a layer that cannot make a child.
///
/// **There is no pid-addressed wait left**: `SYS_WAITPID` is retired and
/// `SYS_PROCESS_WAIT` takes the handle a spawn answered with. This layer holds
/// no such handle for anything, because it has no spawn path at all — `fork`,
/// `execvp` and `system` all answer `-1` above — so every pid it could be
/// asked about is a pid it has no child for, and `ECHILD` is the true answer
/// rather than a stub.
///
/// The day this layer grows `posix_spawn`, it grows a `pid -> Process handle`
/// map beside it and this reads that. Faking ambient authority is what the
/// compat layer is for; faking it over an empty set is not.
#[no_mangle]
pub unsafe extern "C" fn waitpid(_pid: i32, _status: *mut i32, _options: i32) -> i32 {
    crate::errno::set(ECHILD);
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

// Signal (stubs — ToyOS has no signals)

type SigHandlerT = unsafe extern "C" fn(i32);

#[no_mangle]
pub unsafe extern "C" fn signal(_signum: i32, handler: SigHandlerT) -> SigHandlerT {
    handler // return the handler as "previous", effectively a no-op
}

#[no_mangle]
pub unsafe extern "C" fn sigaction(
    _signum: i32, _act: *const u8, _oldact: *mut u8,
) -> i32 {
    0 // success
}

#[no_mangle]
pub unsafe extern "C" fn sigprocmask(
    _how: i32, _set: *const u64, _oldset: *mut u64,
) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn raise(_sig: i32) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn kill(_pid: i32, _sig: i32) -> i32 {
    0
}

// sysconf

const _SC_PAGESIZE: i32 = 30;
const _SC_NPROCESSORS_ONLN: i32 = 84;
const _SC_CLK_TCK: i32 = 2;

#[no_mangle]
pub unsafe extern "C" fn sysconf(name: i32) -> i64 {
    match name {
        _SC_PAGESIZE => 4096,
        _SC_NPROCESSORS_ONLN => syscall::cpu_count() as i64,
        _SC_CLK_TCK => 100,
        _ => -1,
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

/// Where `s` ends after `end` code units, or `s` itself when there was no
/// number: what C's `endptr` is told.
pub(crate) unsafe fn set_end<U>(s: *const U, end: usize, endptr: *mut *mut U) {
    if !endptr.is_null() {
        unsafe { *endptr = s.add(end).cast_mut() };
    }
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
    let (value, end) = unsafe { strtonum::signed(s, base) };
    unsafe { set_end(s, end, endptr) };
    value
}

#[no_mangle]
pub unsafe extern "C" fn strtoul(s: *const u8, endptr: *mut *mut u8, base: i32) -> u64 {
    let (value, end) = unsafe { strtonum::unsigned(s, base) };
    unsafe { set_end(s, end, endptr) };
    value
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
    let (value, end) = unsafe { strtonum::float::<u8, f64>(s) };
    unsafe { set_end(s, end, endptr) };
    value
}

#[no_mangle]
pub unsafe extern "C" fn strtof(s: *const u8, endptr: *mut *mut u8) -> f32 {
    let (value, end) = unsafe { strtonum::float::<u8, f32>(s) };
    unsafe { set_end(s, end, endptr) };
    value
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
    match syscall::dl_open(path_bytes) {
        Ok(index) => (index + 1) as *mut u8,
        Err(_) => ptr::null_mut(),
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
        crate::errno::set(crate::errno::EINVAL);
        return -1;
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(buffer, length) };
    match syscall::random(buf) {
        Ok(()) => 0,
        Err(_) => {
            crate::errno::set(crate::errno::EIO);
            -1
        }
    }
}
