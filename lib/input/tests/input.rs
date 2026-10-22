//! The owner of input's merge and the one reader of a source's records.

use toyos_abi::input::{RawKeyEvent, MOD_RELEASED, MOD_SHIFT, POINTER_ABSOLUTE, POINTER_RELATIVE};
use toyos_input::wire::{
    self, Motion, Record, Refused, Report, DEVICES, GONE_LEN, KEY_LEN, MAX_PAYLOAD, MSG_GONE, MSG_KEY, MSG_POINTER, POINTER_LEN,
};
use toyos_input::{Full, Merge, Pointer, Slot, State};

const SHIFT: u8 = 0xE1;
const A: u8 = 0x04;

fn press(keycode: u8) -> RawKeyEvent {
    RawKeyEvent { keycode, modifiers: 0 }
}

fn lift(keycode: u8) -> RawKeyEvent {
    RawKeyEvent { keycode, modifiers: MOD_RELEASED }
}

fn slot(n: usize) -> Slot {
    Slot::connection(n).expect("a connection the port holds")
}

/// A key frame of source 0.
fn key(keycode: u8, modifiers: u8) -> Result<RawKeyEvent, Refused> {
    match wire::decode(MSG_KEY, &[0, 0, keycode, modifiers])? {
        Record::Key { key, .. } => Ok(key),
        other => panic!("a key frame decoded as {other:?}"),
    }
}

/// A pointer frame of source 0.
fn pointer(buttons: u8, kind: u8, x: i16, y: i16) -> Result<Report, Refused> {
    let ([x0, x1], [y0, y1]) = (x.to_le_bytes(), y.to_le_bytes());
    match wire::decode(MSG_POINTER, &[0, 0, buttons, 0, kind, x0, x1, y0, y1])? {
        Record::Pointer(report) => Ok(report),
        other => panic!("a pointer frame decoded as {other:?}"),
    }
}

fn relative(source: u16, buttons: u8, dx: i16, dy: i16) -> Report {
    Report { source, buttons, scroll: 0, motion: Motion::Relative { dx, dy } }
}

#[test]
fn a_key_with_no_usage_or_a_modifier_bit_is_refused() {
    assert_eq!(key(0, 0), Err(Refused::NoUsage));
    assert_eq!(key(0, MOD_RELEASED), Err(Refused::NoUsage));
    for bit in (0..8).map(|n| 1u8 << n).filter(|&bit| bit != MOD_RELEASED) {
        assert_eq!(key(A, bit), Err(Refused::Modifiers(bit)), "modifier bit {bit:#04x}");
        assert_eq!(key(A, bit | MOD_RELEASED), Err(Refused::Modifiers(bit | MOD_RELEASED)));
    }
    assert_eq!(key(A, 0), Ok(press(A)));
    assert_eq!(key(A, MOD_RELEASED), Ok(lift(A)));
}

#[test]
fn a_pointer_report_past_five_buttons_of_no_kind_or_off_the_surface_is_refused() {
    for button in 5..8 {
        let buttons = 1 << button;
        assert_eq!(pointer(buttons, POINTER_RELATIVE, 0, 0), Err(Refused::Buttons(buttons)));
    }
    assert!(pointer(0x1F, POINTER_RELATIVE, 0, 0).is_ok(), "five buttons");
    for kind in 2..=u8::MAX {
        assert_eq!(pointer(0, kind, 0, 0), Err(Refused::Kind(kind)));
    }
    for (x, y) in [(-1, 0), (0, -1), (i16::MIN, 5)] {
        assert_eq!(pointer(0, POINTER_ABSOLUTE, x, y), Err(Refused::Position { x, y }));
    }
    assert_eq!(pointer(1, POINTER_ABSOLUTE, 32767, 0).map(|r| r.motion), Ok(Motion::Absolute { x: 32767, y: 0 }));
    assert_eq!(pointer(1, POINTER_RELATIVE, -300, 255).map(|r| r.motion), Ok(Motion::Relative { dx: -300, dy: 255 }));
}

#[test]
fn a_frame_of_another_type_or_length_is_refused() {
    assert_eq!(wire::decode(0, &[]), Err(Refused::Type(0)));
    assert_eq!(wire::decode(4, &[0, 0, A, 0]), Err(Refused::Type(4)));
    for len in [0, 2, KEY_LEN - 1, KEY_LEN + 1, POINTER_LEN] {
        assert_eq!(wire::decode(MSG_KEY, &vec![A; len]), Err(Refused::Length { msg_type: MSG_KEY, len }));
    }
    for len in [0, KEY_LEN, POINTER_LEN - 1, POINTER_LEN + 1] {
        assert_eq!(wire::decode(MSG_POINTER, &vec![0; len]), Err(Refused::Length { msg_type: MSG_POINTER, len }));
    }
    for len in [0, GONE_LEN - 1, GONE_LEN + 1] {
        assert_eq!(wire::decode(MSG_GONE, &vec![0; len]), Err(Refused::Length { msg_type: MSG_GONE, len }));
    }
}

#[test]
fn every_record_decodes_as_the_frame_it_was_sent_as() {
    let records = [
        Record::Key { source: 0, key: press(A) },
        Record::Key { source: 0xBEEF, key: lift(SHIFT) },
        Record::Pointer(relative(3, 0b101, -7, 300)),
        Record::Pointer(Report { source: 0x1234, buttons: 0, scroll: -2, motion: Motion::Absolute { x: 32767, y: 9 } }),
        Record::Gone { source: 0 },
        Record::Gone { source: u16::MAX },
    ];
    for record in records {
        let mut out = [0; MAX_PAYLOAD];
        let (msg_type, len) = record.frame(&mut out);
        assert_eq!(wire::decode(msg_type, &out[..len]), Ok(record));
    }
}

#[test]
fn shift_from_one_source_and_a_from_another_arrive_as_a_shifted_a() {
    let mut merge = Merge::default();
    assert_eq!(merge.key(slot(0), 0, press(SHIFT)).unwrap().collect::<Vec<_>>(), [RawKeyEvent { keycode: SHIFT, modifiers: MOD_SHIFT }]);
    assert_eq!(merge.key(slot(3), 0, press(A)).unwrap().collect::<Vec<_>>(), [RawKeyEvent { keycode: A, modifiers: MOD_SHIFT }]);
    assert_eq!(merge.key(slot(0), 0, lift(SHIFT)).unwrap().collect::<Vec<_>>(), [RawKeyEvent { keycode: SHIFT, modifiers: MOD_RELEASED }]);
    assert_eq!(merge.key(slot(3), 0, lift(A)).unwrap().collect::<Vec<_>>(), [RawKeyEvent { keycode: A, modifiers: MOD_RELEASED }]);
}

#[test]
fn a_repeated_make_from_one_source_delivers_nothing() {
    let mut merge = Merge::default();
    assert_eq!(merge.key(slot(1), 0, press(A)).unwrap().count(), 1);
    assert_eq!(merge.key(slot(1), 0, press(A)).unwrap().count(), 0);
    assert_eq!(merge.key(slot(2), 0, press(A)).unwrap().count(), 0, "a key another source holds");
    assert_eq!(merge.key(slot(1), 0, lift(A)).unwrap().count(), 0, "still held by the other source");
    assert_eq!(merge.key(slot(2), 0, lift(A)).unwrap().collect::<Vec<_>>(), [lift(A)]);
    assert_eq!(merge.key(slot(2), 0, lift(A)).unwrap().count(), 0, "a release of what is not held");
}

#[test]
fn a_source_that_ends_releases_what_it_held_and_nothing_another_holds() {
    let mut merge = Merge::default();
    merge.key(slot(0), 0, press(SHIFT)).unwrap().for_each(drop);
    merge.key(slot(0), 0, press(A)).unwrap().for_each(drop);
    merge.key(slot(3), 0, press(SHIFT)).unwrap().for_each(drop);
    assert_eq!(merge.end(slot(0)).collect::<Vec<_>>(), [RawKeyEvent { keycode: A, modifiers: MOD_SHIFT | MOD_RELEASED }]);
    assert_eq!(merge.end(slot(0)).count(), 0, "it holds nothing now");
    assert_eq!(merge.end(slot(3)).collect::<Vec<_>>(), [RawKeyEvent { keycode: SHIFT, modifiers: MOD_RELEASED }]);
}

#[test]
fn two_pointers_buttons_or_and_ones_empty_report_clears_none_of_the_others() {
    let mut pointer = Pointer::new(1920, 1080);
    assert_eq!(pointer.report(slot(0), relative(0, 1, 0, 0)).unwrap().buttons, 1);
    assert_eq!(pointer.report(slot(3), relative(4, 2, 0, 0)).unwrap().buttons, 3, "another slot's pointer");
    assert_eq!(pointer.report(slot(3), relative(5, 0, 0, 0)).unwrap().buttons, 3, "a third pointer, holding nothing");
    assert_eq!(pointer.report(slot(3), relative(4, 0, 0, 0)).unwrap().buttons, 1);
    assert_eq!(pointer.report(slot(0), relative(1, 4, 0, 0)).unwrap().buttons, 5, "one slot's two pointers");
}

#[test]
fn a_connections_end_clears_its_pointers_buttons_and_no_others() {
    let mut pointer = Pointer::new(1920, 1080);
    pointer.report(slot(2), relative(0, 1, 0, 0)).unwrap();
    pointer.report(slot(2), relative(9, 4, 0, 0)).unwrap();
    pointer.report(slot(1), relative(0, 2, 0, 0)).unwrap();
    assert_eq!(pointer.end(slot(2)).buttons, 2);
    assert_eq!(pointer.end(slot(1)).buttons, 0);
}

#[test]
fn relative_motion_scales_by_the_screen_and_absolute_motion_sets() {
    // 1080 rows' worth of 64 counts each, so a count is 64 down and 64 * 1080 / 1920 = 36 across.
    let mut pointer = Pointer::new(1920, 1080);
    let start = pointer.report(slot(0), relative(0, 0, 0, 0)).unwrap();
    assert_eq!((start.x, start.y), (16384, 16384), "it starts in the middle");
    let moved = pointer.report(slot(0), relative(0, 0, 10, -3)).unwrap();
    assert_eq!((moved.x, moved.y), (16384 + 10 * 36, 16384 - 3 * 64));
    let set = pointer.report(slot(1), Report { source: 0, buttons: 0, scroll: 1, motion: Motion::Absolute { x: 7, y: 9 } }).unwrap();
    assert_eq!(set, State { buttons: 0, scroll: 1, x: 7, y: 9 });
    let clamped = pointer.report(slot(0), relative(0, 0, i16::MIN, i16::MAX)).unwrap();
    assert_eq!((clamped.x, clamped.y), (0, 32767), "the edges hold it");
    pointer.screen(800, 800);
    let square = pointer.report(slot(0), relative(0, 0, 1, -1)).unwrap();
    assert_eq!((square.x, square.y), (64, 32767 - 64));
}

/// A key usbd held at its death is released by the owner: its connection's
/// end releases every device it carried. A `gone` releases its own device
/// alone, and neither touches another connection's.
#[test]
fn a_connections_end_releases_every_device_it_carried_and_a_gone_only_its_own_source() {
    const B: u8 = 0x05;
    const C: u8 = 0x06;
    let mut merge = Merge::default();
    let mut pointer = Pointer::new(1920, 1080);
    // Connection 0 carries two keyboards and two pointers; connection 1 one of each.
    merge.key(slot(0), 7, press(A)).unwrap().for_each(drop);
    merge.key(slot(0), 9, press(B)).unwrap().for_each(drop);
    merge.key(slot(1), 0, press(C)).unwrap().for_each(drop);
    pointer.report(slot(0), relative(7, 0b001, 0, 0)).unwrap();
    pointer.report(slot(0), relative(9, 0b010, 0, 0)).unwrap();
    pointer.report(slot(1), relative(0, 0b100, 0, 0)).unwrap();

    assert_eq!(merge.gone(slot(0), 7).collect::<Vec<_>>(), [lift(A)], "source 7's key, and no other");
    assert_eq!(pointer.gone(slot(0), 7).buttons, 0b110, "source 7's button, and no other");

    assert_eq!(merge.end(slot(0)).collect::<Vec<_>>(), [lift(B)], "every device left on the connection");
    assert_eq!(pointer.end(slot(0)).buttons, 0b100, "every pointer on the connection");

    assert_eq!(merge.end(slot(1)).collect::<Vec<_>>(), [lift(C)]);
    assert_eq!(pointer.end(slot(1)).buttons, 0);
}

/// A device's number is the sender's and is never mistaken for another's: a
/// `gone` of a source holding nothing releases nothing.
#[test]
fn a_gone_of_a_source_that_holds_nothing_releases_nothing() {
    let mut merge = Merge::default();
    merge.key(slot(0), 1, press(A)).unwrap().for_each(drop);
    assert_eq!(merge.gone(slot(0), 2).count(), 0);
    assert_eq!(merge.gone(slot(1), 1).count(), 0, "the same number on another connection");
    assert_eq!(merge.key(slot(0), 1, lift(A)).unwrap().collect::<Vec<_>>(), [lift(A)]);
}

/// A connection holds at most `DEVICES` devices holding something; the next
/// is the sender breaking the protocol, refused by name and changing nothing,
/// and a device that holds nothing again frees its place.
#[test]
fn a_connection_past_its_devices_is_refused_and_an_idle_device_holds_no_place() {
    let mut merge = Merge::default();
    let mut pointer = Pointer::new(1920, 1080);
    for source in 0..DEVICES as u16 {
        merge.key(slot(0), source, press(A)).unwrap().for_each(drop);
        pointer.report(slot(0), relative(source, 1, 0, 0)).unwrap();
    }
    let past = DEVICES as u16;
    assert_eq!(merge.key(slot(0), past, press(SHIFT)).err(), Some(Full { source: past }));
    assert_eq!(pointer.report(slot(0), relative(past, 2, 0, 0)), Err(Full { source: past }));
    assert_eq!(pointer.report(slot(1), relative(past, 2, 0, 0)).unwrap().buttons, 3, "another connection has room");
    // One device lets go of everything, and the next has its place.
    merge.key(slot(0), 0, lift(A)).unwrap().for_each(drop);
    pointer.report(slot(0), relative(0, 0, 0, 0)).unwrap();
    assert!(merge.key(slot(0), past, press(SHIFT)).is_ok());
    assert!(pointer.report(slot(0), relative(past, 2, 0, 0)).is_ok());
}
