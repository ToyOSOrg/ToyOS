//! A function the IORT routes past the SMMUv3 — QEMU's `e1000e`, behind a
//! root port of a `pxb-pcie` with `bypass_iommu=on` — is refused its claim,
//! and the kernel that refused it goes on: the next claim, of `edu`, is
//! handed over.

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::PciDev;
use toyos_abi::syscall::{PciId, SyscallError};

/// QEMU's `e1000e`, an 82574L.
const E1000E: PciId = PciId { vendor: 0x8086, device: 0x10d3 };
/// `hw/misc/edu.c`: QEMU's educational device.
const EDU: PciId = PciId { vendor: 0x1234, device: 0x11e8 };

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability");
    let refused = cap.claim_pci::<PciDev>(E1000E).map(|_| ());
    assert_eq!(refused, Err(SyscallError::NotSupported), "claim_unrouted: the claim on e1000e was not refused as unusable");
    let _edu: PciDev = cap.claim_pci(EDU).unwrap_or_else(|why| panic!("claim_unrouted: edu, claimed after, was refused: {why:?}"));
    println!("claim_unrouted: e1000e, routed past the SMMUv3, was refused, and edu was claimed after");
}
