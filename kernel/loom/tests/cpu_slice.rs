//! Host-fast regression for a per-CPU table's one sizing.
//!
//! `kernel/src/cpu_slice.rs` is compiled here as the kernel compiles it. A
//! second sizing is a kernel bug, refused by name; the refusal must leave the
//! first table whole, or a reader on another CPU pairs its pointer with the
//! second length and reads past it.

#![cfg(not(feature = "loom"))]

use kernel_loom::cpu_slice::CpuSlice;

#[test]
fn a_second_sizing_is_refused_and_leaves_the_first_table_whole() {
    let table: CpuSlice<u64> = CpuSlice::new();
    assert!(table.is_empty(), "a table nobody sized is empty");
    table.size(2, |cpu| cpu as u64 * 10);
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| table.size(64, |_| 7)));
    assert!(refused.is_err(), "a second sizing was taken");
    assert_eq!(&*table, &[0, 10], "the refused sizing changed the table a reader sees");
}
