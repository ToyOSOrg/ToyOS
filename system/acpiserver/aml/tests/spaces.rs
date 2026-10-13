//! Every address space of Table 5.182 reaches the host: one addressed by
//! offset an access unit at a time, as an [`Address`], and one whose protocol
//! is command- or connection-based (§5.5.2.4) whole, as a
//! [`toyos_aml::Transfer`] with its AccessAs protocol and its Connection.

mod common;

use common::*;
use toyos_aml::{Access, Address, Error, Value};

/// FieldFlags (§20.2.5.2).
const BYTE: u8 = 1;
const DWORD: u8 = 3;
const BUFFER: u8 = 5;

/// `AccessAs (<access type>, <attrib>)` (§20.2.5.2, AccessField).
fn access_as(access_type: u8, attrib: u8) -> Vec<u8> {
    vec![0x01, access_type, attrib]
}

/// An ExtendedAccessField: `AccessAs (<access type>, <attrib> (<length>))`.
fn access_as_extended(access_type: u8, attrib: u8, length: u8) -> Vec<u8> {
    vec![0x03, access_type, attrib, length]
}

/// `Connection (<buffer>)` and `Connection (<name>)` (§20.2.5.2, ConnectField).
fn connection(descriptor: &[u8]) -> Vec<u8> {
    cat(&[&[0x02], &buffer(&int(descriptor.len() as u64), descriptor)])
}

fn connection_named(n: &str) -> Vec<u8> {
    cat(&[&[0x02], &name(n)])
}

fn read(body: &[u8], answer: &[u8], path: &str) -> (Machine, Result<Value, Error>) {
    let (mut i, mut m) = loaded(body);
    m.answer = answer.to_vec();
    m.log.clear();
    let r = i.evaluate(&mut m, path, &[]);
    (m, r)
}

fn transfers(m: &Machine) -> Vec<Event> {
    m.log.iter().filter(|e| matches!(e, Event::Transfer { .. })).cloned().collect()
}

/// §5.5.2.4.7: a GenericSerialBus field is one transfer with its protocol
/// and its connection, its value the buffer the host answered, and a store
/// to it answers the response its buffer became (`BUFF = (FLD = BUFF)`).
/// An AccessAs names its protocol by its byte, or, by the AccessType byte's
/// bits 7:6, AttribBytes, AttribRawBytes or AttribRawProcessBytes of a
/// count; an ExtendedAccessField names both.
#[test]
fn a_generic_serial_bus_field_is_one_transfer_with_its_protocol_and_connection() {
    let descriptor = [0x8E, 0x01, 0x02, 0x03];
    let body = cat(&[
        &op_region("GSB0", 0x09, &int(0x10), &int(0x100)),
        &field(
            "GSB0",
            BUFFER,
            &[
                connection(&descriptor),
                access_as(BUFFER, 0x08),
                unit("WORD", 8),
                access_as(BUFFER | 1 << 6, 4),
                unit("BYT4", 8),
                access_as(BUFFER | 3 << 6, 2),
                unit("RAWP", 8),
                access_as_extended(BUFFER, 0x0E, 16),
                unit("RAWB", 8),
            ],
        ),
        &def_name("BUFF", &buffer(&int(4), &[0, 0, 0x34, 0x12])),
        &method("SEND", 0, &ret(&store(&name("BUFF"), &name("WORD")))),
    ]);
    let answer = [0x00, 0x02, 0xCD, 0xAB];
    for (unit, bit, attrib) in [("\\WORD", 0, (0x08, 0)), ("\\BYT4", 8, (0x0B, 4)), ("\\RAWP", 16, (0x0F, 2)), ("\\RAWB", 24, (0x0E, 16))] {
        let (m, v) = read(&body, &answer, unit);
        assert_eq!(v, Ok(Value::Buffer(answer.to_vec())), "{unit}");
        let expected = Event::Transfer { space: 0x09, region: 0x10, bit, bits: 8, attrib, connection: Some((descriptor.to_vec(), 0)), write: None };
        assert_eq!(transfers(&m), [expected], "{unit}");
    }
    let (m, v) = read(&body, &answer, "\\SEND");
    assert_eq!(v, Ok(Value::Buffer(answer.to_vec())), "the store's value is the response");
    let written = Event::Transfer { space: 0x09, region: 0x10, bit: 0, bits: 8, attrib: (0x08, 0), connection: Some((descriptor.to_vec(), 0)), write: Some(vec![0, 0, 0x34, 0x12]) };
    assert_eq!(transfers(&m), [written]);
}

/// §5.5.2.4.6: a GeneralPurposeIO field's pins follow its Connection,
/// inline or named, from the bit that connection began at; its value is an
/// Integer of its pins, and a store writes them.
#[test]
fn a_general_purpose_io_field_reaches_its_pins_through_its_connection() {
    let (first, second) = ([0x8C, 0x01, 0x0A], [0x8C, 0x01, 0x07]);
    let body = cat(&[
        &def_name("GMOD", &buffer(&int(3), &first)),
        &op_region("GPO2", 0x08, &int(0), &int(1)),
        &field("GPO2", BYTE, &[connection_named("GMOD"), unit("MODE", 2), connection(&second), unit("STAT", 1)]),
        &method("SET", 0, &store(&int(3), &name("MODE"))),
    ]);
    let (m, v) = read(&body, &[0x02], "\\MODE");
    assert_eq!(v, Ok(Value::Integer(2)));
    assert_eq!(transfers(&m), [Event::Transfer { space: 0x08, region: 0, bit: 0, bits: 2, attrib: (0, 0), connection: Some((first.to_vec(), 0)), write: None }]);
    let (m, v) = read(&body, &[0x01], "\\STAT");
    assert_eq!(v, Ok(Value::Integer(1)));
    assert_eq!(transfers(&m), [Event::Transfer { space: 0x08, region: 0, bit: 2, bits: 1, attrib: (0, 0), connection: Some((second.to_vec(), 2)), write: None }]);
    let (m, _) = read(&body, &[], "\\SET");
    assert_eq!(transfers(&m), [Event::Transfer { space: 0x08, region: 0, bit: 0, bits: 2, attrib: (0, 0), connection: Some((first.to_vec(), 0)), write: Some(vec![3]) }]);
}

/// SMBus, IPMI, PlatformRtMechanism and FFixedHW are carried whole, each a
/// BufferAcc field whose value is the host's buffer.
#[test]
fn smbus_ipmi_prm_and_ffh_fields_are_carried_whole() {
    for (space, region) in [(0x04, 0x4200), (0x07, 0x3000), (0x0B, 0x1234), (0x7F, 0)] {
        let body = cat(&[&op_region("REG0", space, &int(region), &int(0x100)), &field("REG0", BUFFER, &[skip(8 * 0xC1), unit("FLD", 8)])]);
        let (m, v) = read(&body, &[0x00, 0x01, 0x5A], "\\FLD");
        assert_eq!(v, Ok(Value::Buffer(vec![0x00, 0x01, 0x5A])), "{space:#x}");
        assert_eq!(transfers(&m), [Event::Transfer { space, region, bit: 8 * 0xC1, bits: 8, attrib: (0, 0), connection: None, write: None }], "{space:#x}");
    }
}

/// SystemCMOS (§5.5.2.4.3) and PCC (§5.5.2.4.8) a byte at a time,
/// PciBarTarget (§5.5.2.4.4) by the function's BAR at the region's offset,
/// its length the BAR's, and an OEM-defined space by offset: each an
/// [`Address`] the host answers.
#[test]
fn every_space_addressed_by_offset_reaches_the_host() {
    let cmos = cat(&[&op_region("CMS0", 0x05, &int(0), &int(0x100)), &field("CMS0", BYTE, &[skip(8 * 0x40), unit("C40", 8)])]);
    let (m, v) = read(&cmos, &[], "\\C40");
    assert_eq!((m.accesses(), v), (vec![Event::Read(Address::SystemCmos(0x40), Access::Byte)], Ok(Value::Integer(0))));
    let wide = cat(&[&op_region("CMS0", 0x05, &int(0), &int(0x100)), &field("CMS0", 0x02, &[unit("CW", 16)])]);
    assert!(matches!(read(&wide, &[], "\\CW").1, Err(Error::Type(_))), "SystemCMOS permits ByteAcc alone (Table 19.34)");

    let bar = |offset: u64| {
        device(
            "PCI0",
            &cat(&[
                &def_name("_BBN", &int(0)),
                &device("DEV2", &cat(&[&def_name("_ADR", &int(0x0002_0000)), &op_region("PBAR", 0x06, &int(offset), &int(4)), &field("PBAR", DWORD, &[skip(8 * 0x20), unit("REG", 32)])])),
            ]),
        )
    };
    let (m, v) = read(&bar(0x10), &[], "\\PCI0.DEV2.REG");
    let at = Address::PciBarTarget { segment: 0, bus: 0, device: 2, function: 0, bar: 0x10, offset: 0x20 };
    assert_eq!((m.accesses(), v), (vec![Event::Read(at, Access::DWord)], Ok(Value::Integer(0))));
    for offset in [0x11, 0x28, 0x0C] {
        assert_eq!(read(&bar(offset), &[], "\\PCI0.DEV2.REG").1, Err(Error::Rule("a PciBarTarget region whose offset is no BAR's (§5.5.2.4.4.1)")), "{offset:#x}");
    }

    let pcc = cat(&[&op_region("PFRM", 0x0A, &int(2), &int(0x10C)), &field("PFRM", BYTE, &[skip(32), unit("FLGS", 16)])]);
    let (m, _) = read(&pcc, &[], "\\FLGS");
    assert_eq!(m.accesses(), [4, 5].map(|offset| Event::Read(Address::Pcc { subspace: 2, offset }, Access::Byte)));

    let oem = cat(&[&op_region("OEM0", 0x80, &int(0x1000), &int(0x10)), &field("OEM0", DWORD, &[unit("O", 32)])]);
    let (m, _) = read(&oem, &[], "\\O");
    assert_eq!(m.accesses(), [Event::Read(Address::Oem { space: 0x80, offset: 0x1000 }, Access::DWord)]);
}

/// A reserved space (Table 5.182), BufferAcc where the space takes no
/// buffer, and a BankField of a carried space break a rule, and nothing
/// reaches the host.
#[test]
fn a_reserved_space_buffer_acc_by_offset_and_a_bank_of_a_carried_space_are_refused() {
    let reserved = cat(&[&op_region("RES0", 0x0C, &int(0), &int(4)), &field("RES0", BYTE, &[unit("R", 8)])]);
    let memory = cat(&[&op_region("MEM0", 0x00, &int(0x1000), &int(4)), &field("MEM0", BUFFER, &[unit("M", 8)])]);
    let bank = cat(&[
        &op_region("MEM0", 0x00, &int(0x1000), &int(4)),
        &field("MEM0", BYTE, &[unit("BNK", 8)]),
        &op_region("SMB0", 0x04, &int(0), &int(0x100)),
        &bank_field("SMB0", "BNK", &int(1), BUFFER, &[unit("B", 8)]),
    ]);
    for (body, path, why) in [
        (reserved, "\\R", "a region of a reserved address space (Table 5.182)"),
        (memory, "\\M", "BufferAcc in a space addressed by offset, which takes no buffer (§19.6.47)"),
        (bank, "\\B", "a BankField of a space whose protocol is command- or connection-based (§5.5.2.4)"),
    ] {
        let (m, v) = read(&body, &[], path);
        assert_eq!(v, Err(Error::Rule(why)), "{path}");
        assert_eq!((m.accesses(), transfers(&m)), (vec![], vec![]), "{path}");
    }
}

/// A reference to an unnamed object handed to the caller is what it referred
/// to: a package's element by Index; a method's LocalX is gone with it; and
/// no caller passes one back.
#[test]
fn a_reference_to_an_unnamed_object_is_handed_over_as_what_it_refers_to() {
    let body = cat(&[
        &def_name("PKG", &package(&[int(5), int(7)])),
        &method("ELEM", 0, &ret(&index(&name("PKG"), &int(1), &[0x00]))),
        &method("LOCL", 0, &cat(&[&store(&int(9), &local(0)), &ret(&ref_of(&local(0)))])),
        &method("TAKE", 1, &ret(&arg(0))),
    ]);
    let (mut i, mut m) = loaded(&body);
    assert_eq!(i.evaluate(&mut m, "\\ELEM", &[]), Ok(Value::Unnamed(Box::new(Value::Integer(7)))));
    assert_eq!(i.evaluate(&mut m, "\\LOCL", &[]), Err(Error::NotFound("a LocalX or ArgX its method's exit destroyed".into())));
    let passed = Value::Unnamed(Box::new(Value::Integer(7)));
    assert_eq!(i.evaluate(&mut m, "\\TAKE", &[passed]), Err(Error::Type("an argument that is a reference to an unnamed object, which only AML makes")));
}

/// ACPI 6.5 reserves `0x5B 0x2A`, once Unload (Table 20.2), and a block a
/// Load made has no handle to name: it is malformed, as a statement and as
/// an argument.
#[test]
fn unload_is_a_reserved_opcode() {
    assert!(matches!(returns(&[0x5B, 0x2A, 0x60]), Err(Error::Malformed { .. })));
    assert!(matches!(returns(&ret(&[0x5B, 0x2A, 0x60])), Err(Error::Malformed { .. })));
}
