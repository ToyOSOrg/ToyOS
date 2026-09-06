//! The refusals, and the arithmetic a wrong answer would put a port write in.

use toyos_tco::{bound_of, chipset, timer_for, Chipset, NoPort, CHIPSETS};

#[test]
fn the_q35_row_reaches_the_port_qemu_puts_the_block_at() {
    let ich9 = chipset(0x8086, 0x2918).expect("the q35 row");
    assert_eq!(ich9.port(0x601, 0x601), Ok(0x660));
    assert_eq!(ich9.port(0x641, 0x641), Ok(0x660), "the mask is 0xff80, never 0xffc0");
}

#[test]
fn a_machine_no_row_names_is_refused_rather_than_guessed() {
    assert_eq!(chipset(0x8086, 0x2930), None, "q35's own SMBus function is not the LPC bridge");
    assert_eq!(chipset(0x1234, 0x2918), None, "the device id alone is not the key");
}

#[test]
fn the_tiger_lake_row_takes_its_base_from_the_smbus_function() {
    let tgl = chipset(0x8086, 0xa0a3).expect("the Tiger Lake-LP row");
    assert_eq!(tgl.port(0x0401, 0x0100), Ok(0x0400), "bit 0 is not part of the address");
    assert_eq!(tgl.port(0x0403, 0x0100), Ok(0x0402), "the mask is !1, never !3");
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

/// Neither shipped row can reach that edge, so this one carries the whole
/// address and puts the block at its start.
#[test]
fn the_last_register_has_to_end_inside_the_port_space() {
    let ich9 = *chipset(0x8086, 0x2918).expect("the q35 row");
    let edge = Chipset { base_mask: 0xffff, base_offset: 0, ..ich9 };

    assert_eq!(edge.port(0xffec, 0xffed), Ok(0xffec), "the far byte lands exactly on 0xffff");
    assert_eq!(
        edge.port(0xffed, 0xffed),
        Err(NoPort::Base(0xffed)),
        "one past it: without the `+ 1` this would be admitted"
    );

    let wide = Chipset { base_offset: 0xffe0, ..ich9 };
    assert_eq!(wide.port(0x601, 0x601), Err(NoPort::Base(0x601)));
}

#[test]
fn the_declared_control_words_run_and_halt_the_timer() {
    assert_eq!(toyos_tco::TCO1_CNT_RUN & toyos_tco::TCO_TMR_HLT, 0);
    assert_eq!(toyos_tco::TCO1_CNT_HALT & toyos_tco::TCO_TMR_HLT, toyos_tco::TCO_TMR_HLT);
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
fn the_register_offsets_are_ich9_tco_hs() {
    assert_eq!(
        (toyos_tco::TCO_RLD, toyos_tco::TCO2_STS, toyos_tco::TCO1_CNT, toyos_tco::TCO_TMR),
        (0x00, 0x06, 0x08, 0x12),
    );
    assert_eq!(
        (toyos_tco::TCO_TMR_HLT, toyos_tco::TCO_SECOND_TO_STS, toyos_tco::TCO_BOOT_STS),
        (1 << 11, 1 << 1, 1 << 2),
    );
}

