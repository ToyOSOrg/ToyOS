//! Each function of a fixed list this machine has is claimed, and the MSI-X
//! entry the kernel armed is read back through its holder's own mapping of the
//! table's BAR: the address and data the kernel logged writing it say the
//! mapping is the function's own pages where firmware put them, the table's
//! page included (PCI 3.0 §6.8.2: the table is read and written through a
//! memory BAR).
//!
//! The functions are the radios P1 of `issues/there-is-no-wifi.md` names on
//! the benches, which no image drives, and QEMU's virtio NIC, which a guest
//! row reads.

#[path = "../function.rs"]
mod function;

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::PciDev;
use toyos_abi::syscall::{PciId, RegWidth, SyscallError, SYSCAP_LABEL};
use toyos_pci::caps::CapWalk;
use toyos_pci::msix;

const FUNCTIONS: &[(&str, PciId)] = &[
    ("the T14's AX210", PciId { vendor: 0x8086, device: 0x2725 }),
    ("the AMD bench's AX200", PciId { vendor: 0x8086, device: 0x2723 }),
    ("QEMU's virtio NIC", PciId { vendor: 0x1af4, device: 0x1041 }),
];

/// The entry the kernel arms (`drivers/pci.rs`'s `MSIX_ENTRY`).
const ENTRY: u64 = 0;

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("the test estate is endowed a device-minting capability");
    let mut read = 0;
    for &(name, id) in FUNCTIONS {
        let dev: PciDev = match function::claim(&cap, id) {
            Ok(dev) => dev,
            Err(SyscallError::NotFound) => continue,
            Err(why) => panic!("msix_in_place: the claim on {name} was refused: {why:?}"),
        };
        let info = dev.describe().expect("msix_in_place: the claim's description");
        let (bir, offset) = table(&dev, name);
        let bytes = info.bar_bytes[usize::from(bir)];
        assert!(
            offset + (ENTRY + 1) * msix::ENTRY_BYTES <= bytes,
            "msix_in_place: {name}'s table at BAR {bir} + {offset:#x} is outside the {bytes:#x} bytes handed over"
        );
        let bar = dev.map_bar(u32::from(bir), bytes).expect("msix_in_place: the table's BAR");
        let word = |at: u64| {
            // SAFETY: inside the mapping, as asserted above, which `bar` keeps
            // alive; a device register is read by a volatile load.
            unsafe { bar.as_ptr().add((offset + ENTRY * msix::ENTRY_BYTES + at) as usize).cast::<u32>().read_volatile() }
        };
        println!(
            "msix_in_place: {name} at PCI {:02x}:{:02x}.{}: entry {ENTRY} at BAR {bir} + {offset:#x} reads \
             address={:#010x} data={:#010x} through its holder's mapping",
            info.bus,
            info.dev,
            info.func,
            word(msix::ENTRY_ADDRESS_LO),
            word(msix::ENTRY_DATA),
        );
        read += 1;
    }
    assert!(read > 0, "msix_in_place: this machine has none of the functions");
}

/// Which BAR the MSI-X table is in and how far into it, off config space.
fn table(dev: &PciDev, name: &str) -> (u8, u64) {
    let read = |at: u8| {
        dev.config_read(at.into(), RegWidth::U32)
            .unwrap_or_else(|why| panic!("msix_in_place: {name}'s config read at {at:#x} refused: {why:?}"))
    };
    let mut walk = CapWalk::new();
    let mut next = read(0x34) as u8;
    while let Some(at) = walk.step(next) {
        let header = read(at);
        next = (header >> 8) as u8;
        if header as u8 == msix::CAP_ID {
            let table = msix::Msix::decode((header >> 16) as u16, read(at + msix::TABLE as u8))
                .unwrap_or_else(|why| panic!("msix_in_place: {name}'s MSI-X capability: {why}"));
            return (table.bir(), table.offset());
        }
    }
    panic!("msix_in_place: {name} publishes no MSI-X capability");
}
