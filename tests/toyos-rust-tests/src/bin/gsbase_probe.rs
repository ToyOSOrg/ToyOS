//! The GS-base primitive from Ring 3: `#UD` where the kernel took
//! `CR4.FSGSBASE` away, a leaked per-CPU pointer where it did not.

fn main() {
    let base: u64;
    unsafe {
        core::arch::asm!("rdgsbase {b}", "wrgsbase {b}", b = out(reg) base, options(nomem, nostack));
    }
    println!("gsbase-primitive-present base={base:#018x}");
}
