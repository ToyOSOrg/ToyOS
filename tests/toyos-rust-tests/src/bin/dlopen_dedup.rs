//! A repeat `dlopen` of one library shares that module rather than mapping a
//! new copy — `SYS_DLOPEN` used to grow the address space without bound. Read
//! two ways: handle identity (64 loads of one path return one handle, not
//! 0, 1, 2, …), and the ELF/POSIX same-object guarantee (a TLS global written
//! through the first handle's symbol is read back through the last's — one
//! module, one TLS block; a fresh second module would read zero). Raw `dl_*`
//! syscalls, not `libloading`, so the handle integer is observable.

use std::sync::{Arc, Barrier};

use toyos_abi::syscall;

const LIB: &[u8] = b"/system/lib/libtls_dlopen_lib.so";
/// A name this binary loads nowhere else, so the concurrent arm starts clean.
const OTHER_LIB: &[u8] = b"/system/lib/libtls_multi_crate.so";
const LOADS: usize = 64;

/// A directory only this test spawns from, so the `DT_NEEDED` fallback caches
/// under a spelling nothing else can produce; `check_dlopen_dedup` in
/// `tests/toyos.rs` holds the verdict. The binary beside it `DT_NEEDED`s a
/// library that is in `/system/lib` and not there.
const FROM: &str = "/tmp/dlopen-dedup";
const NEEDS_A_LIB: &str = "/system/bin/test_rs_std_tls";
const BY_PATH: &[u8] = b"/system/lib/libtls_lib.so";

fn main() {
    let handles = handle_identity();
    object_identity(handles[0], handles[LOADS - 1]);
    one_name_under_contention();
    one_library_under_two_spellings();
    println!("all dlopen dedup checks passed");
}

/// A library reached through the `/system/lib` fallback and then by its own path is one
/// module, not two. **The verdict is the kernel's and this arm only stages it:**
/// a guest cannot count physical images, so it puts the fallback's spelling
/// somewhere unique — a `DT_NEEDED`-carrying binary in [`FROM`], which holds no
/// library — and a loader caching under the directory it searched logs that path.
fn one_library_under_two_spellings() {
    let copy = format!("{FROM}/needs-a-lib");
    std::fs::create_dir_all(FROM).unwrap_or_else(|e| panic!("make {FROM}: {e}"));
    let bytes = std::fs::read(NEEDS_A_LIB).unwrap_or_else(|e| panic!("read {NEEDS_A_LIB}: {e}"));
    std::fs::write(&copy, &bytes).unwrap_or_else(|e| panic!("write {copy}: {e}"));

    let status = std::process::Command::new(&copy)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("spawn {copy}: {e}"));
    assert_eq!(
        status.code(),
        Some(0),
        "the copy in {FROM} did not run, so its DT_NEEDED never took the /system/lib fallback",
    );

    syscall::dl_open(BY_PATH).expect("the same library by its own path");
    println!("  PASS: a library came through the fallback and then by its own path");
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

/// The ELF/POSIX same-object guarantee: two handles to one name share the
/// module's TLS. Written through `first`, read through `last`.
fn object_identity(first: u64, last: u64) {
    // Same symbol, same address: a second, cheaper witness of the one mapping.
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
                syscall::dl_open(OTHER_LIB).unwrap_or_else(|e| panic!("racer {i}: {e:?}"))
            })
        })
        .collect();
    let handles: Vec<u64> = racers.into_iter().map(|t| t.join().expect("a racer")).collect();
    assert!(
        handles.iter().all(|&h| h == handles[0]),
        "{RACERS} threads loading one name concurrently got {handles:?} — the dedup holds \
         only once the race has settled, so each loser mapped the library again"
    );
    println!("  PASS: {RACERS} concurrent loads of one name returned one handle ({})", handles[0]);
}
