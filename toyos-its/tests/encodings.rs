//! Every encoding against IHI 0069 H.b's own field positions: each expected
//! value is built from `(msb, lsb, value)` triples as the specification's
//! figures and field headings give them, and compared whole, so a field in
//! the wrong place and a stray bit are both a difference.

use toyos_its::command::Command;
use toyos_its::lpi::{self, Space};
use toyos_its::{baser, gits_baser, probe, table, CommandQueue, Its, Lacks, PageSize, Phys, Table};

/// `N` doublewords holding `value` in bits `[msb:lsb]` for every triple, and
/// zero everywhere else.
fn layout<const N: usize>(fields: &[(u32, u32, u64)]) -> [u64; N] {
    let mut words = [0u64; N];
    for &(msb, lsb, value) in fields {
        let width = msb - lsb + 1;
        assert!(width == 64 || value >> width == 0, "{value:#x} does not fit [{msb}:{lsb}]");
        for bit in 0..width {
            if value >> bit & 1 != 0 {
                let at = lsb + bit;
                assert_eq!(words[(at / 64) as usize] >> (at % 64) & 1, 0, "two fields claim bit {at}");
                words[(at / 64) as usize] |= 1 << (at % 64);
            }
        }
    }
    words
}

fn word(fields: &[(u32, u32, u64)]) -> u64 {
    layout::<1>(fields)[0]
}

/// A command's fields, each given by its doubleword and its bits in it.
fn command(fields: &[(u32, u32, u32, u64)]) -> [u64; 4] {
    let absolute: Vec<_> = fields.iter().map(|&(dw, msb, lsb, value)| (dw * 64 + msb, dw * 64 + lsb, value)).collect();
    layout(&absolute)
}

/// `GITS_TYPER` of an ITS of 16-bit EventIDs and DeviceIDs and 8-byte
/// translation entries, holding 3 collections itself, which names a
/// redistributor by `pta`: Physical [0], ITT_entry_size [7:4], ID_bits
/// [12:8], Devbits [17:13], PTA [19], HCC [31:24].
fn typer(pta: u64) -> u64 {
    word(&[(0, 0, 1), (7, 4, 7), (12, 8, 15), (17, 13, 15), (19, 19, pta), (31, 24, 3)])
}

fn its(pta: u64) -> Its {
    probe(typer(pta)).expect("an ITS of physical LPIs")
}

// --- §12.18, §12.19: the registers ------------------------------------------

#[test]
fn the_type_register_says_how_wide_an_its_is() {
    let its = its(0);
    assert_eq!((its.event_bits, its.device_bits, its.itt_entry_bytes, its.collections_held), (16, 16, 8, 3));
    // Each field at its largest, alone: every one is its value plus one but HCC.
    let wide = |field: (u32, u32, u64)| probe(word(&[(0, 0, 1), field])).unwrap();
    assert_eq!(wide((12, 8, 31)).event_bits, 32);
    assert_eq!(wide((17, 13, 31)).device_bits, 32);
    assert_eq!(wide((7, 4, 15)).itt_entry_bytes, 16);
    assert_eq!(wide((31, 24, 255)).collections_held, 255);
    let least = probe(1).unwrap();
    assert_eq!((least.event_bits, least.device_bits, least.itt_entry_bytes, least.collections_held), (1, 1, 1, 0));
}

#[test]
fn an_its_without_physical_lpis_is_refused() {
    assert_eq!(probe(typer(0) & !1), Err(Lacks::PhysicalLpis));
    assert_eq!(probe(!1), Err(Lacks::PhysicalLpis));
    assert!(probe(u64::MAX).is_ok());
}

#[test]
fn the_registers_sit_where_the_its_map_has_them() {
    assert_eq!(
        (toyos_its::GITS_CTLR, toyos_its::GITS_TYPER, toyos_its::GITS_CBASER, toyos_its::GITS_CWRITER, toyos_its::GITS_CREADR),
        (0x0000, 0x0008, 0x0080, 0x0088, 0x0090)
    );
    // GITS_BASER<n> is 0x0100 to 0x0138.
    assert_eq!((gits_baser(0), gits_baser(7), gits_baser(8)), (Some(0x0100), Some(0x0138), None));
    // GITS_TRANSLATER is 0x0040 of the second 64 KiB frame.
    assert_eq!(toyos_its::TRANSLATION_FRAME + toyos_its::GITS_TRANSLATER, 0x1_0040);
    assert_eq!(u64::from(toyos_its::CTLR_ENABLED | toyos_its::CTLR_QUIESCENT), word(&[(0, 0, 1), (31, 31, 1)]));
    assert_eq!(toyos_its::CREADR_STALLED, word(&[(0, 0, 1)]));
}

#[test]
fn a_table_register_says_what_it_backs_and_how_it_is_read() {
    // Type [58:56], Entry_Size [52:48] the bytes minus one, Page_Size [9:8].
    let read = |kind: u64, entry: u64, page: u64| table(word(&[(58, 56, kind), (52, 48, entry), (9, 8, page)]));
    assert_eq!(read(0b000, 0, 0b00), (Table::Unimplemented, 1, PageSize::K4));
    assert_eq!(read(0b001, 7, 0b10), (Table::Devices, 8, PageSize::K64));
    assert_eq!(read(0b100, 15, 0b01), (Table::Collections, 16, PageSize::K16));
    assert_eq!(read(0b010, 31, 0b11), (Table::Other(0b010), 32, PageSize::K64));
    assert_eq!(read(0b111, 0, 0b00).0, Table::Other(0b111));
    assert_eq!((PageSize::K4.bytes(), PageSize::K16.bytes(), PageSize::K64.bytes()), (4096, 16384, 65536));
}

#[test]
fn a_table_is_given_flat_valid_and_counted_in_its_own_pages() {
    let at = Phys::<16>::new(0xffff_ffff_0000).expect("64 KiB aligned, below 2^48");
    let written = |page: u64, pages: u64| {
        Some(word(&[
            (63, 63, 1),                      // Valid
            (62, 62, 0),                      // Indirect
            (61, 59, 0b111),                  // InnerCache: RaWaWb
            (55, 53, 0b000),                  // OuterCache: as the inner
            (47, 12, 0xffff_ffff_0000 >> 12), // Physical_Address
            (11, 10, 0b01),                   // Shareability: inner
            (9, 8, page),                     // Page_Size
            (7, 0, pages - 1),                // Size
        ]))
    };
    assert_eq!(baser(at, PageSize::K4, 1), written(0b00, 1));
    assert_eq!(baser(at, PageSize::K16, 8), written(0b01, 8));
    assert_eq!(baser(at, PageSize::K64, 256), written(0b10, 256));
    assert_eq!(baser(at, PageSize::K64, 0), None);
    assert_eq!(baser(at, PageSize::K64, 257), None);
}

#[test]
fn an_address_off_its_alignment_or_past_48_bits_is_no_address() {
    assert_eq!(Phys::<8>::new(0x100).map(Phys::get), Some(0x100));
    assert_eq!(Phys::<8>::new(0x80), None);
    assert_eq!(Phys::<16>::new(0x8000), None);
    assert_eq!(Phys::<16>::new(1 << 48), None);
    assert_eq!(Phys::<16>::new((1 << 48) - 0x1_0000).map(Phys::get), Some((1 << 48) - 0x1_0000));
}

// --- §5.2.8: the command queue ----------------------------------------------

#[test]
fn the_command_queue_is_counted_in_4_kib_pages_and_holds_one_command_fewer() {
    let at = Phys::<16>::new(0x4003_0000).unwrap();
    // Valid [63], InnerCache [61:59], OuterCache [55:53], Physical_Address [51:12], Shareability [11:10], Size [7:0].
    let written = |pages: u64| {
        word(&[(63, 63, 1), (61, 59, 0b111), (55, 53, 0), (51, 12, 0x4003_0000 >> 12), (11, 10, 0b01), (7, 0, pages - 1)])
    };
    assert_eq!(CommandQueue::new(1).unwrap().cbaser(at), written(1));
    assert_eq!(CommandQueue::new(256).unwrap().cbaser(at), written(256));
    assert!(CommandQueue::new(0).is_none() && CommandQueue::new(257).is_none());

    // One page is 128 commands. Against a count kept beside it, over three laps.
    let queue = CommandQueue::new(1).unwrap();
    let (mut writer, mut reader, mut held) = (0u32, 0u32, 0u32);
    for step in 0..3 * 128 * 2 {
        assert_eq!(queue.is_empty(writer, reader), held == 0, "step {step}");
        assert_eq!(queue.is_full(writer, reader), held == 127, "step {step}");
        if held < 127 && (step / 150u32).is_multiple_of(2) {
            writer = queue.after(writer);
            held += 1;
        } else if held > 0 {
            reader = queue.after(reader);
            held -= 1;
        }
        assert!(writer < 4096 && reader < 4096 && writer % 32 == 0 && reader % 32 == 0);
    }
    assert_eq!(queue.after(4096 - 32), 0);
    // An offset is the ITS's word: past the queue, it is still an offset in it.
    assert_eq!(queue.after(u32::MAX), 0);
    assert_eq!(queue.after(4096 + 64), 96);
}

#[test]
fn an_offset_register_names_a_command_by_its_offset_field_alone() {
    let queue = CommandQueue::new(2).unwrap();
    // Offset [19:5]; Stalled or Retry [0]; the rest reserved.
    assert_eq!(queue.offset(word(&[(19, 5, 0x40), (0, 0, 1), (4, 1, 0xf), (63, 20, 0xfff_ffff_ffff)])), 0x40 << 5);
    // An offset past the queue is a device's word: it still names a command in the queue.
    assert!(queue.offset(word(&[(19, 5, 0x7fff)])) < 8192);
}

// --- §5.3: commands ---------------------------------------------------------

#[test]
fn each_command_holds_its_number_and_parameters_where_its_figure_has_them() {
    let its = its(0);
    let events = its.events(5).expect("32 events of an ITS of 16-bit EventIDs");
    let itt = Phys::<8>::new(0xffff_ffff_ff00).expect("256-byte aligned");
    let target = its.target(0x1234, Phys::new(0x080a_0000).unwrap());
    let lpi = Space::new(16).unwrap().lpi(0xabcd).expect("an LPI");

    // MAPD: DeviceID [63:32]; Size [4:0] of DW1, the bits minus one; V [63] and ITT_addr [51:8] of DW2.
    assert_eq!(
        Command::MapDevice { device: 0xdead_beef, table: itt, events }.words(),
        command(&[(0, 7, 0, 0x08), (0, 63, 32, 0xdead_beef), (1, 4, 0, 4), (2, 63, 63, 1), (2, 51, 8, 0xffff_ffff_ff00 >> 8)])
    );
    assert_eq!(
        Command::UnmapDevice { device: 0xdead_beef }.words(),
        command(&[(0, 7, 0, 0x08), (0, 63, 32, 0xdead_beef), (2, 63, 63, 0)])
    );
    // MAPC: V [63], RDbase [51:16] and ICID [15:0] of DW2.
    assert_eq!(
        Command::MapCollection { collection: 0xc011, target }.words(),
        command(&[(0, 7, 0, 0x09), (2, 63, 63, 1), (2, 51, 16, 0x1234), (2, 15, 0, 0xc011)])
    );
    // MAPTI: DeviceID [63:32]; pINTID [63:32] and EventID [31:0] of DW1; ICID [15:0] of DW2.
    assert_eq!(
        Command::MapEvent { device: 0xdead_beef, event: 0x1f, lpi, collection: 0xc011 }.words(),
        command(&[(0, 7, 0, 0x0A), (0, 63, 32, 0xdead_beef), (1, 63, 32, 0xabcd), (1, 31, 0, 0x1f), (2, 15, 0, 0xc011)])
    );
    // INV and DISCARD: DeviceID [63:32]; EventID [31:0] of DW1.
    assert_eq!(
        Command::Reconfigure { device: 0xdead_beef, event: 0xffff_ffff }.words(),
        command(&[(0, 7, 0, 0x0C), (0, 63, 32, 0xdead_beef), (1, 31, 0, 0xffff_ffff)])
    );
    assert_eq!(
        Command::Discard { device: 0xdead_beef, event: 0xffff_ffff }.words(),
        command(&[(0, 7, 0, 0x0F), (0, 63, 32, 0xdead_beef), (1, 31, 0, 0xffff_ffff)])
    );
    // SYNC: RDbase [51:16] of DW2.
    assert_eq!(Command::Sync(target).words(), command(&[(0, 7, 0, 0x05), (2, 51, 16, 0x1234)]));
}

#[test]
fn a_redistributor_is_named_as_the_its_asks_by_number_or_by_address() {
    let frame = Phys::<16>::new(0xffff_fffe_0000).unwrap();
    let rdbase = |pta: u64| Command::Sync(its(pta).target(0x1234, frame)).words()[2];
    // PTA clear: GICR_TYPER.Processor_Number. PTA set: bits [51:16] of the frame's address.
    assert_eq!(rdbase(0), word(&[(51, 16, 0x1234)]));
    assert_eq!(rdbase(1), word(&[(51, 16, 0xffff_fffe_0000 >> 16)]));
}

#[test]
fn a_devices_events_are_at_least_two_and_no_more_than_the_its_has_bits_for() {
    let its = its(0);
    assert_eq!(its.events(0), None);
    assert_eq!(its.events(17), None);
    // 8 bytes an entry: 2 events are 16 bytes and 2^16 are 512 KiB.
    assert_eq!(its.events(1).map(|events| its.itt_bytes(events)), Some(16));
    assert_eq!(its.events(16).map(|events| its.itt_bytes(events)), Some(8 << 16));
    let size = |bits| Command::MapDevice { device: 0, table: Phys::new(0).unwrap(), events: its.events(bits).unwrap() }.words()[1];
    assert_eq!((size(1), size(16)), (0, 15));
}

// --- §5.1: LPIs --------------------------------------------------------------

#[test]
fn an_lpi_is_an_intid_from_8192_inside_its_space() {
    let space = Space::new(14).expect("the fewest bits that reach an LPI");
    assert_eq!(space.lpi(8191), None);
    assert_eq!(space.lpi(8192).map(|lpi| lpi.intid()), Some(8192));
    assert_eq!(space.lpi(16383).map(|lpi| lpi.intid()), Some(16383));
    assert_eq!(space.lpi(16384), None);
    assert_eq!(space.lpi(u32::MAX), None);
    assert!(Space::new(13).is_none() && Space::new(24).is_some() && Space::new(25).is_none());
}

#[test]
fn the_configuration_table_starts_at_intid_8192_and_the_pending_table_at_zero() {
    let space = Space::new(16).unwrap();
    // "(base address + (N - 8192))", one byte an LPI.
    assert_eq!(space.lpi(8192).unwrap().configuration_index(), 0);
    assert_eq!(space.lpi(65535).unwrap().configuration_index(), 65535 - 8192);
    assert_eq!(space.configuration_bytes(), 65536 - 8192);
    // "(base address + (N / 8))", bit "(N MOD 8)": one bit an INTID, the first 1 KiB below every LPI.
    assert_eq!(space.lpi(8192).unwrap().pending_bit(), (1024, 0));
    assert_eq!(space.lpi(8199).unwrap().pending_bit(), (1024, 7));
    assert_eq!(space.lpi(65535).unwrap().pending_bit(), (8191, 7));
    assert_eq!(space.pending_bytes(), 8192);
    assert_eq!((Space::new(14).unwrap().configuration_bytes(), Space::new(14).unwrap().pending_bytes()), (8192, 2048));
}

#[test]
fn an_lpis_configuration_is_its_priority_a_set_bit_and_its_enable() {
    // Priority [7:2], bit [1] RES1, Enable [0].
    let byte = |priority: u64, enable: u64| word(&[(7, 2, priority), (1, 1, 1), (0, 0, enable)]) as u8;
    assert_eq!(lpi::configuration(0xa0, true), byte(0xa0 >> 2, 1));
    assert_eq!(lpi::configuration(0xa0, false), byte(0xa0 >> 2, 0));
    // The priority's low two bits are not the table's to hold.
    assert_eq!(lpi::configuration(0xff, false), byte(0x3f, 0));
    assert_eq!(lpi::configuration(0x03, true), byte(0, 1));
}

#[test]
fn the_redistributors_base_registers_name_the_two_tables() {
    let space = Space::new(16).unwrap();
    // OuterCache [58:56], Physical_Address [51:12], Shareability [11:10], InnerCache [9:7], IDbits [4:0] the bits minus one.
    assert_eq!(
        space.propbaser(Phys::new(0xffff_ffff_f000).unwrap()),
        word(&[(58, 56, 0), (51, 12, 0xffff_ffff_f000 >> 12), (11, 10, 0b01), (9, 7, 0b111), (4, 0, 15)])
    );
    assert_eq!(Space::new(14).unwrap().propbaser(Phys::new(0).unwrap()) & 0x1f, 13);
    // PTZ [62], OuterCache [58:56], Physical_Address [51:16], Shareability [11:10], InnerCache [9:7].
    assert_eq!(
        lpi::pendbaser(Phys::new(0xffff_ffff_0000).unwrap()),
        word(&[(62, 62, 1), (58, 56, 0), (51, 16, 0xffff_ffff_0000 >> 16), (11, 10, 0b01), (9, 7, 0b111)])
    );
    assert_eq!((lpi::GICR_CTLR, lpi::GICR_TYPER, lpi::GICR_PROPBASER, lpi::GICR_PENDBASER), (0x0000, 0x0008, 0x0070, 0x0078));
    assert_eq!(u64::from(lpi::CTLR_ENABLE_LPIS), word(&[(0, 0, 1)]));
    // GICR_TYPER: PLPIS [0], Processor_Number [23:8] under an affinity and flags it is not.
    assert_eq!(lpi::TYPER_PLPIS, word(&[(0, 0, 1)]));
    assert_eq!(lpi::processor_number(word(&[(63, 32, 0xffff_ffff), (23, 8, 0xbeef), (7, 0, 0xff), (31, 24, 0xff)])), 0xbeef);
}
