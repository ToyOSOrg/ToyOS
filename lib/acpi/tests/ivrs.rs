//! The IVRS: QEMU's own, decoded against what `iasl -d` read off the same
//! bytes, and every block, every device entry and every refusal over crafted
//! ones.

mod common;

use common::{declare_len, reseal, rsdp, sdt, xsdt, Machine};
use toyos_acpi::{
    ivrs, DeviceEntry, Features, IvInfo, Ivmd, Ivrs, IvrsBlock, IvrsRefused, Requesters, TableError, Uid, Unit,
};

const RSDP_AT: u64 = 0x800;
const XSDT_AT: u64 = 0x1000;
const TABLE_AT: u64 = 0x9000;

/// QEMU 11.1.1's IVRS for `-device amd-iommu,intremap=off,dma-remap=on` on
/// a q35 with a VGA, an xHCI and an NVMe controller (`SOURCE` beside it).
const QEMU: &[u8] = include_bytes!("../fixtures/qemu-11.1.1-amdvi/ivrs.bin");

/// `bytes` at [`TABLE_AT`] under an RSDP and an XSDT naming it, opened.
fn judged<T>(bytes: &[u8], ask: impl FnOnce(&Ivrs<Machine<'_>>) -> T) -> Result<T, IvrsRefused> {
    let head = rsdp(XSDT_AT, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(RSDP_AT, &head), (XSDT_AT, &root), (TABLE_AT, bytes)];
    ivrs(Machine { regions }, RSDP_AT).map(|table| ask(&table))
}

fn refusal(bytes: &[u8]) -> Option<IvrsRefused> {
    judged(bytes, |_| ()).err()
}

fn blocks(bytes: &[u8]) -> Vec<IvrsBlock> {
    judged(bytes, |t| t.blocks().collect()).expect("a table every block of which holds")
}

/// Every unit's device entries, each walk to its end or its refusal.
fn entries(bytes: &[u8]) -> Vec<Vec<Result<DeviceEntry, IvrsRefused>>> {
    judged(bytes, |t| {
        t.blocks()
            .filter_map(|b| match b {
                IvrsBlock::Unit(unit) => Some(t.devices(&unit).collect()),
                _ => None,
            })
            .collect()
    })
    .expect("a table every block of which holds")
}

// --- QEMU's ---------------------------------------------------------------

/// The unit both blocks describe: `amd-iommu` is function 00:02.0, its
/// capability at 0x40 and its registers at `AMDVI_BASE_ADDR`.
fn qemu_unit(at: usize, kind: u8, flags: u8, features: Features) -> Unit {
    Unit { at, kind, flags, device: 0x10, capability: 0x40, base: 0xfed8_0000, segment: 0, info: 0, features }
}

/// `iasl -d`: revision 1, `Virtualization Info : 00002801`, an IVHD 10h at
/// 30h with flags D1 and `Feature Reporting : 00000044`, an IVHD 11h at 68h
/// with flags 11 and `EFR Image : 00000000000029D3`. 10h and 11h name one
/// unit, so the 11h is the unit and the 10h is superseded, whatever order
/// they come in.
#[test]
fn qemus_table_is_one_unit_described_twice_and_answered_once_at_its_higher_type() {
    let (revision, info) = judged(QEMU, |t| (t.revision(), t.info())).expect("the IVRS QEMU published");
    assert_eq!(revision, 1);
    assert_eq!(info, IvInfo { raw: 0x2801, efr_images: true, dma_remap: false, physical_bits: 40, virtual_bits: 0 });
    assert_eq!(
        blocks(QEMU),
        [
            IvrsBlock::Superseded(qemu_unit(0x30, 0x10, 0xd1, Features::Reported(0x44))),
            IvrsBlock::Unit(qemu_unit(0x68, 0x11, 0x11, Features::Image { attributes: 0, efr: 0x29d3, efr2: 0 })),
        ]
    );
}

/// `Device Entry: Select One Device` eight times in each block: 00:00.0,
/// 00:01.0, the unit's own 00:02.0, the xHCI's 00:03.0, the NVMe
/// controller's 00:04.0, and the ICH9's 00:1f.0, .2 and .3.
#[test]
fn qemus_unit_serves_every_function_of_bus_zero_by_one_select_each() {
    let select = |id| Ok(DeviceEntry::Select { id, data: 0 });
    let functions: Vec<_> = [0x00, 0x08, 0x10, 0x18, 0x20, 0xf8, 0xfa, 0xfb].into_iter().map(select).collect();
    assert_eq!(entries(QEMU), [functions.as_slice()]);
    let superseded = judged(QEMU, |t| {
        let IvrsBlock::Superseded(unit) = t.blocks().next().expect("a first block") else { panic!("not superseded") };
        t.devices(&unit).collect::<Vec<_>>()
    });
    assert_eq!(superseded, Ok(functions));
}

// --- crafted --------------------------------------------------------------

/// An IVHD of `kind` over `entries`: unit `device` of segment 0, its
/// capability at 0x40, its registers at `base`.
fn ivhd(kind: u8, device: u16, base: u64, entries: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![kind, 0xa5, 0, 0];
    b.extend(device.to_le_bytes());
    b.extend(0x40u16.to_le_bytes());
    b.extend(base.to_le_bytes());
    b.extend(0u16.to_le_bytes());
    b.extend(0x1f03u16.to_le_bytes());
    if kind == 0x10 {
        b.extend(0x8000_0044u32.to_le_bytes());
    } else {
        b.extend(1u32.to_le_bytes());
        b.extend(0x0123_4567_89ab_cdefu64.to_le_bytes());
        b.extend(2u64.to_le_bytes());
    }
    b.extend(entries.concat());
    let len = b.len() as u16;
    b[2..4].copy_from_slice(&len.to_le_bytes());
    b
}

fn ivmd(kind: u8, flags: u8, device: u16, aux: u16, start: u64, length: u64) -> Vec<u8> {
    let mut b = vec![kind, flags, 32, 0];
    b.extend(device.to_le_bytes());
    b.extend(aux.to_le_bytes());
    b.extend(3u16.to_le_bytes());
    b.extend([0u8; 6]);
    b.extend(start.to_le_bytes());
    b.extend(length.to_le_bytes());
    b
}

/// An IVRS over `blocks`, its IVinfo saying EFR images, pre-boot DMA
/// protection, 48 physical and 64 virtual address bits.
fn table(blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut body = (0x0020_3003u32).to_le_bytes().to_vec();
    body.extend([0u8; 8]);
    body.extend(blocks.concat());
    sdt(b"IVRS", 2, &body)
}

fn e4(kind: u8, id: u16, data: u8) -> Vec<u8> {
    let [lo, hi] = id.to_le_bytes();
    vec![kind, lo, hi, data]
}

fn e8(kind: u8, id: u16, data: u8, word: u32) -> Vec<u8> {
    let mut e = e4(kind, id, data);
    e.extend(word.to_le_bytes());
    e
}

fn hid(id: u16, data: u8, hid: &[u8; 8], cid: &[u8; 8], format: u8, uid: &[u8]) -> Vec<u8> {
    let mut e = e4(0xf0, id, data);
    e.extend(hid);
    e.extend(cid);
    e.extend([format, uid.len() as u8]);
    e.extend(uid);
    e
}

/// Every entry type: each decoded one, padding of both lengths, and a
/// reserved type of each.
fn every_entry() -> Vec<Vec<u8>> {
    vec![
        e4(0x01, 0, 0xd7),
        e4(0x00, 0xffff, 0xff),
        e4(0x02, 0x0018, 0x01),
        e4(0x03, 0x0100, 0x40),
        e4(0x04, 0x01ff, 0xee),
        e8(0x42, 0x0200, 0x02, 0x0003_0100),
        e8(0x43, 0x0300, 0x04, 0x0003_0200),
        e4(0x02, 0x00a0, 0x00),
        e4(0x04, 0x03ff, 0x00),
        e8(0x46, 0x0400, 0x08, 0x8000_0000),
        e8(0x47, 0x0500, 0x10, 0x4000_0001),
        e4(0x04, 0x05ff, 0x00),
        e8(0x40, 0, 0, 0xffff_ffff),
        e8(0x48, 0, 0xd7, 0x0100_a021),
        e8(0x48, 0, 0x00, 0x0200_a000),
        e4(0x05, 0x0600, 0x00),
        e8(0x49, 0x0700, 0x00, 0),
        hid(0x00a5, 0x40, b"AMDI0010", b"\0\0\0\0\0\0\0\0", 1, &[3]),
        hid(0x00a6, 0x00, b"AMDI0020", b"PNP0C50\0", 2, b"\\_SB.I2CA"),
        hid(0x00a7, 0x00, b"AMDI0030", b"\0\0\0\0\0\0\0\0", 0, &[]),
    ]
}

/// Units 00:00.2 and 00:01.2: the first at 10h and 40h, the second at 10h
/// alone; and an IVMD of each type and a block of a type not decoded.
fn crafted() -> Vec<u8> {
    table(&[
        ivhd(0x10, 0x0002, 0xfeb8_0000, &[e4(0x01, 0, 0)]),
        ivmd(0x20, 0x09, 0, 0, 0x0009_d000, 0x1000),
        ivhd(0x40, 0x0002, 0xfeb8_0000, &every_entry()),
        ivhd(0x10, 0x000a, 0xfec0_0000, &[e4(0x02, 0x0008, 0)]),
        ivmd(0x21, 0x06, 0x00a0, 0, 0x7f00_0000, 0x10_0000),
        ivmd(0x22, 0x01, 0x0100, 0x01ff, 0xfee0_0000, 0x10_0000),
        vec![0x30, 0, 8, 0, 0, 0, 0, 0],
    ])
}

/// The offsets `crafted` puts its blocks at.
const FIRST_AT: usize = 48;
const SECOND_AT: usize = FIRST_AT + 28 + 32;

fn unit(at: usize, kind: u8, device: u16, base: u64) -> Unit {
    let features = if kind == 0x10 {
        Features::Reported(0x8000_0044)
    } else {
        Features::Image { attributes: 1, efr: 0x0123_4567_89ab_cdef, efr2: 2 }
    };
    Unit { at, kind, flags: 0xa5, device, capability: 0x40, base, segment: 0, info: 0x1f03, features }
}

#[test]
fn every_block_type_decodes_and_a_unit_is_answered_once_at_its_highest_type() {
    let t = crafted();
    let second_len = 40 + every_entry().concat().len();
    let third_at = SECOND_AT + second_len;
    assert_eq!(judged(&t, |t| t.info()).map(|i| (i.efr_images, i.dma_remap, i.physical_bits, i.virtual_bits)), Ok((true, true, 48, 64)));
    assert_eq!(
        blocks(&t),
        [
            IvrsBlock::Superseded(unit(FIRST_AT, 0x10, 0x0002, 0xfeb8_0000)),
            IvrsBlock::Memory(Ivmd { requesters: Requesters::All, flags: 0x09, segment: 3, start: 0x9_d000, length: 0x1000 }),
            IvrsBlock::Unit(unit(SECOND_AT, 0x40, 0x0002, 0xfeb8_0000)),
            IvrsBlock::Unit(unit(third_at, 0x10, 0x000a, 0xfec0_0000)),
            IvrsBlock::Memory(Ivmd {
                requesters: Requesters::One(0x00a0),
                flags: 0x06,
                segment: 3,
                start: 0x7f00_0000,
                length: 0x10_0000
            }),
            IvrsBlock::Memory(Ivmd {
                requesters: Requesters::Range { first: 0x0100, last: 0x01ff },
                flags: 0x01,
                segment: 3,
                start: 0xfee0_0000,
                length: 0x10_0000
            }),
            IvrsBlock::Other { kind: 0x30, at: third_at + 28 + 32 * 2, len: 8 },
        ]
    );
}

/// The string UID's bytes, where [`Uid::String`] says they are.
const UID_AT: usize = SECOND_AT + 40 + 4 * 9 + 8 * 8 + 23 + 22;

#[test]
fn every_device_entry_decodes_with_its_range_paired_and_its_padding_dropped() {
    let walked = entries(&crafted());
    assert_eq!(
        walked[0],
        [
            DeviceEntry::All { data: 0xd7 },
            DeviceEntry::Select { id: 0x0018, data: 0x01 },
            DeviceEntry::Range { first: 0x0100, last: 0x01ff, data: 0x40 },
            DeviceEntry::Alias { id: 0x0200, used: 0x0301, data: 0x02 },
            // An entry between a start and its end is its own.
            DeviceEntry::Select { id: 0x00a0, data: 0x00 },
            DeviceEntry::AliasRange { first: 0x0300, last: 0x03ff, used: 0x0302, data: 0x04 },
            DeviceEntry::Extended { id: 0x0400, data: 0x08, extended: 0x8000_0000 },
            DeviceEntry::ExtendedRange { first: 0x0500, last: 0x05ff, data: 0x10, extended: 0x4000_0001 },
            DeviceEntry::Special { handle: 0x21, used: 0x00a0, variety: 1, data: 0xd7 },
            DeviceEntry::Special { handle: 0x00, used: 0x00a0, variety: 2, data: 0x00 },
            DeviceEntry::Other(0x05),
            DeviceEntry::Other(0x49),
            DeviceEntry::Hid { id: 0x00a5, data: 0x40, hid: *b"AMDI0010", cid: [0; 8], uid: Uid::Integer(3) },
            DeviceEntry::Hid {
                id: 0x00a6,
                data: 0x00,
                hid: *b"AMDI0020",
                cid: *b"PNP0C50\0",
                uid: Uid::String { at: UID_AT, len: 9 }
            },
            DeviceEntry::Hid { id: 0x00a7, data: 0x00, hid: *b"AMDI0030", cid: [0; 8], uid: Uid::Absent },
        ]
        .map(Ok::<_, IvrsRefused>)
    );
    assert_eq!(walked[1], [Ok(DeviceEntry::Select { id: 0x0008, data: 0 })]);
    assert_eq!(judged(&crafted(), |t| t.bytes(UID_AT, 9).collect::<Vec<_>>()), Ok(b"\\_SB.I2CA".to_vec()));
}

/// The block at [`SECOND_AT`]'s device entries when it holds `list`, in an
/// IVHD of `kind`.
fn walk(kind: u8, list: &[Vec<u8>]) -> Vec<Result<DeviceEntry, IvrsRefused>> {
    entries(&table(&[ivhd(kind, 0x0002, 0xfeb8_0000, list)])).remove(0)
}

const HEAD_40: usize = FIRST_AT + 40;

#[test]
fn a_walk_that_meets_a_bad_entry_ends_on_its_refusal() {
    let select = Ok(DeviceEntry::Select { id: 8, data: 0 });
    let refused = |list: &[Vec<u8>], why| {
        let mut full = vec![e4(0x02, 8, 0)];
        full.extend_from_slice(list);
        full.push(e4(0x02, 0x10, 0));
        assert_eq!(walk(0x40, &full), [select, Err(why)], "{list:02x?}");
    };
    let at = HEAD_40 + 4;
    refused(&[e4(0x04, 0x10, 0)], IvrsRefused::Unopened { at });
    refused(&[e4(0x03, 0x10, 0), e8(0x43, 0x20, 0, 0)], IvrsRefused::Unclosed { at });
    refused(&[e4(0x03, 0x10, 0), e4(0x04, 0x0f, 0)], IvrsRefused::Range { at, first: 0x10, last: 0x0f });
    refused(&[e8(0x80, 0, 0, 0)], IvrsRefused::EntryType { at, kind: 0x80 });
    refused(&[e8(0xc0, 0, 0, 0)], IvrsRefused::EntryType { at, kind: 0xc0 });
    for (format, uid) in [(0u8, &[1u8][..]), (1, &[]), (1, &[0; 9]), (3, &[1])] {
        refused(&[hid(0, 0, b"AMDI0010", &[0; 8], format, uid)], IvrsRefused::Uid { at, format, len: uid.len() as u8 });
    }
    // F0h is IVHD 40h's alone.
    let in_11 = walk(0x11, &[e4(0x02, 8, 0), hid(0, 0, b"AMDI0010", &[0; 8], 0, &[]), e4(0x02, 0x10, 0)]);
    assert_eq!(in_11, [select, Err(IvrsRefused::EntryType { at: HEAD_40 + 4, kind: 0xf0 })]);
    // A start of range the block ends inside.
    assert_eq!(walk(0x40, &[e4(0x02, 8, 0), e4(0x03, 0x10, 0)]), [select, Err(IvrsRefused::Unclosed { at })]);
}

/// The block ends where its length says, whatever an entry's type says it
/// needs past that.
#[test]
fn an_entry_the_block_cannot_hold_is_refused_not_read_past() {
    let short = |list: &[Vec<u8>], cut: usize| {
        let mut bytes = table(&[ivhd(0x40, 0x0002, 0xfeb8_0000, list)]);
        // Shorten the block by `cut` and the table with it, so nothing past the block exists.
        let len = u16::from_le_bytes([bytes[FIRST_AT + 2], bytes[FIRST_AT + 3]]) - cut as u16;
        bytes[FIRST_AT + 2..FIRST_AT + 4].copy_from_slice(&len.to_le_bytes());
        bytes.truncate(bytes.len() - cut);
        let len = bytes.len() as u32;
        declare_len(&mut bytes, len);
        entries(&bytes).remove(0)
    };
    let at = HEAD_40;
    assert_eq!(short(&[e8(0x42, 0, 0, 0)], 2), [Err(IvrsRefused::Entry { at })]);
    assert_eq!(short(&[e4(0x02, 0, 0)], 1), [Err(IvrsRefused::Entry { at })]);
    // The fixed part, and then the UID its length byte asks for.
    assert_eq!(short(&[hid(0, 0, b"AMDI0010", &[0; 8], 0, &[])], 1), [Err(IvrsRefused::Entry { at })]);
    assert_eq!(short(&[hid(0, 0, b"AMDI0010", &[0; 8], 2, b"ab")], 1), [Err(IvrsRefused::Entry { at })]);
}

/// A unit's list refused leaves the table, and every other unit's header
/// and list, readable: the header is what switching a unit off needs.
#[test]
fn a_refused_list_refuses_its_walk_and_not_the_table() {
    let t = table(&[ivhd(0x40, 0x0002, 0xfeb8_0000, &[e4(0x04, 0, 0)]), ivhd(0x10, 0x000a, 0xfec0_0000, &[e4(0x02, 8, 0)])]);
    let walked = entries(&t);
    assert_eq!(walked, [vec![Err(IvrsRefused::Unopened { at: HEAD_40 })], vec![Ok(DeviceEntry::Select { id: 8, data: 0 })]]);
}

#[test]
fn a_block_shorter_than_its_header_or_longer_than_the_table_is_refused() {
    let with_len = |block: Vec<u8>, len: u16| {
        let mut block = block;
        block[2..4].copy_from_slice(&len.to_le_bytes());
        table(&[block, ivmd(0x20, 0, 0, 0, 0, 0)])
    };
    let unit = ivhd(0x10, 2, 0xfeb8_0000, &[]);
    let ext = ivhd(0x11, 2, 0xfeb8_0000, &[]);
    let at = FIRST_AT;
    for (block, len) in [(unit.clone(), 23u16), (ext.clone(), 39), (ivmd(0x22, 0, 0, 0, 0, 0), 31), (vec![0x30; 8], 5), (unit, 0)] {
        assert_eq!(refusal(&with_len(block, len)), Some(IvrsRefused::Block { at, declared: len.into() }), "{len}");
    }
    // Past the table, by one byte and by the whole of the length field.
    let total = with_len(ext.clone(), 40).len() - FIRST_AT;
    for len in [total as u16 + 1, u16::MAX] {
        assert_eq!(refusal(&with_len(ext.clone(), len)), Some(IvrsRefused::Block { at, declared: len.into() }));
    }
    // Bytes after the last block too few to be a block's header.
    for tail in 1..6 {
        let mut body = table(&[ivhd(0x10, 2, 0xfeb8_0000, &[])])[36..].to_vec();
        body.extend(vec![0x30; tail]);
        let bytes = sdt(b"IVRS", 2, &body);
        assert_eq!(refusal(&bytes), Some(IvrsRefused::Block { at: FIRST_AT + 24, declared: 0 }), "{tail}");
    }
}

#[test]
fn a_table_too_short_for_its_first_block_is_refused_by_length() {
    let mut bytes = table(&[]);
    declare_len(&mut bytes, 47);
    assert_eq!(refusal(&bytes), Some(IvrsRefused::Table(TableError::Length { declared: 47, needed: 48 })));
    // Exactly the header is a table of no unit.
    assert_eq!(judged(&table(&[]), |t| t.blocks().count()), Ok(0));
}

#[test]
fn an_ivmd_range_running_backwards_or_past_the_address_space_is_refused() {
    let at = FIRST_AT;
    assert_eq!(
        refusal(&table(&[ivmd(0x22, 0, 0x0200, 0x01ff, 0, 0x1000)])),
        Some(IvrsRefused::Range { at, first: 0x0200, last: 0x01ff })
    );
    let start = u64::MAX - 0xfff;
    assert_eq!(
        refusal(&table(&[ivmd(0x20, 0, 0, 0, start, 0x1000)])),
        Some(IvrsRefused::Memory { at, start, length: 0x1000 })
    );
    assert_eq!(judged(&table(&[ivmd(0x20, 0, 0, 0, start, 0xfff)]), |t| t.blocks().count()), Ok(1));
}

#[test]
fn two_ivhds_of_one_type_naming_one_unit_are_refused_and_two_units_are_not() {
    let a = ivhd(0x11, 0x0002, 0xfeb8_0000, &[]);
    assert_eq!(refusal(&table(&[a.clone(), a.clone()])), Some(IvrsRefused::Duplicate { at: FIRST_AT + 40, other: FIRST_AT }));
    let b = ivhd(0x11, 0x000a, 0xfeb8_0000, &[]);
    assert_eq!(blocks(&table(&[a, b])).iter().filter(|b| matches!(b, IvrsBlock::Unit(_))).count(), 2);
}

#[test]
fn more_ivhds_than_the_bound_are_refused() {
    let units = |n: u16| table(&(0..n).map(|i| ivhd(0x10, i, 0xfeb8_0000, &[])).collect::<Vec<_>>());
    assert_eq!(judged(&units(64), |t| t.blocks().count()), Ok(64));
    assert_eq!(refusal(&units(65)), Some(IvrsRefused::Definitions { most: 64 }));
}

/// Firmware's bytes, any of them wrong: every single-byte change to QEMU's
/// table and to the crafted one, resealed, and every length either could
/// declare, is decoded or refused without a panic, and every walk ends
/// within its bound.
#[test]
fn no_corruption_of_an_ivrs_panics_or_runs_away() {
    let mut walked = 0usize;
    let mut refused = 0usize;
    let mut walks_refused = 0usize;
    let mut ask = |bytes: &[u8]| {
        let _ = judged(bytes, |t| {
            let _ = t.info();
            assert!(t.blocks().count() <= bytes.len() / 6);
            for block in t.blocks() {
                if let IvrsBlock::Unit(unit) | IvrsBlock::Superseded(unit) = block {
                    let mut seen = 0;
                    for entry in t.devices(&unit) {
                        seen += 1;
                        assert!(seen <= bytes.len() / 4 + 1, "a device walk is not advancing");
                        match entry {
                            Ok(DeviceEntry::Hid { uid: Uid::String { at, len }, .. }) => {
                                assert_eq!(t.bytes(at, len).count(), usize::from(len));
                            }
                            Ok(_) => {}
                            Err(_) => walks_refused += 1,
                        }
                    }
                    walked += seen;
                }
            }
        })
        .map_err(|_| refused += 1);
    };
    for original in [QEMU.to_vec(), crafted()] {
        for at in 36..original.len() {
            for value in [0x00, 0x01, 0x04, 0x7f, 0x80, 0xf0, 0xff, original[at].wrapping_add(1), original[at] ^ 0x10] {
                let mut bytes = original.clone();
                bytes[at] = value;
                reseal(&mut bytes);
                ask(&bytes);
            }
        }
        for len in 36..original.len() as u32 {
            let mut bytes = original.clone();
            declare_len(&mut bytes, len);
            ask(&bytes);
        }
    }
    assert!(
        walked > 10_000 && refused > 0 && walks_refused > 0,
        "walked {walked} entries, refused {refused} tables and {walks_refused} walks: an arm of the sweep reached nothing"
    );
}
