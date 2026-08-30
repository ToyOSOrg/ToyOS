//! A repeat `dlopen` of one library shares that module — it does not map a new
//! copy each time.
//!
//! `SYS_DLOPEN` used to push a fresh module and a fresh mapping on every call,
//! so a loop of `dlopen` on the same `.so` walked the process out of virtual
//! address space. It now returns the handle the name already holds.
//!
//! Two independent readings of the one fix:
//!
//! - **Handle identity** (the mechanism): sixty-four loads of one path return
//!   one handle, where the old loader returned 0, 1, 2, … .
//! - **Object identity** (the ELF/POSIX guarantee that a repeat `dlopen`
//!   resolves the *same object*): a module-global written through a symbol from
//!   the first handle is read back through a symbol resolved from the last —
//!   the same value, because there is one module and one TLS block, not two.
//!   A loader that returned a fresh module would read the fresh module's zero.
//!
//! Uses the raw `dl_*` syscalls rather than `libloading` so the handle integer
//! itself is observable.

use toyos_abi::syscall;

const LIB: &[u8] = b"/lib/libtls_dlopen_lib.so";
const LOADS: usize = 64;

fn main() {
    let handles = handle_identity();
    object_identity(handles[0], handles[LOADS - 1]);
    println!("all dlopen dedup checks passed");
}

/// The mechanism: repeated loads of one name are one handle.
fn handle_identity() -> Vec<u64> {
    let handles: Vec<u64> = (0..LOADS)
        .map(|i| syscall::dl_open(LIB).unwrap_or_else(|e| panic!("dlopen #{i}: {e:?}")))
        .collect();

    assert!(
        handles.iter().all(|&h| h == handles[0]),
        "dlopen did not dedup: {LOADS} loads of one library returned {handles:?} — a fresh \
         mapping each time is the address-space exhaustion this closes"
    );
    println!("  PASS: {LOADS} loads of one library returned one handle ({})", handles[0]);
    handles
}

/// The ELF/POSIX guarantee: two handles to one name are one object, so they
/// share the module's TLS. Written through `first`, read through `last`.
fn object_identity(first: u64, last: u64) {
    // The addresses come out equal too when there is one mapping — a second,
    // cheaper witness of the same object before the state check below.
    let get_via_first =
        unsafe { syscall::dl_sym(first, b"dl_tls_get_a") }.expect("dl_tls_get_a via first");
    let get_via_last =
        unsafe { syscall::dl_sym(last, b"dl_tls_get_a") }.expect("dl_tls_get_a via last");
    assert_eq!(
        get_via_first, get_via_last,
        "the same symbol resolved to two addresses — the library is mapped twice"
    );

    let inc =
        unsafe { syscall::dl_sym(first, b"dl_tls_increment_a") }.expect("dl_tls_increment_a");
    let inc: extern "C" fn() -> u64 = unsafe { core::mem::transmute(inc as usize) };
    let get_last: extern "C" fn() -> u64 = unsafe { core::mem::transmute(get_via_last as usize) };

    // Write the module's TLS counter through the first handle's symbol.
    inc();
    inc();
    inc();

    // Read it through the last handle's symbol. One object, one TLS block on
    // this thread, so this is 3 — a fresh second module would read 0.
    let seen = get_last();
    assert_eq!(
        seen, 3,
        "a value written through the first handle was not seen through the last ({seen}) — the \
         repeat dlopen produced a separate object rather than the same one"
    );
    println!("  PASS: a module-global written through one handle is read through the other");
}
