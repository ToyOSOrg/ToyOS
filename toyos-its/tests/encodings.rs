//! Every encoding against IHI 0069 H.b's own field positions: each expected
//! value is built from `(msb, lsb, value)` triples as the specification's
//! figures and field headings give them, and compared whole, so a field in
//! the wrong place and a stray bit are both a difference.

use toyos_its::command::Command;
use toyos_its::lpi::{self, Layout, Space};
use toyos_its::{gits_baser, probe, table, CommandQueue, Its, Lacks, PageSize, Table};
use toyos_phys::Phys;

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
const TYPER: &[(u32, u32, u64)] = &[(0, 0, 1), (7, 4, 7), (12, 8, 15), (17, 13, 15), (31, 24, 3)];

/// `TYPER` with `change`'s fields beside it.
fn typer(change: &[(u32, u32, u64)]) -> u64 {
    word(&[TYPER, change].concat())
}

fn its(pta: u64) -> Its {
    probe(typer(&[(19, 19, pta)])).expect("an ITS of physical LPIs")
}

/// `GICD_TYPER` of a distributor that takes LPIs under 16-bit INTIDs: LPIS
/// [17], IDbits [23:19] the bits minus one, num_LPIs [15:11] zero.
const GICD_TYPER: &[(u32, u32, u64)] = &[(17, 17, 1), (23, 19, 15)];

/// The LPIs of a redistributor that took a layout of `bits` bits under `GICD_TYPER`.
fn space(bits: u8) -> Space {
    let layout = Layout::new(word(GICD_TYPER) as u32, bits).expect("bits the distributor has");
    layout.space(layout.propbaser(Phys::new(0x4000_0000).unwrap())).expect("the register as it was written")
}

// --- §12.18, §12.19: the registers ------------------------------------------

#[test]
fn the_type_register_says_how_wide_an_its_is() {
    let its = its(0);
    assert_eq!(its.device_bits, 16);
    // ID_bits: 16 bits of EventID and no more; ITT_entry_size: 8 bytes an entry.
    assert!(its.events(16).is_some() && its.events(17).is_none());
    assert_eq!(its.events(1).map(|events| its.itt_bytes(events)), Some(16));
    // Each field at its largest, alone: every one is its value plus one.
    let wide = |field: (u32, u32, u64)| probe(word(&[(0, 0, 1), field])).unwrap();
    assert_eq!(wide((17, 13, 31)).device_bits, 32);
    assert!(wide((12, 8, 31)).events(32).is_some());
    let entry = wide((7, 4, 15));
    assert_eq!(entry.events(1).map(|events| entry.itt_bytes(events)), Some(32));
    let least = probe(1).unwrap();
    assert_eq!(least.device_bits, 1);
    assert!(least.events(1).is_some() && least.events(2).is_none());
    assert_eq!(least.events(1).map(|events| least.itt_bytes(events)), Some(2));
}

#[test]
fn an_its_without_physical_lpis_is_refused() {
    assert_eq!(probe(typer(&[]) & !1), Err(Lacks::PhysicalLpis));
    assert_eq!(probe(!1), Err(Lacks::PhysicalLpis));
    assert!(probe(u64::MAX).is_ok());
}

#[test]
fn a_table_register_says_what_it_backs_and_how_it_is_read() {
    // Type [58:56], Entry_Size [52:48] the bytes minus one, Page_Size [9:8].
    let read = |kind: u64, entry: u64, page: u64| {
        let backing = table(word(&[(58, 56, kind), (52, 48, entry), (9, 8, page)]));
        (backing.table, backing.page, backing.entries(1))
    };
    assert_eq!(read(0b000, 0, 0b00), (Table::Unimplemented, PageSize::K4, 4096));
    assert_eq!(read(0b001, 7, 0b10), (Table::Devices, PageSize::K64, 65536 / 8));
    assert_eq!(read(0b100, 15, 0b01), (Table::Collections, PageSize::K16, 16384 / 16));
    assert_eq!(read(0b010, 31, 0b11), (Table::Other(0b010), PageSize::K64, 65536 / 32));
    assert_eq!(read(0b111, 0, 0b00).0, Table::Other(0b111));
    assert_eq!((PageSize::K4.bytes(), PageSize::K16.bytes(), PageSize::K64.bytes()), (4096, 16384, 65536));
}

#[test]
fn a_table_is_sized_in_its_own_pages_for_the_entries_it_must_hold() {
    // 8 bytes an entry in 64 KiB pages: 8192 to the page.
    let devices = table(word(&[(58, 56, 0b001), (52, 48, 7), (9, 8, 0b10)]));
    assert_eq!(devices.pages(0), None);
    assert_eq!((devices.pages(1), devices.pages(8192), devices.pages(8193)), (Some(1), Some(1), Some(2)));
    // A flat table for every DeviceID of 16 bits is eight pages, and of 21 bits the field's 256.
    assert_eq!(devices.pages(1 << 16), Some(8));
    assert_eq!(devices.pages(1 << 21), Some(256));
    assert_eq!(devices.pages((1 << 21) + 1), None);
    assert_eq!(devices.pages(u64::MAX), None);
    assert_eq!((devices.entries(8), devices.entries(256)), (1 << 16, 1 << 21));
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
    let paged = |page: u64| table(word(&[(9, 8, page)]));
    assert_eq!(paged(0b00).baser(at, 1), written(0b00, 1));
    assert_eq!(paged(0b01).baser(at, 8), written(0b01, 8));
    assert_eq!(paged(0b10).baser(at, 256), written(0b10, 256));
    assert_eq!(paged(0b10).baser(at, 0), None);
    assert_eq!(paged(0b10).baser(at, 257), None);
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
    let (mut writer, mut reader, mut held) = (0u64, 0u64, 0u32);
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
    assert_eq!(queue.after(u64::MAX), 0);
    assert_eq!(queue.after(4096 + 64), 96);
}

#[test]
fn an_offset_register_names_a_command_by_its_offset_field_alone() {
    let queue = CommandQueue::new(2).unwrap();
    // Offset [19:5]; Stalled or Retry [0]; the rest reserved.
    let beside = |offset: u64| word(&[(19, 5, offset), (0, 0, 1), (4, 1, 0xf), (63, 20, 0xfff_ffff_ffff)]);
    assert_eq!(queue.offset(beside(0x40)), 0x40 << 5);
    // An offset past the queue is a device's word: it still names a command in the queue.
    assert!(queue.offset(word(&[(19, 5, 0x7fff)])) < 8192);
    // `GITS_CREADR` with `Stalled` set and every reserved bit beside its offset is the same offset.
    let clean = |offset: u64| word(&[(19, 5, offset)]);
    assert!(queue.is_empty(clean(0x40), beside(0x40)));
    assert!(!queue.is_empty(clean(0x41), beside(0x40)));
    assert!(queue.is_full(clean(0x3f), beside(0x40)));
    assert!(!queue.is_full(clean(0x40), beside(0x40)));
    assert_eq!(queue.after(beside(0x40)), clean(0x41));
    assert_eq!(toyos_its::CREADR_STALLED, word(&[(0, 0, 1)]));
}

// --- §5.3: commands ---------------------------------------------------------

#[test]
fn each_command_holds_its_number_and_parameters_where_its_figure_has_them() {
    let its = its(0);
    let events = its.events(5).expect("32 events of an ITS of 16-bit EventIDs");
    let event = events.event(0x1f).expect("the last of 32 events");
    let itt = Phys::<8>::new(0xffff_ffff_ff00).expect("256-byte aligned");
    let target = its.target(0x1234, Phys::new(0x080a_0000).unwrap());
    let collection = its.collections(0xc012).collection(0xc011).expect("a collection the table holds");
    let lpi = space(16).lpi(0xabcd).expect("an LPI");

    // MAPD (Figure 5-13): DeviceID [63:32]; Size [4:0] of DW1, the bits minus one; V [63] and ITT_addr [51:8] of DW2.
    assert_eq!(
        Command::MapDevice { device: 0xdead_beef, table: itt, events }.words(),
        command(&[(0, 7, 0, 0x08), (0, 63, 32, 0xdead_beef), (1, 4, 0, 4), (2, 63, 63, 1), (2, 51, 8, 0xffff_ffff_ff00 >> 8)])
    );
    assert_eq!(
        Command::UnmapDevice { device: 0xdead_beef }.words(),
        command(&[(0, 7, 0, 0x08), (0, 63, 32, 0xdead_beef), (2, 63, 63, 0)])
    );
    // MAPC (Figure 5-12): V [63], RDbase [51:16] and ICID [15:0] of DW2.
    assert_eq!(
        Command::MapCollection { collection, target }.words(),
        command(&[(0, 7, 0, 0x09), (2, 63, 63, 1), (2, 51, 16, 0x1234), (2, 15, 0, 0xc011)])
    );
    // MAPTI (Figure 5-15): DeviceID [63:32]; pINTID [63:32] and EventID [31:0] of DW1; ICID [15:0] of DW2.
    assert_eq!(
        Command::MapEvent { device: 0xdead_beef, event, lpi, collection }.words(),
        command(&[(0, 7, 0, 0x0A), (0, 63, 32, 0xdead_beef), (1, 63, 32, 0xabcd), (1, 31, 0, 0x1f), (2, 15, 0, 0xc011)])
    );
    // SYNC (Figure 5-18): RDbase [51:16] of DW2.
    assert_eq!(Command::Sync(target).words(), command(&[(0, 7, 0, 0x05), (2, 51, 16, 0x1234)]));
}

/// `DISCARD` (Figure 5-7) and `INV` (Figure 5-9), one layout under two
/// numbers: doubleword 0 holds DeviceID [63:32], RES0 [31:8] and the command
/// number [7:0]; doubleword 1 holds RES0 [63:32] and EventID [31:0];
/// doublewords 2 and 3 are RES0.
#[test]
fn inv_and_discard_hold_a_device_and_an_event_and_nothing_else() {
    let event = its(0).events(16).unwrap().event(0xa5c3).unwrap();
    let figure = |number: u64| command(&[(0, 63, 32, 0xdead_beef), (0, 31, 8, 0), (0, 7, 0, number), (1, 63, 32, 0), (1, 31, 0, 0xa5c3)]);
    assert_eq!(Command::Discard { device: 0xdead_beef, event }.words(), figure(0x0F));
    assert_eq!(Command::Reconfigure { device: 0xdead_beef, event }.words(), figure(0x0C));
    // The widest event there is fills its field and reaches no other.
    let widest = probe(word(&[(0, 0, 1), (12, 8, 31)])).unwrap().events(32).unwrap().event(u32::MAX).unwrap();
    assert_eq!(Command::Discard { device: 0, event: widest }.words(), [0x0F, 0xffff_ffff, 0, 0]);
    assert_eq!(Command::Reconfigure { device: u32::MAX, event: widest }.words(), [0xffff_ffff_0000_000c, 0xffff_ffff, 0, 0]);
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

#[test]
fn an_event_is_one_its_devices_table_holds() {
    let its = its(0);
    let events = its.events(5).unwrap();
    assert!(events.event(0).is_some() && events.event(31).is_some());
    assert_eq!(events.event(32), None);
    assert_eq!(events.event(u32::MAX), None);
    // The widest table there is holds every EventID.
    let wide = probe(word(&[(0, 0, 1), (12, 8, 31)])).unwrap();
    assert!(wide.events(32).unwrap().event(u32::MAX).is_some());
}

/// §5.2.2 and §5.3.1: the collections are the table's, with those the ITS
/// holds itself (`HCC`) where it has no table or `CCT` [2] counts both, and
/// an ICID is 16 bits or, under `CIL` [36], `CIDbits` [35:32] plus one.
#[test]
fn a_collection_is_one_the_its_holds_or_its_table_does() {
    let last = |its: Its, in_table: u64, id: u16| its.collections(in_table).collection(id).is_some();
    // No table: the three the ITS holds.
    let held = probe(typer(&[])).unwrap();
    assert!(last(held, 0, 2) && !last(held, 0, 3));
    // A table without CCT: the table's alone.
    assert!(last(held, 8, 7) && !last(held, 8, 8));
    // With CCT: both.
    let cumulative = probe(typer(&[(2, 2, 1)])).unwrap();
    assert!(last(cumulative, 8, 10) && !last(cumulative, 8, 11));
    assert!(last(cumulative, 0, 2) && !last(cumulative, 0, 3));
    // No more than an ICID numbers: all 2^16 of 16 bits, and 2^4 under CIL with CIDbits 3.
    assert!(last(held, 1 << 20, u16::MAX));
    let narrow = probe(typer(&[(36, 36, 1), (35, 32, 3)])).unwrap();
    assert!(last(narrow, 1 << 20, 15) && !last(narrow, 1 << 20, 16));
    // CIDbits says nothing where CIL is clear.
    let unlimited = probe(typer(&[(35, 32, 3)])).unwrap();
    assert!(last(unlimited, 1 << 20, 16));
    // An ITS that holds none and was given no table has none.
    let none = probe(1).unwrap();
    assert!(!last(none, 0, 0));
    // Every bit set and a table of every entry: still no more than a 16-bit ICID numbers.
    assert!(last(probe(u64::MAX).unwrap(), u64::MAX, u16::MAX));
}

// --- §5.1: LPIs --------------------------------------------------------------

/// `GICD_TYPER` (§12.9.38): LPIS [17], IDbits [23:19] the INTID bits minus
/// one, num_LPIs [15:11].
#[test]
fn an_lpi_layout_is_inside_the_distributors_intids() {
    let typer = |fields: &[(u32, u32, u64)]| word(fields) as u32;
    // No LPIs at all, whatever the bits.
    assert_eq!(Layout::new(typer(&[(23, 19, 15)]), 14), None);
    // 16 bits of INTID: 14, the fewest that reach an LPI, to 16.
    let sixteen = typer(GICD_TYPER);
    assert_eq!(Layout::new(sixteen, 13), None);
    assert!(Layout::new(sixteen, 14).is_some() && Layout::new(sixteen, 16).is_some());
    assert_eq!(Layout::new(sixteen, 17), None);
    // The field at its largest is 32 bits, and every other field says nothing of it.
    let widest = typer(&[(17, 17, 1), (23, 19, 31), (31, 24, 0xff), (16, 16, 1), (10, 0, 0x7ff)]);
    assert!(Layout::new(widest, 32).is_some());
    assert_eq!(Layout::new(widest, 33), None);
    assert_eq!(Layout::new(u32::MAX, u8::MAX), None);
}

#[test]
fn an_lpi_is_an_intid_from_8192_that_the_tables_and_the_distributor_both_count() {
    let fourteen = space(14);
    assert_eq!(fourteen.lpi(8191), None);
    assert_eq!(fourteen.lpi(8192).map(|lpi| lpi.intid()), Some(8192));
    assert_eq!(fourteen.lpi(16383).map(|lpi| lpi.intid()), Some(16383));
    assert_eq!(fourteen.lpi(16384), None);
    assert_eq!(fourteen.lpi(u32::MAX), None);
    assert_eq!(fourteen.lpi(0), None);

    // num_LPIs [15:11] of 9: 2^10 LPIs, 8192 to 9215, under tables laid out for 16 bits.
    let counted = word(&[GICD_TYPER, &[(15, 11, 9)]].concat()) as u32;
    let layout = Layout::new(counted, 16).unwrap();
    let space = layout.space(layout.propbaser(Phys::new(0).unwrap())).unwrap();
    assert!(space.lpi(9215).is_some());
    assert_eq!(space.lpi(9216), None);
    // And it never counts more than the tables' bits number: 2^16 of them under 14 bits is 8192.
    let over = word(&[GICD_TYPER, &[(15, 11, 15)]].concat()) as u32;
    let layout = Layout::new(over, 14).unwrap();
    let space = layout.space(layout.propbaser(Phys::new(0).unwrap())).unwrap();
    assert!(space.lpi(16383).is_some());
    assert_eq!(space.lpi(16384), None);

    // 32 bits: every INTID from 8192 up.
    let widest = word(&[(17, 17, 1), (23, 19, 31)]) as u32;
    let layout = Layout::new(widest, 32).unwrap();
    let space = layout.space(layout.propbaser(Phys::new(0).unwrap())).unwrap();
    assert!(space.lpi(u32::MAX).is_some() && space.lpi(8191).is_none());
    assert_eq!((layout.configuration_bytes(), layout.pending_bytes()), ((1 << 32) - 8192, 1 << 29));
}

/// §12.11.33: a redistributor may hold `GICR_PROPBASER` as its own, and its
/// `IDbits` [4:0] bounds the LPIs whatever was written.
#[test]
fn a_redistributor_that_reads_other_bits_back_has_no_lpis_of_the_layout() {
    let layout = Layout::new(word(GICD_TYPER) as u32, 16).unwrap();
    let written = layout.propbaser(Phys::new(0x4000_0000).unwrap());
    assert!(layout.space(written).is_some());
    // Its cacheability and address may read back otherwise; its IDbits may not.
    assert!(layout.space(written & 0x1f).is_some());
    assert!(layout.space(written | !0x1f).is_some());
    for other in [0u64, 13, 14, 16, 31] {
        assert_eq!(layout.space(written & !0x1f | other), None, "IDbits {other}");
    }
}

#[test]
fn the_configuration_table_starts_at_intid_8192_and_the_pending_table_at_zero() {
    let layout = Layout::new(word(GICD_TYPER) as u32, 16).unwrap();
    let space = space(16);
    // "(base address + (N - 8192))", one byte an LPI.
    assert_eq!(space.lpi(8192).unwrap().configuration_index(), 0);
    assert_eq!(space.lpi(65535).unwrap().configuration_index(), 65535 - 8192);
    assert_eq!(layout.configuration_bytes(), 65536 - 8192);
    // One bit an INTID, the first 1 KiB below every LPI.
    assert_eq!(layout.pending_bytes(), 8192);
    let fourteen = Layout::new(word(GICD_TYPER) as u32, 14).unwrap();
    assert_eq!((fourteen.configuration_bytes(), fourteen.pending_bytes()), (8192, 2048));
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
    let layout = Layout::new(word(GICD_TYPER) as u32, 16).unwrap();
    // OuterCache [58:56], Physical_Address [51:12], Shareability [11:10], InnerCache [9:7], IDbits [4:0] the bits minus one.
    assert_eq!(
        layout.propbaser(Phys::new(0xffff_ffff_f000).unwrap()),
        word(&[(58, 56, 0), (51, 12, 0xffff_ffff_f000 >> 12), (11, 10, 0b01), (9, 7, 0b111), (4, 0, 15)])
    );
    assert_eq!(Layout::new(word(GICD_TYPER) as u32, 14).unwrap().propbaser(Phys::new(0).unwrap()) & 0x1f, 13);
    // PTZ [62], OuterCache [58:56], Physical_Address [51:16], Shareability [11:10], InnerCache [9:7].
    assert_eq!(
        lpi::pendbaser(Phys::new(0xffff_ffff_0000).unwrap()),
        word(&[(62, 62, 1), (58, 56, 0), (51, 16, 0xffff_ffff_0000 >> 16), (11, 10, 0b01), (9, 7, 0b111)])
    );
    assert_eq!(u64::from(lpi::CTLR_ENABLE_LPIS), word(&[(0, 0, 1)]));
    // GICR_TYPER: PLPIS [0], Processor_Number [23:8] under an affinity and flags it is not.
    assert_eq!(lpi::TYPER_PLPIS, word(&[(0, 0, 1)]));
    assert_eq!(lpi::processor_number(word(&[(63, 32, 0xffff_ffff), (23, 8, 0xbeef), (7, 0, 0xff), (31, 24, 0xff)])), 0xbeef);
}

// --- the register maps -------------------------------------------------------

/// §12.18: "The control registers, which are located at ITS_base + 0x000000.
/// The interrupt translation space, which is located at ITS_base +
/// 0x010000." Table 12-33, the ITS control register map, and Table 12-34,
/// the ITS translation register map, row by row; each register's own
/// description (§12.19.4, .13, .2, .5, .3, .1, .12) states the same offset.
#[test]
fn the_its_registers_sit_where_tables_12_33_and_12_34_have_them() {
    let control: [(usize, usize); 5] = [
        (0x0000, toyos_its::GITS_CTLR),
        (0x0008, toyos_its::GITS_TYPER),
        (0x0080, toyos_its::GITS_CBASER),
        (0x0088, toyos_its::GITS_CWRITER),
        (0x0090, toyos_its::GITS_CREADR),
    ];
    for (specified, declared) in control {
        assert_eq!(declared, specified);
    }
    // "0x0100-0x0138 GITS_BASER<n>", and §12.19.1's "0x0100 + (8 * n)", n = 0 - 7.
    assert_eq!((0..9).map(gits_baser).collect::<Vec<_>>(), [
        Some(0x0100),
        Some(0x0108),
        Some(0x0110),
        Some(0x0118),
        Some(0x0120),
        Some(0x0128),
        Some(0x0130),
        Some(0x0138),
        None
    ]);
    // "0x0040 GITS_TRANSLATER", of the frame at ITS_base + 0x010000.
    assert_eq!((toyos_its::TRANSLATION_FRAME, toyos_its::GITS_TRANSLATER), (0x01_0000, 0x0040));
    assert_eq!(u64::from(toyos_its::CTLR_ENABLED), word(&[(0, 0, 1)]));
}

/// §12.10, Table 12-27, the GIC physical LPI Redistributor register map, by
/// its offsets from `RD_base`; §12.11.33 and §12.11.32 state the same.
#[test]
fn the_redistributors_lpi_registers_sit_where_table_12_27_has_them() {
    assert_eq!((lpi::GICR_PROPBASER, lpi::GICR_PENDBASER), (0x0070, 0x0078));
}
