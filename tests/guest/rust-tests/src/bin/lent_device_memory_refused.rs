//! A file server lets go a hello whose window is a device's registers, which
//! the kernel says are not memory it allocated, and answers the next client as
//! the same process.
//!
//! The device is QEMU's virtio NIC, which no program of
//! `tests/images/testcases.toml` holds.

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::{AsHandle, OwnedHandle, PciDev};
use toyos_abi::syscall::{self, PciId, SyscallError, SYSCAP_LABEL};

#[path = "../lent_hello.rs"]
mod lent_hello;
use lent_hello::{hello, window};

const NIC: PciId = PciId { vendor: 0x1af4, device: 0x1041 };

fn main() {
    let served = hello(window()).expect("lent_device_memory_refused: a window of WINDOW_BYTES is served");

    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("lent_device_memory_refused: a device-minting capability");
    let nic: PciDev = cap.claim_pci(NIC).expect("lent_device_memory_refused: the virtio NIC's claim");
    let info = nic.describe().expect("lent_device_memory_refused: the claim's description");
    let bar = info.bar_bytes.iter().position(|&bytes| bytes != 0).expect("lent_device_memory_refused: a memory BAR");
    let bar = syscall::device_bar_map(nic.as_handle(), bar as u32).expect("lent_device_memory_refused: the BAR's region");
    // SAFETY: the kernel installed the handle for this call, and nothing else answers for it.
    let answer = hello(unsafe { OwnedHandle::from_raw(bar) });
    let again = hello(window()).expect("lent_device_memory_refused: a window after the refusal is served");

    println!("lent_device_memory_refused: a hello lending a device's BAR answered {answer:?}");
    assert_eq!(answer, Err(SyscallError::Gone), "lent_device_memory_refused: the BAR was not let go unanswered");
    assert_eq!(again, served, "lent_device_memory_refused: the server that let go is not the one that answers");
    println!("lent_device_memory_refused: the server that let go answers the next client, volume {served}");
}
