//! An AMD laptop's fixed hardware and reset register, decoded from an extract
//! of its FACP: the machine whose reset register is its `SMI_CMD`, and whose
//! power button is a control method device.
//!
//! **An extract, never the table**: the FACP here is laid out from the fields
//! `fixed_hardware`, `pm1a_control` and `reset_register` read, at their
//! offsets and with the machine's own bytes, and zeros everywhere else, then
//! sealed. The whole table was checked against these fields outside the tree.

mod common;

use common::{sdt, Machine};
use toyos_abi::acpi::Block;
use toyos_acpi::{
    fixed_hardware, pm1a_control, reset_register, FixedHardware, PowerButton, Reset, SmiCmd, Table, FADT_FOR_FIXED_HARDWARE,
};

/// The FACP's fields: (offset, the machine's bytes there). Revision 5, 268
/// bytes long; a Generic Address Structure is space, bit width, bit offset,
/// access size, then the address.
const FACP_LEN: usize = 268;
const FACP_REVISION: u8 = 5;
const FACP_FIELDS: &[(usize, &[u8])] = &[
    (46, &[0x09, 0x00]),
    (48, &[0xb0, 0x00, 0x00, 0x00]),
    (52, &[0xa0, 0xa1, 0x00, 0x00]),
    (56, &[0x00, 0x04, 0x00, 0x00]),
    (64, &[0x04, 0x04, 0x00, 0x00]),
    (80, &[0x20, 0x04, 0x00, 0x00]),
    (88, &[0x04, 0x02]),
    (92, &[0x08, 0x00]),
    (112, &[0xbd, 0xc5, 0x20, 0x00]),
    (116, &[0x01, 0x08, 0x00, 0x01, 0xb0, 0, 0, 0, 0, 0, 0, 0]),
    (128, &[0xfb]),
    (148, &[0x01, 0x20, 0x00, 0x03, 0x00, 0x04, 0, 0, 0, 0, 0, 0]),
    (172, &[0x01, 0x10, 0x00, 0x02, 0x04, 0x04, 0, 0, 0, 0, 0, 0]),
    (220, &[0x01, 0x40, 0x00, 0x04, 0x20, 0x04, 0, 0, 0, 0, 0, 0]),
];

const AT: u64 = 0x9000_0000;

fn fadt() -> Table<Machine<'static>> {
    let mut body = vec![0u8; FACP_LEN - 36];
    for &(at, bytes) in FACP_FIELDS {
        body[at - 36..at - 36 + bytes.len()].copy_from_slice(bytes);
    }
    let bytes: &'static [u8] = Box::leak(sdt(b"FACP", FACP_REVISION, &body).into_boxed_slice());
    let regions: &'static [(u64, &[u8])] = Box::leak(Box::new([(AT, bytes)]));
    Table::open(Machine { regions }, AT, b"FACP", FADT_FOR_FIXED_HARDWARE).expect("a sealed extract")
}

/// `SMI_CMD` 0xb0 with `ACPI_ENABLE` 0xa0 and `ACPI_DISABLE` 0xa1, PM1a
/// events at 0x400, control at 0x404, eight bytes of GPE0 at 0x420, and
/// flags 0x0020c5bd, whose bit 4 makes the power button a control method
/// device.
#[test]
fn the_fadt_names_a_control_method_power_button() {
    let fadt = fadt();
    assert_eq!(
        fixed_hardware(&fadt),
        Ok(FixedHardware {
            sci_int: 9,
            smi_cmd: Some(SmiCmd { port: 0xb0, acpi_enable: 0xa0, acpi_disable: 0xa1, s4bios_req: 0, pstate_cnt: 0, cst_cnt: 0 }),
            pm1a_event: Block { port: 0x400, len: 4 },
            gpe0: Block { port: 0x420, len: 8 },
            power_button: PowerButton::ControlMethod,
        })
    );
    assert_eq!(pm1a_control(&fadt), Ok(Block { port: 0x404, len: 2 }));
}

/// The reset register is SystemIO 0xb0 with `RESET_VALUE` 0xfb: `SMI_CMD`'s
/// own port. So 0xfb is a sixth value the tables give that port a meaning,
/// and is among the bytes kept from whoever asks for a write there; a reset
/// register anywhere else adds none.
#[test]
fn the_reset_register_is_smi_cmd_and_its_value_is_one_the_tables_name() {
    let fadt = fadt();
    let reset = reset_register(&fadt);
    assert_eq!(reset, Reset::Port { port: 0xb0, value: 0xfb });
    let smi_cmd = fixed_hardware(&fadt).expect("the extract decodes").smi_cmd.expect("a port");
    assert_eq!(smi_cmd.named(false, reset), [Some(0xa0), Some(0xa1), None, None, None, Some(0xfb)]);
    assert_eq!(smi_cmd.named(false, Reset::Port { port: 0xcf9, value: 0xfb }), [Some(0xa0), Some(0xa1), None, None, None, None]);
    for elsewhere in [Reset::Absent, Reset::Unsupported, Reset::SystemMemory, Reset::Address(0xb0_0000)] {
        assert_eq!(smi_cmd.named(false, elsewhere)[5], None, "{elsewhere:?}");
    }
}
