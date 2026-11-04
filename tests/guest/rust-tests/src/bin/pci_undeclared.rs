//! A PCI claim on a boot nothing declares the root bridges' memory on: the
//! device set says the declaration is pending, and a claim of each of QEMU's
//! `edu` there is refused at once, `WouldBlock`, rather than waiting for one.

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::Device;
use toyos_abi::devset::Declaration;
use toyos_abi::inventory::{Pci, RawRecord};
use toyos_abi::syscall::{SyscallError, SYSCAP_LABEL};

/// What it says once every claim was refused.
const REFUSED: &str = "pci_undeclared: the set says the declaration is pending, and each edu's claim was refused at once: WouldBlock";

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("the test estate is endowed a device-minting capability");
    let set = cap.device_set().expect("pci_undeclared: the device set, minted");
    let (declaration, edus) = set
        .read(|n| vec![RawRecord::EMPTY; n], |snapshot| {
            let edus: Vec<Pci> = snapshot.functions().filter(|pci| pci.vendor == 0x1234 && pci.device == 0x11e8).collect();
            (snapshot.header.declaration, edus)
        })
        .unwrap_or_else(|why| panic!("pci_undeclared: {why}"));
    assert_eq!(declaration, Declaration::Pending, "pci_undeclared: the declaration on a boot with no acpi holder");
    assert_eq!(edus.len(), 2, "pci_undeclared: the edu this machine was made with");
    for edu in edus {
        let refused = cap.claim_function::<Device>(edu.arrival).err();
        assert_eq!(refused, Some(SyscallError::WouldBlock), "pci_undeclared: the claim of arrival {}", edu.arrival.0);
    }
    println!("{REFUSED}");
}
