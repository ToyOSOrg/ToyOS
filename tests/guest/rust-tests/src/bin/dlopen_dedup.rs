//! A repeat `dlopen` of one library shares that module rather than mapping a
//! new copy — the kernel's loader used to grow the address space without bound. Read
//! two ways: handle identity (64 loads of one path return one handle, not
//! 0, 1, 2, …), and the ELF/POSIX same-object guarantee (a TLS global written
//! through the first handle's symbol is read back through the last's — one
//! module, one TLS block; a fresh second module would read zero). libc's
//! `dlopen` and `dlsym`, not `libloading`, so the handle is observable.

use std::ffi::{c_char, c_void, CStr};
use std::sync::{Arc, Barrier};

// The C library's loader.
#[link(name = "toyos_c", kind = "static")]
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

const LIB: &CStr = c"/system/lib/libtls_dlopen_lib.so";
/// A name this binary loads nowhere else, so the concurrent arm starts clean.
const OTHER_LIB: &CStr = c"/system/lib/libtls_multi_crate.so";

/// libc's handle for `path`, as an integer so it can be compared and sent.
fn open(path: &CStr) -> Option<usize> {
    // SAFETY: a NUL-terminated path.
    let handle = unsafe { dlopen(path.as_ptr(), 2) };
    (!handle.is_null()).then_some(handle as usize)
}

/// The address `handle`'s module gives `name`.
fn sym(handle: usize, name: &CStr) -> Option<usize> {
    // SAFETY: a handle `dlopen` answered, and a NUL-terminated name.
    let at = unsafe { dlsym(handle as *mut c_void, name.as_ptr()) };
    (!at.is_null()).then_some(at as usize)
}
const LOADS: usize = 64;

fn main() {
    let handles = handle_identity();
    object_identity(handles[0], handles[LOADS - 1]);
    one_name_under_contention();
    println!("all dlopen dedup checks passed");
}

/// The mechanism: repeated loads of one name are one handle.
fn handle_identity() -> Vec<usize> {
    let handles: Vec<usize> = (0..LOADS)
        .map(|i| open(LIB).unwrap_or_else(|| panic!("dlopen #{i}")))
        .collect();

    assert!(
        handles.iter().all(|&h| h == handles[0]),
        "dlopen did not dedup: {LOADS} loads of one library returned {handles:?} — a fresh \
         mapping each time is the address-space exhaustion this closes"
    );
    println!("  PASS: {LOADS} loads of one library returned one handle ({})", handles[0]);
    handles
}

/// The ELF/POSIX same-object guarantee: two handles to one name share the
/// module's TLS. Written through `first`, read through `last`.
fn object_identity(first: usize, last: usize) {
    // Same symbol, same address: a second, cheaper witness of the one mapping.
    let get_via_first = sym(first, c"dl_tls_get_a").expect("dl_tls_get_a via first");
    let get_via_last = sym(last, c"dl_tls_get_a").expect("dl_tls_get_a via last");
    assert_eq!(
        get_via_first, get_via_last,
        "the same symbol resolved to two addresses — the library is mapped twice"
    );

    let inc = sym(first, c"dl_tls_increment_a").expect("dl_tls_increment_a");
    let inc: extern "C" fn() -> u64 = unsafe { core::mem::transmute(inc) };
    let get_last: extern "C" fn() -> u64 = unsafe { core::mem::transmute(get_via_last) };

    inc();
    inc();
    inc();

    // One object, one TLS block on this thread: 3, where a fresh module reads 0.
    let seen = get_last();
    assert_eq!(
        seen, 3,
        "a value written through the first handle was not seen through the last ({seen}) — the \
         repeat dlopen produced a separate object rather than the same one"
    );
    println!("  PASS: a module-global written through one handle is read through the other");
}

/// One name under contention is still one module. A second library, because a name
/// this process already holds never reaches the window between the dedup lookup and
/// the registration; the barrier is what makes the racers collide rather than queue.
fn one_name_under_contention() {
    const RACERS: usize = 8;
    let line = Arc::new(Barrier::new(RACERS));
    let racers: Vec<_> = (0..RACERS)
        .map(|i| {
            let line = Arc::clone(&line);
            std::thread::spawn(move || {
                line.wait();
                open(OTHER_LIB).unwrap_or_else(|| panic!("racer {i}"))
            })
        })
        .collect();
    let handles: Vec<usize> = racers.into_iter().map(|t| t.join().expect("a racer")).collect();
    assert!(
        handles.iter().all(|&h| h == handles[0]),
        "{RACERS} threads loading one name concurrently got {handles:?} — the dedup holds \
         only once the race has settled, so each loser mapped the library again"
    );
    println!("  PASS: {RACERS} concurrent loads of one name returned one handle ({})", handles[0]);
}
