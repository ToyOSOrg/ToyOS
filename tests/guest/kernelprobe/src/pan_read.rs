//! `pan_read`: the test kernel reads a page of this process at its user
//! address from EL1 (`SYS_DEBUG`'s `NULL_READ`). With `PSTATE.PAN` set that is
//! a permission fault, which ends the machine; a call that returns is a kernel
//! that reached EL0's memory.

use toyos_abi::syscall::{self, debug_action, VmAt, VmProt};

pub fn main(_args: Vec<String>) {
    // SAFETY: a fresh page nothing else names.
    let page = unsafe { syscall::vm_reserve(4096, VmProt::ReadWrite, VmAt::ANYWHERE) }.expect("pan_read: reserve a page");
    // Populated and holding a known word, so a read that goes through has
    // something to find, not a fault of its own.
    // SAFETY: the page reserved above, read-write.
    unsafe { page.cast::<u64>().write_volatile(0x5041_4e5f_5245_4144) };
    let answer = syscall::debug_with(debug_action::NULL_READ, page as u64);
    panic!("pan_read: the kernel read {page:p} at EL1 and answered {answer:#x}: PAN did not refuse it");
}
