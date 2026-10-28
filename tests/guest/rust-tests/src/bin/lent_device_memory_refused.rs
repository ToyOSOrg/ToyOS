//! What the kernel says of a device's registers lent as a window, asked as a
//! file server asks it (`SharedMemory::lent`): not memory it allocated to a
//! region, so they are never mapped as one.
//!
//! The device is QEMU's virtio NIC, which no program of
//! `tests/images/testcases.toml` holds.

#[path = "../function.rs"]
mod function;

use toyos::endow::Endowments;
use toyos::fs::WINDOW_BYTES;
use toyos::shm::{Lent, SharedMemory};
use toyos::syscap::SysCap;
use toyos::{AsHandle, PciDev};
use toyos_abi::syscall::{self, PciId, SYSCAP_LABEL};

const NIC: PciId = PciId { vendor: 0x1af4, device: 0x1041 };

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("lent_device_memory_refused: a device-minting capability");
    let nic: PciDev = function::claim(&cap, NIC).expect("lent_device_memory_refused: the virtio NIC's claim");
    let info = nic.describe().expect("lent_device_memory_refused: the claim's description");
    let bar = info.bar_bytes.iter().position(|&bytes| bytes != 0).expect("lent_device_memory_refused: a memory BAR");
    let bar = syscall::device_bar_map(nic.as_handle().raw(), bar as u32).expect("lent_device_memory_refused: the BAR's region");
    // SAFETY: a fresh handle nothing else answers for.
    let answer = SharedMemory::lent(unsafe { toyos::OwnedHandle::from_raw(bar) }, WINDOW_BYTES).err();
    assert_eq!(answer, Some(Lent::NotRam), "lent_device_memory_refused: a device's BAR lent as a window");
    println!("lent_device_memory_refused: a device's BAR lent as a window is not memory the kernel allocated");
}
