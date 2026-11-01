//! The virtio network driver: one process per virtio-net function the kernel
//! does not drive, holding its claim (a `drives` row) and carrying its frames
//! to the network stack over a link (`toyos-link`).

use toyos::endow;
use toyos::{say, PciDev};
use toyos_abi::syscall::PciId;
use toyos_link::driver::Nic as _;
use toyos_manifest::device::PciMatch;

mod nic;

/// A virtio network device, class `02:00:00`: device id `0x1041` is virtio's
/// `0x1040 + 1` for one (virtio 1.2 §4.1.2.1).
const FUNCTION: PciMatch = PciMatch::new([0x02, 0x00, 0x00], Some(PciId { vendor: 0x1af4, device: 0x1041 }));

fn main() {
    let claim = endow::claim::<PciDev>(&FUNCTION.to_string())
        .expect("virtio-net: the drives row endows this process its function's claim");
    // A device the claim was minted on that this driver cannot bring up is
    // something it said that this driver does not understand: loud, and the
    // supervisor's to start again or give up on.
    let mut nic = nic::VirtioNet::open(claim)
        .unwrap_or_else(|why| panic!("virtio-net: the NIC this program was given is not one it can drive — {why}"));
    let m = nic.mac();
    say!("virtio-net: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]);
    toyos_link::driver::drive("virtio-net", &mut nic);
}
