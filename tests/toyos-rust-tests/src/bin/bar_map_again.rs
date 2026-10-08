//! One memory BAR of a claimed function, asked for, its handle closed, and
//! asked for again on the same live claim: the second answer is a handle or a
//! refusal. Its exit is that the kernel answered at all.
//!
//! The function is QEMU's virtio NIC, which no program of `tests/testcases`
//! holds.

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::{AsHandle, PciDev};
use toyos_abi::syscall::{self, PciId, SYSCAP_LABEL};

const NIC: PciId = PciId { vendor: 0x1af4, device: 0x1041 };

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    let nic: PciDev = cap.claim_pci(NIC).expect("bar_map_again: the virtio NIC's claim");
    let info = nic.describe().expect("bar_map_again: the claim's description");
    let bar = info
        .bar_bytes
        .iter()
        .position(|&bytes| bytes != 0)
        .expect("bar_map_again: the function has a memory BAR its holder may map") as u32;
    let first = syscall::device_bar_map(nic.as_handle(), bar).expect("bar_map_again: the BAR, asked for once");
    syscall::close(first);
    println!("bar_map_again: BAR {bar} asked for and its handle closed; asking again");
    match syscall::device_bar_map(nic.as_handle(), bar) {
        Ok(second) => {
            syscall::close(second);
            println!("bar_map_again: answered with a handle");
        }
        Err(why) => println!("bar_map_again: refused: {why:?}"),
    }
}
