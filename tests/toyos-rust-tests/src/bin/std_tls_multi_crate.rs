use std::sync::{Arc, Barrier};

use toyos_abi::syscall;

/// Three TLS-bearing names no earlier line of this binary holds, so every racer
/// runs the whole load rather than returning the dedup lookup's answer. The
/// third is the one the rest of this binary exercises: the checks below read
/// the first two, and every test after them reads that one.
const RACED: [&[u8]; 3] = [
    b"/lib/libtls_lib.so",
    b"/lib/libtls_dlopen_lib.so",
    b"/lib/libtls_multi_crate.so",
];

fn main() {
    two_names_at_once_are_two_modules();

    let lib = unsafe { libloading::Library::new("/lib/libtls_multi_crate.so") }
        .expect("failed to dlopen tls-multi-crate");

    let push = unsafe { lib.get::<unsafe extern "C" fn(u8) -> u8>(b"mc_push") }.expect("mc_push");
    let pop = unsafe { lib.get::<unsafe extern "C" fn(u8) -> u8>(b"mc_pop") }.expect("mc_pop");
    let lazy_val = unsafe { lib.get::<unsafe extern "C" fn() -> u64>(b"mc_lazy_value") }.expect("mc_lazy_value");
    let global_count = unsafe { lib.get::<unsafe extern "C" fn() -> u64>(b"mc_global_count") }.expect("mc_global_count");
    let dep_tls = unsafe { lib.get::<unsafe extern "C" fn() -> u64>(b"mc_dep_tls") }.expect("mc_dep_tls");
    let dep_tls_set = unsafe { lib.get::<unsafe extern "C" fn(u64)>(b"mc_dep_tls_set") }.expect("mc_dep_tls_set");

    // Test 1: basic push/pop (like cranelift timing tokens)
    unsafe {
        let prev = push(10);
        assert_eq!(prev, 0, "push(10): prev should be 0 (initial)");
        let prev = push(20);
        assert_eq!(prev, 10, "push(20): prev should be 10");
        let cur = pop(10);
        assert_eq!(cur, 20, "pop(10): current should be 20");
        let cur = pop(0);
        assert_eq!(cur, 10, "pop(0): current should be 10");
    }
    println!("PASS: basic push/pop");

    // Test 2: lazy Box<dyn Trait> TLS works
    unsafe {
        assert_eq!(lazy_val(), 42, "lazy TLS should return 42");
    }
    println!("PASS: lazy Box<dyn Trait> TLS");

    // Test 3: dep global counter works (and doesn't corrupt TLS)
    unsafe {
        let count = global_count();
        // push was called twice above, each bumps global
        assert_eq!(count, 2, "global counter should be 2 after 2 push calls");
    }
    println!("PASS: dep global counter");

    // Test 4: dep TLS works
    unsafe {
        assert_eq!(dep_tls(), 0xBEEF, "dep TLS initial value");
        dep_tls_set(0xCAFE);
        assert_eq!(dep_tls(), 0xCAFE, "dep TLS after set");
    }
    println!("PASS: dep TLS");

    // Test 5: push/pop still works after accessing global + lazy + dep TLS
    // (catches corruption from overlapping symbols)
    unsafe {
        let prev = push(30);
        assert_eq!(prev, 0, "push(30) after all accesses: should be 0 (was restored)");
        let cur = pop(0);
        assert_eq!(cur, 30, "pop(0): should be 30");
    }
    println!("PASS: push/pop after mixed accesses");

    // Test 6: interleaved global bumps + TLS access (stress test for corruption)
    unsafe {
        for i in 0u8..50 {
            let prev = push(i + 1);
            assert_eq!(prev, i, "iteration {}: push({}) prev should be {}", i, i+1, i);
            let _ = global_count(); // touch global between TLS accesses
            let _ = lazy_val();     // touch lazy TLS
        }
        let cur = pop(0);
        assert_eq!(cur, 50, "after 50 pushes: current should be 50");
        // Unwind all the nested pushes (we only popped once, so current is now 0)
    }
    println!("PASS: interleaved stress test");

    println!("all multi-crate TLS tests passed");
}

fn sym(handle: u64, name: &[u8]) -> usize {
    let addr = unsafe { syscall::dl_sym(handle, name) }
        .unwrap_or_else(|e| panic!("dl_sym {}: {e:?}", String::from_utf8_lossy(name)));
    addr as usize
}

/// Two threads loading two *different* TLS-bearing names at once hold two
/// modules: `DTPMOD64` names a module and two modules are two, so one library's
/// thread-locals are never reachable through the other's DTV slot.
fn two_names_at_once_are_two_modules() {
    let line = Arc::new(Barrier::new(RACED.len()));
    let racers: Vec<_> = RACED
        .iter()
        .map(|name| {
            let name: &'static [u8] = name;
            let line = Arc::clone(&line);
            std::thread::spawn(move || {
                // A first load is a disk read, so the barrier alone would race
                // the block queue rather than the two loads.
                let path = core::str::from_utf8(name).expect("a library path");
                std::fs::read(path).unwrap_or_else(|e| panic!("warm {path}: {e}"));
                line.wait();
                syscall::dl_open(name).unwrap_or_else(|e| panic!("dlopen {path}: {e:?}"))
            })
        })
        .collect();
    let handles: Vec<u64> = racers.into_iter().map(|t| t.join().expect("a racer")).collect();

    let a_label: extern "C" fn() -> u64 =
        unsafe { core::mem::transmute(sym(handles[0], b"tls_get_label")) };
    let a_set_label: extern "C" fn(u64) =
        unsafe { core::mem::transmute(sym(handles[0], b"tls_set_label")) };
    let a_inc: extern "C" fn() -> u64 =
        unsafe { core::mem::transmute(sym(handles[0], b"tls_increment")) };
    let b_label: extern "C" fn() -> u64 =
        unsafe { core::mem::transmute(sym(handles[1], b"dl_tls_get_label")) };
    let b_set_label: extern "C" fn(u64) =
        unsafe { core::mem::transmute(sym(handles[1], b"dl_tls_set_label")) };
    let b_counter: extern "C" fn() -> u64 =
        unsafe { core::mem::transmute(sym(handles[1], b"dl_tls_get_a")) };
    let b_buffer: extern "C" fn() -> u64 =
        unsafe { core::mem::transmute(sym(handles[1], b"dl_tls_check_buffer")) };

    // Read before anything writes: a block sized and templated from the other
    // module answers with the other module's initialisers.
    assert_eq!(
        a_label(),
        0xDEAD_BEEF,
        "libtls_lib's own TLS initialiser is not what its first read returned — its block was \
         templated from the sibling loaded beside it"
    );
    assert_eq!(
        b_label(),
        0xDEAD_BEEF,
        "libtls_dlopen_lib's own TLS initialiser is not what its first read returned — its block \
         was templated from the sibling loaded beside it"
    );
    assert_eq!(
        b_buffer(),
        1,
        "libtls_dlopen_lib's 64-byte thread-local buffer is not its template's bytes — its block \
         was sized from a smaller module"
    );

    a_set_label(0xA1);
    b_set_label(0xB2);
    assert_eq!(a_label(), 0xA1, "libtls_lib read back a label it did not write");
    assert_eq!(b_label(), 0xB2, "libtls_dlopen_lib read back a label it did not write");

    assert_eq!(a_inc(), 1, "libtls_lib's counter did not start at its own zero");
    assert_eq!(a_inc(), 2, "libtls_lib's counter did not advance by one");
    assert_eq!(a_inc(), 3, "libtls_lib's counter did not advance by one");
    assert_eq!(
        b_counter(),
        0,
        "libtls_dlopen_lib's counter moved when the sibling's did — the two names took one module \
         id and share one thread-local block"
    );
    println!("PASS: two names loaded at once are two TLS modules");
}
