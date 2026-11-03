//! A Ring 3 thread may set `RFLAGS.AC` (`CR0.AM` is clear, so it costs the
//! thread nothing), and Ring 0 runs without it: under it SMAP lets a kernel
//! access to a user page through. This thread sets it and enters the kernel
//! by a page fault on each of fresh pages and by a syscall; the test kernel's
//! `ac_witness` ends the machine by name at an entry that found it set. The
//! flag still set when the thread reads it back says the entries were taken
//! with it, and that the kernel gave the thread back its own.
//!
//! The witness is the test kernel's alone, where this would pass having
//! checked nothing, so the thread first asks for a `SYS_DEBUG` reading that
//! kernel answers and no other does.

use toyos_abi::syscall::{self, debug_action, SyscallError, VmAt, VmProt};

const PAGES: usize = 64;
const PAGE: usize = 4096;
const AC: u64 = 1 << 18;

fn main() {
    let reached = syscall::debug(debug_action::SPACE_REACHED);
    assert_ne!(reached, SyscallError::InvalidArgument.to_u64(), "this kernel carries no actuators, so no AC witness");
    // SAFETY: a fresh range nothing else names; committed, so each page is
    // populated by the first touch below, a Ring 3 page fault.
    let region = unsafe { syscall::vm_reserve(PAGES * PAGE, VmProt::ReadWrite, VmAt::ANYWHERE) }.expect("reserve");
    let flags: u64;
    // SAFETY: sets and clears one flag of this thread's own; the stores land in
    // the range reserved above, and the syscall clobbers what the ABI says.
    unsafe {
        core::arch::asm!(
            "pushfq",
            "or qword ptr [rsp], {ac}",
            "popfq",
            "2:",
            "mov byte ptr [{at}], 1",
            "add {at}, {page}",
            "dec {left:e}",
            "jnz 2b",
            "syscall",
            "pushfq",
            "pop {flags}",
            "pushfq",
            "and qword ptr [rsp], {not_ac}",
            "popfq",
            ac = const AC,
            not_ac = const !AC,
            page = const PAGE,
            at = inout(reg) region => _,
            left = inout(reg) PAGES as u32 => _,
            flags = out(reg) flags,
            in("rdi") syscall::SYS_CLOCK_NOW,
            lateout("rax") _,
            out("rcx") _,
            out("r11") _,
        );
    }
    assert!(flags & AC != 0, "RFLAGS.AC was clear after {PAGES} page faults and a syscall taken with it set");
    // SAFETY: nothing reaches the range after this.
    unsafe { syscall::vm_release(region, PAGES * PAGE) }.expect("release");
    println!("ring0_clears_ac: {PAGES} page faults and a syscall taken with RFLAGS.AC set found it clear in Ring 0");
}
