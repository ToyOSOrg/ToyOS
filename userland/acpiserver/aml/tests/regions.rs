//! Field units over operation regions and buffer fields over buffers
//! (§5.5.2.4, §19.6.47, §19.6.63, §19.6.7, §19.6.18-23, Table 19.7).

mod common;

use common::*;
use toyos_aml::{Access, Address, Error, Interpreter, Value};

/// FieldFlags (§20.2.5.2).
const BYTE: u8 = 1;
const WORD: u8 = 2;
const DWORD: u8 = 3;
const ANY: u8 = 0;
const LOCK: u8 = 0x10;
const ONES_RULE: u8 = 0x20;
const ZEROS_RULE: u8 = 0x40;

fn mem(a: u64) -> Address {
    Address::Memory(a)
}

fn store_into(body: &[u8], field: &str, v: &[u8]) -> (Machine, Result<Value, Error>) {
    let (mut i, mut m) = loaded(&cat(&[body, &method("SET", 0, &store(v, &name(field)))]));
    m.log.clear();
    let r = i.evaluate(&mut m, "\\SET", &[]);
    (m, r)
}

fn read(body: &[u8], prime: &[(Address, &[u8])], field: &str) -> (Machine, Result<Value, Error>) {
    let (mut i, mut m) = loaded(body);
    for (a, v) in prime {
        m.poke(*a, v);
    }
    m.log.clear();
    let r = i.evaluate(&mut m, field, &[]);
    (m, r)
}

fn memory_region(flags: u8, units: &[Vec<u8>]) -> Vec<u8> {
    cat(&[&op_region("MEM0", 0x00, &int(0x1000), &int(0x10)), &field("MEM0", flags, units)])
}

#[test]
fn a_field_is_read_shifted_and_masked_from_its_units() {
    let body = memory_region(BYTE, &[unit("LO", 4), unit("HI", 4), unit("NEXT", 8), unit("WIDE", 16)]);
    let prime: &[(Address, &[u8])] = &[(mem(0x1000), &[0xA5, 0x3C, 0x34, 0x12])];
    assert_eq!(read(&body, prime, "\\LO").1, Ok(Value::Integer(0x5)));
    assert_eq!(read(&body, prime, "\\HI").1, Ok(Value::Integer(0xA)));
    assert_eq!(read(&body, prime, "\\NEXT").1, Ok(Value::Integer(0x3C)));
    let (m, v) = read(&body, prime, "\\WIDE");
    assert_eq!(v, Ok(Value::Integer(0x1234)));
    // ByteAcc: one byte access per unit.
    assert_eq!(m.accesses(), vec![Event::Read(mem(0x1002), Access::Byte), Event::Read(mem(0x1003), Access::Byte)]);
}

/// §19.6.47: "when those 4 bits are modified the UpdateRule specifies how
/// the other 12 bits are treated".
#[test]
fn the_update_rule_completes_a_unit_the_field_covers_in_part() {
    let units = [skip(4), unit("MID", 4)];
    let (m, r) = {
        let (mut i, mut m) = loaded(&cat(&[&memory_region(WORD, &units), &method("SET", 0, &store(&int(0xF), &name("MID")))]));
        m.poke(mem(0x1000), &[0x21, 0x43]);
        m.log.clear();
        let r = i.evaluate(&mut m, "\\SET", &[]);
        (m, r)
    };
    r.unwrap();
    assert_eq!(m.accesses(), vec![Event::Read(mem(0x1000), Access::Word), Event::Write(mem(0x1000), Access::Word, 0x43F1)]);

    let (m, r) = store_into(&memory_region(WORD | ONES_RULE, &units), "MID", &int(0));
    r.unwrap();
    assert_eq!(m.accesses(), vec![Event::Write(mem(0x1000), Access::Word, 0xFF0F)]);

    let (m, r) = store_into(&memory_region(WORD | ZEROS_RULE, &units), "MID", &int(0xF));
    r.unwrap();
    assert_eq!(m.accesses(), vec![Event::Write(mem(0x1000), Access::Word, 0x00F0)]);

    // A unit the field covers whole is written without a read.
    let (m, r) = store_into(&memory_region(BYTE, &[unit("ALL", 8)]), "ALL", &int(0x1FF));
    r.unwrap();
    assert_eq!(m.accesses(), vec![Event::Write(mem(0x1000), Access::Byte, 0xFF)]);
}

#[test]
fn an_access_type_sets_the_unit_and_anyacc_takes_the_narrowest_natural_one() {
    let (m, v) = read(&memory_region(DWORD, &[skip(8), unit("B1", 8)]), &[(mem(0x1000), &[0, 0x77, 0, 0])], "\\B1");
    assert_eq!(v, Ok(Value::Integer(0x77)));
    assert_eq!(m.accesses(), vec![Event::Read(mem(0x1000), Access::DWord)]);

    let (m, _) = read(&memory_region(ANY, &[skip(16), unit("W", 16)]), &[], "\\W");
    assert_eq!(m.accesses(), vec![Event::Read(mem(0x1002), Access::Word)]);
    let (m, _) = read(&memory_region(ANY, &[skip(8), unit("Q", 32)]), &[], "\\Q");
    assert_eq!(m.accesses(), vec![Event::Read(mem(0x1000), Access::QWord)]);
    let (m, _) = read(&memory_region(ANY, &[skip(56), unit("X", 16)]), &[], "\\X");
    assert_eq!(m.accesses(), vec![Event::Read(mem(0x1007), Access::Byte), Event::Read(mem(0x1008), Access::Byte)]);
    // A unit that would hold the field whole but runs past its region is not
    // taken: the last two bytes of a 13-byte region are read as bytes.
    let odd = cat(&[&op_region("ODD", 0x00, &int(0x2000), &int(13)), &field("ODD", ANY, &[skip(88), unit("END", 16)])]);
    let (m, v) = read(&odd, &[(mem(0x200B), &[0x34, 0x12])], "\\END");
    assert_eq!(v, Ok(Value::Integer(0x1234)));
    assert_eq!(m.accesses(), vec![Event::Read(mem(0x200B), Access::Byte), Event::Read(mem(0x200C), Access::Byte)]);
}

/// §19.6.47: "If the FieldUnit is larger than the size of an Integer, it
/// will be treated as a Buffer."
#[test]
fn a_field_wider_than_an_integer_is_a_buffer() {
    let (_, v) = read(&memory_region(BYTE, &[unit("BIG", 72)]), &[(mem(0x1000), &[1, 2, 3, 4, 5, 6, 7, 8, 9])], "\\BIG");
    assert_eq!(v, Ok(Value::Buffer(vec![1, 2, 3, 4, 5, 6, 7, 8, 9])));
    // A buffer longer than the field is written in pieces of its size, lower
    // first; a string a character at a time (Table 19.7).
    let (m, r) = store_into(&memory_region(BYTE, &[unit("TWO", 16)]), "TWO", &buffer(&int(3), &[1, 2, 3]));
    r.unwrap();
    assert_eq!(
        m.accesses(),
        vec![
            Event::Write(mem(0x1000), Access::Byte, 1),
            Event::Write(mem(0x1001), Access::Byte, 2),
            Event::Write(mem(0x1000), Access::Byte, 3),
            Event::Write(mem(0x1001), Access::Byte, 0),
        ]
    );
    let (m, r) = store_into(&memory_region(BYTE, &[unit("CH", 8)]), "CH", &string("AB"));
    r.unwrap();
    assert_eq!(m.accesses(), vec![Event::Write(mem(0x1000), Access::Byte, 0x41), Event::Write(mem(0x1000), Access::Byte, 0x42)]);
}

/// §19.6.47: "an access type of WordAcc cannot read the last byte of an
/// odd-length operation region".
#[test]
fn an_access_past_its_region_is_refused() {
    let body = cat(&[&op_region("ODD", 0x00, &int(0), &int(3)), &field("ODD", WORD, &[skip(16), unit("LAST", 8)])]);
    let (m, v) = read(&body, &[], "\\LAST");
    assert!(matches!(v, Err(Error::Rule(_))));
    assert_eq!(m.accesses(), vec![]);
}

#[test]
fn system_io_and_the_embedded_controller_are_addressed_by_offset() {
    let io = cat(&[&op_region("IO", 0x01, &int(0x62), &int(5)), &field("IO", BYTE, &[skip(32), unit("CMD", 8)])]);
    let (m, _) = read(&io, &[], "\\CMD");
    assert_eq!(m.accesses(), vec![Event::Read(Address::Io(0x66), Access::Byte)]);

    let ec = cat(&[&op_region("ECOR", 0x03, &int(0), &int(0x100)), &field("ECOR", ANY, &[skip(0xA0 * 8), unit("TEMP", 16)])]);
    let (m, v) = read(&ec, &[(Address::EmbeddedControl(0xA0), &[0x2C, 0x01])], "\\TEMP");
    assert_eq!(v, Ok(Value::Integer(0x12C)));
    assert_eq!(
        m.accesses(),
        vec![Event::Read(Address::EmbeddedControl(0xA0), Access::Byte), Event::Read(Address::EmbeddedControl(0xA1), Access::Byte)]
    );
    // Table 19.34: EmbeddedControl permits ByteAcc only.
    let wide = cat(&[&op_region("ECOR", 0x03, &int(0), &int(0x100)), &field("ECOR", WORD, &[unit("W", 16)])]);
    assert!(matches!(read(&wide, &[], "\\W").1, Err(Error::Type(_))));
    // §12: the controller's space is 256 bytes.
    let past = cat(&[&op_region("ECOR", 0x03, &int(0xFF), &int(2)), &field("ECOR", BYTE, &[skip(8), unit("P", 8)])]);
    assert!(matches!(read(&past, &[], "\\P").1, Err(Error::Rule(_))));
}

/// §6.1.1, §6.5.5, §6.5.6: the device's `_ADR`, the host bridge's `_BBN` and
/// `_SEG`.
#[test]
fn a_pci_config_region_addresses_its_devices_function() {
    let lpc = |bridge: &[u8]| {
        scope(
            "\\_SB",
            &device(
                "PCI0",
                &cat(&[
                    bridge,
                    &device(
                        "LPCB",
                        &cat(&[
                            &method("_ADR", 0, &ret(&int(0x001F_0003))),
                            &op_region("LPCR", 0x02, &int(0x40), &int(0x10)),
                            &field("LPCR", BYTE, &[skip(8), unit("R41", 8)]),
                        ]),
                    ),
                ]),
            ),
        )
    };
    let bridge = cat(&[&def_name("_BBN", &int(0x80)), &def_name("_SEG", &int(1))]);
    let (m, _) = read(&lpc(&bridge), &[], "\\_SB.PCI0.LPCB.R41");
    let at = Address::PciConfig { segment: 1, bus: 0x80, device: 0x1F, function: 3, offset: 0x41 };
    assert_eq!(m.accesses(), vec![Event::Read(at, Access::Byte)]);
    let (_, v) = read(&lpc(&[]), &[], "\\_SB.PCI0.LPCB.R41");
    assert!(matches!(v, Err(Error::Unsupported(_))));
}

/// A device below a bridge is on the bus the bridge's Secondary Bus Number
/// register names (PCI-to-PCI Bridge Architecture Specification 1.2,
/// §3.2.5.4), read from the bridge on the bus above it.
#[test]
fn a_pci_config_region_below_a_bridge_is_on_its_secondary_bus() {
    let endpoint = cat(&[
        &def_name("_ADR", &int(0x0000_0001)),
        &op_region("CFG", 0x02, &int(0), &int(0x10)),
        &field("CFG", BYTE, &[unit("VEN", 8)]),
    ]);
    let port = cat(&[&def_name("_ADR", &int(0x001C_0002)), &device("PXSX", &endpoint)]);
    let body = scope("\\_SB", &device("PCI0", &cat(&[&def_name("_BBN", &int(0x40)), &device("RP03", &port)])));
    let secondary = Address::PciConfig { segment: 0, bus: 0x40, device: 0x1C, function: 2, offset: 0x19 };
    let (m, _) = read(&body, &[(secondary, &[0x45])], "\\_SB.PCI0.RP03.PXSX.VEN");
    let at = Address::PciConfig { segment: 0, bus: 0x45, device: 0, function: 1, offset: 0 };
    assert_eq!(m.accesses(), vec![Event::Read(secondary, Access::Byte), Event::Read(at, Access::Byte)]);
}

/// The example of §19.6.63: FET3, the high bit at indexed offset 0x2F.
#[test]
fn an_index_field_writes_its_offset_then_reaches_the_data() {
    let body = cat(&[
        &op_region("GIO0", 0x01, &int(0x125), &int(0x100)),
        &field("GIO0", BYTE | ZEROS_RULE, &[unit("IDX0", 8), unit("DAT0", 8)]),
        &index_field("IDX0", "DAT0", BYTE, &[unit("FET0", 1), unit("FET1", 1), skip((0x2F * 8) - 2), skip(7), unit("FET3", 1)]),
        &method("SET", 0, &store(&int(0), &name("FET3"))),
    ]);
    let (mut i, mut m) = loaded(&body);
    m.poke(Address::Io(0x126), &[0xFF]);
    m.log.clear();
    i.evaluate(&mut m, "\\SET", &[]).unwrap();
    assert_eq!(
        m.accesses(),
        vec![
            Event::Write(Address::Io(0x125), Access::Byte, 0x2F),
            Event::Read(Address::Io(0x126), Access::Byte),
            Event::Write(Address::Io(0x125), Access::Byte, 0x2F),
            Event::Write(Address::Io(0x126), Access::Byte, 0x7F),
        ]
    );
}

/// §19.6.7: the bank value is written before the field is reached.
#[test]
fn a_bank_field_selects_its_bank_first() {
    let body = cat(&[
        &op_region("GIO0", 0x01, &int(0x125), &int(0x100)),
        &field("GIO0", BYTE, &[unit("GLB1", 1), unit("GLB2", 1), skip(6), unit("BNK1", 4)]),
        &bank_field("GIO0", "BNK1", &int(1), BYTE, &[skip(0x30 * 8), unit("BLVL", 7), unit("BAC", 1)]),
    ]);
    let (m, _) = read(&body, &[(Address::Io(0x126), &[0xF0])], "\\BLVL");
    assert_eq!(
        m.accesses(),
        vec![
            Event::Read(Address::Io(0x126), Access::Byte),
            Event::Write(Address::Io(0x126), Access::Byte, 0xF1),
            Event::Read(Address::Io(0x155), Access::Byte),
        ]
    );
}

#[test]
fn a_lock_field_holds_the_global_lock_across_its_access() {
    let (m, _) = read(&memory_region(BYTE | LOCK, &[unit("L", 16)]), &[], "\\L");
    assert_eq!(
        m.log,
        vec![
            Event::GlobalLock(true),
            Event::Read(mem(0x1000), Access::Byte),
            Event::Read(mem(0x1001), Access::Byte),
            Event::GlobalLock(false),
        ]
    );
}

#[test]
fn a_host_refusal_and_a_space_not_carried_are_refused_by_name() {
    let (mut i, mut m) = loaded(&memory_region(BYTE, &[unit("A", 8)]));
    m.refuse = true;
    assert_eq!(i.evaluate(&mut m, "\\A", &[]), Err(Error::Host("refused".into())));
    let smbus = cat(&[&op_region("SMB0", 0x04, &int(0), &int(0x100)), &field("SMB0", BYTE, &[unit("S", 8)])]);
    assert_eq!(read(&smbus, &[], "\\S").1, Err(Error::Unsupported("the SMBus address space")));
    // A reserved space or flag loads, and is refused where an access needs
    // its meaning; reserved bit 7 means nothing and is ignored.
    let reserved = cat(&[&op_region("RSV", 0x0C, &int(0), &int(1)), &field("RSV", BYTE, &[unit("R", 8)])]);
    assert_eq!(read(&reserved, &[], "\\R").1, Err(Error::Unsupported("a reserved address space (Table 5.182)")));
    assert!(matches!(read(&memory_region(0x06, &[unit("A", 8)]), &[], "\\A").1, Err(Error::Rule(_))));
    let (m, r) = store_into(&memory_region(BYTE | 0x60, &[unit("A", 4)]), "A", &int(1));
    assert!(matches!(r, Err(Error::Rule(_))));
    assert_eq!(m.accesses(), vec![]);
    assert!(read(&memory_region(BYTE | 0x80, &[unit("A", 8)]), &[], "\\A").1.is_ok());
}

/// §19.6.18-23 and Table 19.7: buffer fields see and change their buffer.
#[test]
fn buffer_fields_read_and_write_their_buffer() {
    let create = |op: &[u8], at: u64, n: &str| cat(&[op, &name("BUF"), &int(at), &name(n)]);
    let body = cat(&[
        &def_name("BUF", &buffer(&int(12), &[0x81, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])),
        &create(&[0x8D], 7, "BIT7"),
        &create(&[0x8C], 1, "BYT1"),
        &create(&[0x8B], 2, "WRD2"),
        &create(&[0x8A], 4, "DWD4"),
        &create(&[0x8F], 4, "QWD4"),
        &cat(&[&[0x5B, 0x13], &name("BUF"), &int(0), &int(96), &name("ALL")]),
        &method(
            "SET",
            0,
            &cat(&[
                &store(&int(0xFF), &name("BYT1")),
                &store(&int(0x1_2345), &name("WRD2")),
                &store(&string("AB"), &name("DWD4")),
            ]),
        ),
    ]);
    let (mut i, mut m) = loaded(&body);
    assert_eq!(i.evaluate(&mut m, "\\BIT7", &[]), Ok(Value::Integer(1)));
    i.evaluate(&mut m, "\\SET", &[]).unwrap();
    assert_eq!(i.evaluate(&mut m, "\\BUF", &[]), Ok(Value::Buffer(vec![0x81, 0xFF, 0x45, 0x23, 0x41, 0x42, 0, 0, 0, 0, 0, 0])));
    assert_eq!(i.evaluate(&mut m, "\\QWD4", &[]), Ok(Value::Integer(0x4241)));
    assert_eq!(i.evaluate(&mut m, "\\ALL", &[]), Ok(Value::Buffer(vec![0x81, 0xFF, 0x45, 0x23, 0x41, 0x42, 0, 0, 0, 0, 0, 0])));
    let mut m = Machine::default();
    let past = cat(&[&def_name("BUF", &buffer(&int(4), &[])), &cat(&[&[0x8A], &name("BUF"), &int(1), &name("X")])]);
    assert!(matches!(Interpreter::new().load_bytes(&mut m, &dsdt(&past)), Err(Error::Rule(_))));
    let none = cat(&[&def_name("BUF", &buffer(&int(4), &[])), &cat(&[&[0x5B, 0x13], &name("BUF"), &int(0), &int(0), &name("X")])]);
    assert!(matches!(Interpreter::new().load_bytes(&mut m, &dsdt(&none)), Err(Error::Rule(_))));
}

#[test]
fn copy_object_into_a_field_writes_an_integer_or_buffer_alone() {
    let (m, r) = {
        let (mut i, mut m) = loaded(&cat(&[
            &memory_region(BYTE, &[unit("A", 8)]),
            &method("CPY", 0, &copy_object(&int(0x5A), &name("A"))),
            &method("BAD", 0, &copy_object(&string("x"), &name("A"))),
        ]));
        m.log.clear();
        let r = i.evaluate(&mut m, "\\CPY", &[]);
        assert!(matches!(i.evaluate(&mut m, "\\BAD", &[]), Err(Error::Type(_))));
        (m, r)
    };
    r.unwrap();
    assert_eq!(m.accesses(), vec![Event::Write(mem(0x1000), Access::Byte, 0x5A)]);
}

/// A source that is not a Buffer converts to a new one (§19.6.18-23), and an
/// explicit conversion into a byte field takes an Integer or a Buffer alone
/// (Table 19.8).
#[test]
fn a_buffer_field_over_a_conversion_and_a_conversion_into_one() {
    let (mut i, mut m) = loaded(&cat(&[
        &def_name("BUF", &buffer(&int(2), &[])),
        &method("SRC", 0, &cat(&[&cat(&[&[0x8C], &string("AB"), &int(1), &name("CH")]), &ret(&name("CH"))])),
        &method("CONV", 0, &cat(&[&[0x96], &string("Z"), &index(&name("BUF"), &int(0), &[0x00])])),
        &method("TOHX", 0, &cat(&[&[0x98], &int(1), &index(&name("BUF"), &int(0), &[0x00])])),
    ]));
    assert_eq!(i.evaluate(&mut m, "\\SRC", &[]), Ok(Value::Integer(0x42)));
    i.evaluate(&mut m, "\\CONV", &[]).unwrap();
    assert_eq!(i.evaluate(&mut m, "\\BUF", &[]), Ok(Value::Buffer(vec![0x5A, 0])));
    assert!(matches!(i.evaluate(&mut m, "\\TOHX", &[]), Err(Error::Type(_))));
}

/// BLOCKER 8: an address word firmware chose that its form cannot hold is
/// refused, never truncated.
#[test]
fn an_address_out_of_its_form_is_refused() {
    let lpc = |bbn: u64, seg: u64, adr: u64, offset: u64| {
        scope(
            "\\_SB",
            &device(
                "PCI0",
                &cat(&[
                    &def_name("_BBN", &int(bbn)),
                    &def_name("_SEG", &int(seg)),
                    &device(
                        "DEV",
                        &cat(&[
                            &def_name("_ADR", &int(adr)),
                            &op_region("CFG", 0x02, &int(offset), &int(0x10)),
                            &field("CFG", BYTE, &[unit("R0", 8)]),
                        ]),
                    ),
                ]),
            ),
        )
    };
    let r0 = |bbn, seg, adr, offset| read(&lpc(bbn, seg, adr, offset), &[], "\\_SB.PCI0.DEV.R0").1;
    assert!(r0(0, 0, 0x001F_0000, 0).is_ok());
    assert!(matches!(r0(0x100, 0, 0x001F_0000, 0), Err(Error::Rule(_))));
    assert!(matches!(r0(0, 0x1_0000, 0x001F_0000, 0), Err(Error::Rule(_))));
    assert!(matches!(r0(0, 0, 0x0020_0000, 0), Err(Error::Rule(_))));
    assert!(matches!(r0(0, 0, 0x001F_0008, 0), Err(Error::Rule(_))));
    assert!(matches!(r0(0, 0, 0x001F_0000, 0x1000), Err(Error::Rule(_))));
    let io = |base: u64| cat(&[&op_region("IO", 0x01, &int(base), &int(4)), &field("IO", BYTE, &[unit("P", 8)])]);
    assert!(read(&io(0xFFFF), &[], "\\P").1.is_ok());
    assert!(matches!(read(&io(0x1_0000), &[], "\\P").1, Err(Error::Rule(_))));
}

