//! The T14's I219 claimed, given back and claimed again in one boot: its exit
//! is whether both claims were handed over. What each claim spent at the unit
//! is the kernel's to say, and the metal rows read it there.

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::PciDev;
use toyos_abi::syscall::{PciId, SYSCAP_LABEL};

const I219: PciId = PciId { vendor: 0x8086, device: 0x15fc };

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    for claim in 1..=2 {
        match cap.claim_pci::<PciDev>(I219) {
            // Dropped at once: the release is what the second claim follows.
            Ok(function) => drop(function),
            Err(why) => {
                println!("pci_reclaim: claim {claim} of 8086:15fc was refused: {why:?}");
                std::process::exit(1);
            }
        }
    }
    println!("pci_reclaim: 8086:15fc was handed over and given back twice");
}
