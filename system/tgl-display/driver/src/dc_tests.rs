//! The DMC package and the DC6 sequence, against the pinned package and
//! packages built here from Vol 12's layouts.

extern crate std;

use std::vec;
use std::vec::Vec;

use crate::dmc::{self, Malformed, Package, Stepping, TGL_PACKAGE};
use crate::model::Engine;
use crate::{allow_display_c6, regs, Refusal};

const C0: Stepping = Stepping { letter: b'C', digit: b'0' };
const D0: Stepping = Stepping { letter: b'D', digit: b'0' };

#[test]
fn the_pinned_package_is_version_2_12_with_one_main_program_inside_the_window() {
    let package = Package::parse(TGL_PACKAGE).expect("the pinned package parses");
    assert_eq!(package.version, 0x0002_000C);
    let main = package.main_program(C0).expect("a main program for C0");
    assert_eq!((main.version, main.start, main.words().count()), (0x0001_0002, 0x8_0000, 4460));
    // The first two words are what the T14's storage held before any driver
    // ran (the scout, 2026-10-31).
    assert_eq!(main.words().take(2).collect::<Vec<_>>(), [0x0c00_4040, 0x0001_0002]);
    assert_eq!(main.writes().count(), 9);
    assert!(main.writes().all(|(at, _)| dmc::WINDOW.contains(&at)));
    assert_eq!(package.main_program(D0).map(|p| p.start), Some(0x8_0000));
}

#[test]
fn a_9a49s_revision_names_its_display_stepping_from_vol_4() {
    assert_eq!(Stepping::of_9a49(0x1), Some(C0));
    assert_eq!(Stepping::of_9a49(0x3), Some(D0));
    assert_eq!(Stepping::of_9a49(0x2), None);
}

/// One program: where it is stored, its payload and its header's writes.
struct Built {
    start: u32,
    words: Vec<u32>,
    writes: Vec<(u32, u32)>,
}

fn program(start: u32, words: &[u32], writes: &[(u32, u32)]) -> Built {
    Built { start, words: words.to_vec(), writes: writes.to_vec() }
}

/// A package in Vol 12's layout: `entries` as (DMC ID, stepping,
/// substepping, the program's index or `None` for 0xFFFFFFFF).
fn package(entries: &[(u8, u8, u8, Option<usize>)], programs: &[Built]) -> Vec<u8> {
    let mut bodies: Vec<u8> = vec![];
    let mut offsets = vec![];
    for p in programs {
        offsets.push(bodies.len() as u32 / 4);
        let mut header = vec![0u8; 256];
        header[0..4].copy_from_slice(&0x4040_3E3Eu32.to_le_bytes());
        header[4] = 64;
        header[5] = 3;
        header[0x0C..0x10].copy_from_slice(&(p.words.len() as u32).to_le_bytes());
        header[0x10..0x14].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        header[0x14..0x18].copy_from_slice(&p.start.to_le_bytes());
        header[0x5C..0x60].copy_from_slice(&(p.writes.len() as u32).to_le_bytes());
        for (n, (at, data)) in p.writes.iter().enumerate().take(20) {
            header[0x60 + 4 * n..0x64 + 4 * n].copy_from_slice(&at.to_le_bytes());
            header[0xB0 + 4 * n..0xB4 + 4 * n].copy_from_slice(&data.to_le_bytes());
        }
        bodies.extend(header);
        bodies.extend(p.words.iter().flat_map(|w| w.to_le_bytes()));
    }
    let mut package_header = vec![0u8; 400];
    package_header[0] = 100;
    package_header[1] = 2;
    package_header[12..16].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    for (n, (dmc, stepping, substepping, which)) in entries.iter().enumerate() {
        let at = 16 + 12 * n;
        package_header[at + 1] = *dmc;
        package_header[at + 2] = *stepping;
        package_header[at + 3] = *substepping;
        let offset = which.map_or(0xFFFF_FFFF, |i| offsets[i]);
        package_header[at + 4..at + 8].copy_from_slice(&offset.to_le_bytes());
    }
    let mut file = vec![0u8; 128];
    let total = (128 + package_header.len() + bodies.len()) as u32 / 4;
    for (at, value) in [(0x00, 0x09), (0x04, 32), (0x08, 0x1_0000), (0x18, total), (0x58, 0x0002_000C)] {
        file[at..at + 4].copy_from_slice(&u32::to_le_bytes(value));
    }
    file.extend(package_header);
    file.extend(bodies);
    file
}

#[test]
fn an_exception_listed_first_wins_over_the_default_and_none_means_none() {
    let file = package(
        &[(0, b'C', b'0', Some(0)), (0, b'D', b'*', None), (0, b'*', b'*', Some(1))],
        &[program(0x8_0000, &[1], &[]), program(0x8_1000, &[2], &[])],
    );
    let package = Package::parse(&file).expect("parses");
    assert_eq!(package.main_program(C0).map(|p| p.start), Some(0x8_0000));
    assert_eq!(package.main_program(Stepping { letter: b'C', digit: b'1' }).map(|p| p.start), Some(0x8_1000));
    assert!(package.main_program(D0).is_none());
}

#[test]
fn a_pipe_dmc_entry_is_never_the_main_program() {
    let file = package(&[(1, b'*', b'*', Some(0))], &[program(0x9_0000, &[1], &[(0x9_2074, 1)])]);
    let package = Package::parse(&file).expect("a pipe DMC past the main window parses");
    assert!(package.main_program(C0).is_none());
}

#[test]
fn a_main_program_outside_the_window_is_refused() {
    let write = package(&[(0, b'*', b'*', Some(0))], &[program(0x8_0000, &[1], &[(0x4_5504, 2)])]);
    assert_eq!(Package::parse(&write).err(), Some(Malformed::OutsideWindow { entry: 0, at: 0x4_5504 }));
    let storage = package(&[(0, b'*', b'*', Some(0))], &[program(0x8_FFFC, &[1, 2], &[])]);
    assert_eq!(Package::parse(&storage).err(), Some(Malformed::OutsideWindow { entry: 0, at: 0x8_FFFC }));
    let unaligned = package(&[(0, b'*', b'*', Some(0))], &[program(0x8_0000, &[1], &[(0x8_F002, 2)])]);
    assert_eq!(Package::parse(&unaligned).err(), Some(Malformed::OutsideWindow { entry: 0, at: 0x8_F002 }));
}

#[test]
fn a_package_that_does_not_hold_together_is_refused() {
    let good = package(&[(0, b'*', b'*', Some(0))], &[program(0x8_0000, &[1, 2], &[(0x8_F000, 1)])]);
    assert!(Package::parse(&good).is_ok());
    let truncated = &good[..good.len() - 4];
    assert_eq!(Package::parse(truncated).err(), Some(Malformed::Css));
    let mut signature = good.clone();
    signature[128 + 400] ^= 1;
    assert_eq!(Package::parse(&signature).err(), Some(Malformed::Program { entry: 0 }));
    let mut writes = good.clone();
    writes[128 + 400 + 0x5C] = 21;
    assert_eq!(Package::parse(&writes).err(), Some(Malformed::Program { entry: 0 }));
    let mut payload = good.clone();
    payload[128 + 400 + 0x0C] = 3;
    assert_eq!(Package::parse(&payload).err(), Some(Malformed::Program { entry: 0 }));
    let mut header = good.clone();
    header[128 + 1] = 3;
    assert_eq!(Package::parse(&header).err(), Some(Malformed::PackageHeader));
    let mut module = good;
    module[0] = 0x0A;
    assert_eq!(Package::parse(&module).err(), Some(Malformed::Css));
}

fn t14_program() -> dmc::Program<'static> {
    Package::parse(TGL_PACKAGE).expect("parses").main_program(C0).expect("a main program")
}

#[test]
fn the_t14_loads_the_program_powers_down_pg3_to_pg5_and_allows_dc6_last() {
    let engine = Engine::t14();
    let main = t14_program();
    let allowed = allow_display_c6(&engine, &engine, &main).expect("allowed");
    assert_eq!((allowed.before, allowed.ddi), ([0x55, 0xff, 0], 1));
    let written = engine.written();
    let words: Vec<_> = main.words().enumerate().map(|(n, w)| (0x8_0000 + 4 * n, w)).collect();
    let header: Vec<_> = main.writes().collect();
    assert_eq!(written[..words.len()], words[..], "the payload is stored first, in order");
    assert_eq!(written[words.len()..words.len() + header.len()], header[..], "then the header's writes");
    let rest = &written[words.len() + header.len()..];
    // PG4 then PG3 withdrawn from PWR_WELL_CTL2, PG5 never requested.
    assert_eq!(
        rest,
        [
            (regs::PWR_WELL_CTL2, 0x7f),
            (regs::PWR_WELL_CTL2, 0x1f),
            (regs::AUDIO_PIN_BUF_CTL, 0),
            (regs::DC_PREREQUISITE, 0b11),
            (regs::DC_STATE_EN, 0b10),
        ]
    );
    assert_eq!(engine.get(regs::PWR_WELL_CTL2), 0x0f, "PG1 and PG2 stay requested and on");
}

#[test]
fn dc_states_already_allowed_are_disallowed_before_the_load() {
    let engine = Engine::t14();
    engine.set(regs::DC_STATE_EN, 0b01);
    allow_display_c6(&engine, &engine, &t14_program()).expect("allowed");
    let written = engine.written();
    assert_eq!(written.first(), Some(&(regs::DC_STATE_EN, 0)));
    assert_eq!(written.last(), Some(&(regs::DC_STATE_EN, 0b10)));
}

fn refused(engine: &Engine, why: Refusal) -> Vec<(usize, u32)> {
    assert_eq!(allow_display_c6(engine, engine, &t14_program()).map(|_| ()), Err(why));
    assert_ne!(engine.get(regs::DC_STATE_EN) & regs::DYNAMIC_DC_STATE, regs::UP_TO_DC6, "DC6 is not allowed");
    engine.written()
}

#[test]
fn a_type_c_port_is_refused_untouched() {
    let engine = Engine::t14();
    engine.set(regs::TRANS_DDI_FUNC_CTL_A, 1 << 31 | 0b0100 << regs::DDI_SELECT_SHIFT);
    assert_eq!(refused(&engine, Refusal::Port(0b0100)), []);
}

/// DDI B is in PG1 too, but DC6 may lose a combo PHY's programming other
/// than port A's.
#[test]
fn a_panel_on_ddi_b_is_refused_untouched() {
    let engine = Engine::t14();
    engine.set(regs::TRANS_DDI_FUNC_CTL_A, 1 << 31 | 0b0010 << regs::DDI_SELECT_SHIFT);
    assert_eq!(refused(&engine, Refusal::Port(0b0010)), []);
}

/// A wait preempted past its deadline while the well was still turning off
/// reads it once more, off, rather than calling it held.
#[test]
fn a_wait_preempted_past_its_deadline_reads_the_well_again() {
    let engine = Engine::t14();
    engine.preempted.set(true);
    allow_display_c6(&engine, &engine, &t14_program()).expect("allowed");
    assert_eq!(engine.get(regs::DC_STATE_EN), 0b10);
}

#[test]
fn vga_display_on_is_refused_untouched() {
    let engine = Engine::t14();
    engine.set(regs::VGA_CONTROL, 0);
    assert_eq!(refused(&engine, Refusal::Vga), []);
}

#[test]
fn storage_that_does_not_hold_the_program_stops_before_any_well() {
    let engine = Engine::t14();
    engine.storage_drops.set(true);
    // Dropped writes leave the two words the storage already held, and zeros:
    // the first payload word past those that is not zero is the one refused.
    let (n, wrote) = t14_program().words().enumerate().skip(2).find(|(_, w)| *w != 0).expect("a non-zero word");
    let written = refused(&engine, Refusal::NotStored { at: 0x8_0000 + 4 * n, wrote, read: 0 });
    assert!(written.iter().all(|(at, _)| dmc::WINDOW.contains(at)), "nothing past the storage is written");
}

#[test]
fn a_well_held_by_another_requester_is_refused_by_name() {
    let engine = Engine::t14();
    engine.held_wells.set(regs::power_well_state(3));
    let written = refused(&engine, Refusal::PowerWellHeld(3));
    assert!(!written.iter().any(|(at, _)| *at == regs::DC_STATE_EN));
}
