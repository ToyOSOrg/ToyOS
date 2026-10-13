//! The ECDT's controller (ACPI 6.5 §5.2.15, Table 5.54): both ports, its GPE
//! and its device's path, and each refusal by name.

mod common;

use common::{sdt, Machine};
use toyos_acpi::{ecdt, Ec, EcRefused, EcRegister, Table, ECDT_NEEDED};

/// A Generic Address Structure (Table 5.1): space, bit width, bit offset,
/// access size, then the address.
fn gas(space: u8, bit_width: u8, bit_offset: u8, address: u64) -> Vec<u8> {
    let mut g = vec![space, bit_width, bit_offset, 1];
    g.extend_from_slice(&address.to_le_bytes());
    g
}

/// An ECDT of `EC_CONTROL`, `EC_DATA`, `UID` 1, `GPE_BIT` and `EC_ID`.
fn table(control: &[u8], data: &[u8], gpe: u8, id: &[u8]) -> Vec<u8> {
    sdt(b"ECDT", 1, &[control, data, &1u32.to_le_bytes(), &[gpe], id].concat())
}

fn decoded(bytes: &[u8]) -> Result<Ec, EcRefused> {
    let m = Machine { regions: &[(0x1000, bytes)] };
    let table = Table::open(m, 0x1000, b"ECDT", ECDT_NEEDED).expect("the crafted ECDT opens");
    ecdt(&table)
}

#[test]
fn the_ecdt_names_the_controllers_ports_gpe_and_device() {
    let bytes = table(&gas(1, 8, 0, 0x66), &gas(1, 8, 0, 0x62), 0x6e, b"\\_SB.PCI0.LPCB.EC\0");
    let ec = decoded(&bytes).expect("a controller");
    assert_eq!((ec.command, ec.data, ec.gpe), (0x66, 0x62, 0x6e));
    assert_eq!(&bytes[ec.id], b"\\_SB.PCI0.LPCB.EC");
}

#[test]
fn a_controller_the_ecdt_names_wrongly_is_refused_by_name() {
    let (io, id) = (gas(1, 8, 0, 0x62), b"\\_SB.EC0\0".as_slice());
    for (control, data, refused) in [
        (gas(0, 8, 0, 0xfe00_0000), io.clone(), EcRefused::NotSystemIo { register: EcRegister::Command, space: 0 }),
        (io.clone(), gas(1, 16, 0, 0x62), EcRefused::Width { register: EcRegister::Data, bit_width: 16, bit_offset: 0 }),
        (gas(1, 8, 1, 0x66), io.clone(), EcRefused::Width { register: EcRegister::Command, bit_width: 8, bit_offset: 1 }),
        (gas(1, 8, 0, 0), io.clone(), EcRefused::Address { register: EcRegister::Command, address: 0 }),
        (io.clone(), gas(1, 8, 0, 0x1_0000), EcRefused::Address { register: EcRegister::Data, address: 0x1_0000 }),
    ] {
        assert_eq!(decoded(&table(&control, &data, 3, id)), Err(refused));
    }
    // A path that runs to the table's end, and one with a byte no name holds.
    assert_eq!(decoded(&table(&io, &io, 3, b"\\_SB.EC0")), Err(EcRefused::Id));
    assert_eq!(decoded(&table(&io, &io, 3, b"\\_SB.E C0\0")), Err(EcRefused::Id));
}
