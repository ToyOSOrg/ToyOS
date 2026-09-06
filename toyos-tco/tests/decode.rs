//! The refusals, and the arithmetic a wrong answer would put a port write in.

use toyos_tco::{bound_of, chipset, timer_for, Chipset, NoPort, CHIPSETS};

/// QEMU's own numbers: `info pci` reports the ISA bridge at 00:1f.0; PMBASE is
/// `include/hw/southbridge/ich9.h:134-136` and the block sits 0x60 in (`:205`).
#[test]
fn the_q35_row_reaches_the_port_qemu_puts_the_block_at() {
    let ich9 = chipset(0x8086, 0x2918).expect("the q35 row");
    assert_eq!(ich9.port(0x601, 0x601), Ok(0x660));
}

#[test]
fn a_machine_no_row_names_is_refused_rather_than_guessed() {
    assert_eq!(chipset(0x8086, 0x2930), None, "q35's own SMBus function is not the LPC bridge");
    assert_eq!(chipset(0x1234, 0x2918), None, "the device id alone is not the key");
}

/// The T14's own base register has never been read, so this row is judged on
/// that machine and by nothing here; what is checked is the decode around it.
#[test]
fn the_tiger_lake_row_takes_its_base_from_the_smbus_function() {
    let tgl = chipset(0x8086, 0xa0a3).expect("the Tiger Lake-LP row");
    assert_eq!(tgl.port(0x0401, 0x0100), Ok(0x0400), "bit 0 is not part of the address");
    assert_eq!(tgl.port(0x0400, 0x0000), Err(NoPort::Disabled), "the enable is clear");
    assert_eq!(tgl.port(0x0400, 0x00ff), Err(NoPort::Disabled), "no bit below 8 enables it");
    assert_eq!(
        tgl.port(0x0001_0400, 0x0100),
        Err(NoPort::Base(0x0001_0400)),
        "a base past the port space is not one `outw` reaches"
    );
}

#[test]
fn a_base_the_block_cannot_live_at_is_refused_by_name() {
    let ich9 = chipset(0x8086, 0x2918).expect("the q35 row");
    assert_eq!(ich9.port(0x601, 0x600), Err(NoPort::Disabled), "the enable bit is clear");
    assert_eq!(ich9.port(0x001, 0x001), Err(NoPort::Base(1)), "the address bits are all zero");
}

#[test]
fn an_offset_that_walks_off_the_port_space_is_refused() {
    let wide = Chipset { base_offset: 0xffe0, ..*chipset(0x8086, 0x2918).expect("the q35 row") };
    assert_eq!(wide.port(0x601, 0x601), Err(NoPort::Base(0x601)));
}

#[test]
fn a_bound_becomes_the_timer_whose_expiries_do_not_outrun_it() {
    assert_eq!(timer_for(300_000), Some(250));
    assert_eq!(bound_of(250), 300_000);

    assert_eq!(timer_for(2_400), Some(2), "the shortest the chipset does not ignore");
    assert_eq!(timer_for(2_399), None, "one millisecond under it buys no timer at all");
    assert_eq!(timer_for(1_227_600), Some(1023), "the widest ten-bit value");
    assert_eq!(timer_for(1_228_800), None, "one tick over ten bits");

    assert_eq!(timer_for(3_000), Some(2));
    assert!(bound_of(2) <= 3_000);
}

#[test]
fn no_two_rows_name_one_device() {
    for (i, a) in CHIPSETS.iter().enumerate() {
        for b in &CHIPSETS[i + 1..] {
            assert_ne!((a.vendor, a.device), (b.vendor, b.device), "{a:?} and {b:?}");
        }
    }
}

#[test]
fn an_absent_device_reading_all_ones_names_no_port() {
    for row in CHIPSETS {
        assert_eq!(row.port(u32::MAX, u32::MAX), Err(NoPort::Absent), "{row:?}");
        assert_eq!(row.port(u32::MAX, 0x601), Err(NoPort::Absent), "{row:?}");
        assert_eq!(row.port(0x601, u32::MAX), Err(NoPort::Absent), "{row:?}");
    }
}

#[test]
fn every_row_keeps_the_whole_block_inside_the_port_space() {
    for row in CHIPSETS {
        let mut reached = 0;
        for base in 0..=u32::from(u16::MAX) {
            let Ok(port) = row.port(base, u32::from(u16::MAX)) else { continue };
            reached += 1;
            assert!(
                port.checked_add(toyos_tco::TCO_TMR).is_some(),
                "{row:?} at {base:#x} put TCO_TMR past the port space",
            );
        }
        assert!(reached > 0, "{row:?} accepted no base at all, so this walk asserted nothing");
    }
}

#[test]
fn the_register_offsets_are_ich9_tco_hs() {
    assert_eq!(
        (toyos_tco::TCO_RLD, toyos_tco::TCO1_STS, toyos_tco::TCO1_CNT, toyos_tco::TCO_TMR),
        (0x00, 0x04, 0x08, 0x12),
    );
    assert_eq!((toyos_tco::TCO_TMR_HLT, toyos_tco::TCO_TIMEOUT), (1 << 11, 1 << 3));
}

