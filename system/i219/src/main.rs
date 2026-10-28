//! The Intel I219 and 82574 driver: one process per such function the kernel
//! does not drive, holding its claim (a `drives` row) and carrying its frames
//! to the network stack over a link (`toyos-link`).
//!
//! **Every decision about the part is `toyos-i219`'s**; [`nic`] is the claim's
//! two mappings and the copies between the part's buffers and the link.

use toyos::endow;
use toyos::{say, PciDev};
use toyos_abi::syscall::PciId;
use toyos_i219::Part;
use toyos_link::driver::Nic as _;
use toyos_manifest::device::PciMatch;

mod nic;

/// The parts this driver brings up, matched by their class and identity
/// rather than by the slot firmware put them in, as the row spells them.
/// `8086:15fc` is the ThinkPad T14's onboard I219 at `00:1f.6`; `8086:10d3`
/// is the 82574L, which QEMU's `e1000e` models. Each names which part it is
/// because below the register file they are not one.
const PARTS: [(PciMatch, Part); 2] = [(ethernet(0x8086, 0x15fc), Part::I219), (ethernet(0x8086, 0x10d3), Part::E82574)];

/// An Ethernet controller, class `02:00:00`.
const fn ethernet(vendor: u16, device: u16) -> PciMatch {
    PciMatch::new([0x02, 0x00, 0x00], Some(PciId { vendor, device }))
}

fn main() {
    let (claim, part) = PARTS
        .iter()
        .find_map(|(matcher, part)| endow::claim::<PciDev>(&matcher.to_string()).map(|claim| (claim, *part)))
        .expect("i219: the drives row endows this process one of its parts' claims");
    // A part the claim was minted on that this driver cannot bring up is
    // something the device said that this driver does not understand: loud,
    // and the supervisor's to start again or give up on.
    let mut nic = nic::Nic::open(claim, part)
        .unwrap_or_else(|why| panic!("i219: the NIC this program was given is not one it can drive — {why}"));
    let m = nic.mac();
    let label = nic.label();
    say!("{label}: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]);
    toyos_link::driver::drive(label, &mut nic);
}
