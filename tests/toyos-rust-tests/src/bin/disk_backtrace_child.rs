//! Dereferences null so the kernel prints a SEGFAULT report for it.

#[inline(never)]
fn null_deref_run_from_disk() -> u64 {
    unsafe { core::ptr::read_volatile(core::ptr::null::<u64>()) }
}

fn main() {
    let _ = null_deref_run_from_disk();
}
