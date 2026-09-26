use std::cell::Cell;

// Functions from tls-lib (loaded as shared library via DT_NEEDED)
#[link(name = "tls_lib")]
extern "C" {
    fn tls_increment() -> u64;
    fn tls_get_counter() -> u64;
    fn tls_get_label() -> u64;
    fn tls_set_label(val: u64);
}

// 64-byte aligned by its own type, so the exe's `PT_TLS` declares at least that
// whatever the linker would pick, and `LOCAL_VALUE`'s address is on 64 exactly
// when the loader placed the exe's module on its declared alignment.
#[repr(align(64))]
struct Aligned(Cell<u64>);

thread_local! {
    static LOCAL_VALUE: Aligned = const { Aligned(Cell::new(42)) };
}

fn main() {
    // Test 0: psABI variant II — the exe's TLS module begins on its declared
    // 64-byte p_align behind libtls_lib.so; the constant-16 loader landed it 32 low.
    LOCAL_VALUE.with(|v| {
        let addr = v as *const Aligned as usize;
        assert_eq!(addr % 64, 0, "exe TLS module base off its declared 64-byte alignment: {addr:#x}");
    });
    println!("PASS: exe TLS honours its declared alignment");

    // Test 1: exe-local thread_local works
    LOCAL_VALUE.with(|v| {
        assert_eq!(v.0.get(), 42, "exe TLS initial value");
        v.0.set(100);
        assert_eq!(v.0.get(), 100, "exe TLS after set");
    });
    println!("PASS: exe thread_local");

    // Test 2: shared library TLS works
    unsafe {
        assert_eq!(tls_get_counter(), 0, "lib TLS initial counter");
        assert_eq!(tls_increment(), 1, "lib TLS first increment");
        assert_eq!(tls_increment(), 2, "lib TLS second increment");
        assert_eq!(tls_get_counter(), 2, "lib TLS counter after increments");
    }
    println!("PASS: lib thread_local counter");

    // Test 3: shared library TLS doesn't alias exe TLS
    unsafe {
        assert_eq!(tls_get_label(), 0xDEAD_BEEF, "lib TLS initial label");
        tls_set_label(0xCAFE_BABE);
        assert_eq!(tls_get_label(), 0xCAFE_BABE, "lib TLS label after set");
    }
    // Verify exe TLS wasn't corrupted
    LOCAL_VALUE.with(|v| {
        assert_eq!(v.0.get(), 100, "exe TLS not corrupted by lib TLS ops");
    });
    println!("PASS: TLS isolation between exe and lib");

    println!("all TLS tests passed");
}
