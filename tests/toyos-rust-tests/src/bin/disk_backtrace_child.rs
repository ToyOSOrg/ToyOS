//! Dereferences null so the kernel prints a SEGFAULT report for it.
//!
//! `disk_backtrace` copies this binary onto a disk and runs it from there, so
//! the report has to name a symbol no *other* boot could have put in the same
//! capture window.

#[inline(never)]
fn null_deref_run_from_disk() -> u64 {
    unsafe { core::ptr::read_volatile(core::ptr::null::<u64>()) }
}

fn main() {
    let _ = null_deref_run_from_disk();
}
