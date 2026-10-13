//! The owner of input's merge and the one reader of a source's records.

use toyos_abi::input::{MouseEvent, RawKeyEvent, MOD_RELEASED, MOD_SHIFT, POINTER_ABSOLUTE, POINTER_RELATIVE};
use toyos_input::wire::{self, Motion, Record, Refused, Report, KEY_LEN, MSG_KEY, MSG_LOST, MSG_POINTER, POINTER_LEN};
use toyos_input::{Merge, Pointer, Slot, State};

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

fn bytes(event: &MouseEvent) -> [u8; POINTER_LEN] {
    toyos_abi::usersafe::bytes(event).try_into().expect("a record's length")
}

fn relative(source: u8, buttons: u8, dx: i16, dy: i16) -> Report {
    Report { source, buttons, scroll: 0, motion: Motion::Relative { dx, dy } }
}

#[test]
fn a_key_with_no_usage_or_a_modifier_bit_is_refused() {
    assert_eq!(wire::key(&[0, 0]), Err(Refused::NoUsage));
    assert_eq!(wire::key(&[0, MOD_RELEASED]), Err(Refused::NoUsage));
    for bit in (0..8).map(|n| 1u8 << n).filter(|&bit| bit != MOD_RELEASED) {
        assert_eq!(wire::key(&[A, bit]), Err(Refused::Modifiers(bit)), "modifier bit {bit:#04x}");
        assert_eq!(wire::key(&[A, bit | MOD_RELEASED]), Err(Refused::Modifiers(bit | MOD_RELEASED)));
    }
    assert_eq!(wire::key(&[A, 0]), Ok(press(A)));
    assert_eq!(wire::key(&[A, MOD_RELEASED]), Ok(lift(A)));
}

#[test]
fn a_pointer_report_past_five_buttons_of_no_kind_or_off_the_surface_is_refused() {
    let event = |buttons, kind, x, y| MouseEvent { source: 0, buttons, scroll: 0, kind, x, y };
    for button in 5..8 {
        let buttons = 1 << button;
        assert_eq!(wire::pointer(&bytes(&event(buttons, POINTER_RELATIVE, 0, 0))), Err(Refused::Buttons(buttons)));
    }
    assert!(wire::pointer(&bytes(&event(0x1F, POINTER_RELATIVE, 0, 0))).is_ok(), "five buttons");
    for kind in 2..=u8::MAX {
        assert_eq!(wire::pointer(&bytes(&event(0, kind, 0, 0))), Err(Refused::Kind(kind)));
    }
    for (x, y) in [(-1, 0), (0, -1), (i16::MIN, 5)] {
        assert_eq!(wire::pointer(&bytes(&event(0, POINTER_ABSOLUTE, x, y))), Err(Refused::Position { x, y }));
    }
    assert_eq!(
        wire::pointer(&bytes(&event(1, POINTER_ABSOLUTE, 32767, 0))).map(|r| r.motion),
        Ok(Motion::Absolute { x: 32767, y: 0 })
    );
    assert_eq!(
        wire::pointer(&bytes(&event(1, POINTER_RELATIVE, -300, 255))).map(|r| r.motion),
        Ok(Motion::Relative { dx: -300, dy: 255 })
    );
}

#[test]
fn a_frame_of_another_type_or_length_is_refused() {
    assert_eq!(wire::decode(0, &[]), Err(Refused::Type(0)));
    assert_eq!(wire::decode(4, &[A, 0]), Err(Refused::Type(4)));
    for len in [0, KEY_LEN - 1, KEY_LEN + 1, POINTER_LEN] {
        assert_eq!(wire::decode(MSG_KEY, &vec![A; len]), Err(Refused::Length { msg_type: MSG_KEY, len }));
    }
    for len in [0, KEY_LEN, POINTER_LEN - 1, POINTER_LEN + 1] {
        assert_eq!(wire::decode(MSG_POINTER, &vec![0; len]), Err(Refused::Length { msg_type: MSG_POINTER, len }));
    }
    assert_eq!(wire::decode(MSG_LOST, &[0]), Err(Refused::Length { msg_type: MSG_LOST, len: 1 }));
}

#[test]
fn every_record_decodes_as_the_frame_it_was_sent_as() {
    let records = [
        Record::Key(press(A)),
        Record::Key(lift(SHIFT)),
        Record::Pointer(relative(3, 0b101, -7, 300)),
        Record::Pointer(Report { source: 0, buttons: 0, scroll: -2, motion: Motion::Absolute { x: 32767, y: 9 } }),
        Record::Lost,
    ];
    for record in records {
        let mut out = [0; POINTER_LEN];
        let (msg_type, len) = record.frame(&mut out);
        assert_eq!(wire::decode(msg_type, &out[..len]), Ok(record));
    }
}

#[test]
fn shift_from_one_source_and_a_from_another_arrive_as_a_shifted_a() {
    let mut merge = Merge::default();
    assert_eq!(merge.key(slot(0), press(SHIFT)).collect::<Vec<_>>(), [RawKeyEvent { keycode: SHIFT, modifiers: MOD_SHIFT }]);
    assert_eq!(merge.key(Slot::KERNEL, press(A)).collect::<Vec<_>>(), [RawKeyEvent { keycode: A, modifiers: MOD_SHIFT }]);
    assert_eq!(merge.key(slot(0), lift(SHIFT)).collect::<Vec<_>>(), [RawKeyEvent { keycode: SHIFT, modifiers: MOD_RELEASED }]);
    assert_eq!(merge.key(Slot::KERNEL, lift(A)).collect::<Vec<_>>(), [RawKeyEvent { keycode: A, modifiers: MOD_RELEASED }]);
}

#[test]
fn a_repeated_make_from_one_source_delivers_nothing() {
    let mut merge = Merge::default();
    assert_eq!(merge.key(slot(1), press(A)).count(), 1);
    assert_eq!(merge.key(slot(1), press(A)).count(), 0);
    assert_eq!(merge.key(slot(2), press(A)).count(), 0, "a key another source holds");
    assert_eq!(merge.key(slot(1), lift(A)).count(), 0, "still held by the other source");
    assert_eq!(merge.key(slot(2), lift(A)).collect::<Vec<_>>(), [lift(A)]);
    assert_eq!(merge.key(slot(2), lift(A)).count(), 0, "a release of what is not held");
}

#[test]
fn a_source_that_ends_releases_what_it_held_and_nothing_another_holds() {
    let mut merge = Merge::default();
    merge.key(slot(0), press(SHIFT)).for_each(drop);
    merge.key(slot(0), press(A)).for_each(drop);
    merge.key(slot(3), press(SHIFT)).for_each(drop);
    assert_eq!(merge.end(slot(0)).collect::<Vec<_>>(), [RawKeyEvent { keycode: A, modifiers: MOD_SHIFT | MOD_RELEASED }]);
    assert_eq!(merge.end(slot(0)).count(), 0, "it holds nothing now");
    assert_eq!(merge.end(slot(3)).collect::<Vec<_>>(), [RawKeyEvent { keycode: SHIFT, modifiers: MOD_RELEASED }]);
}

#[test]
fn two_pointers_buttons_or_and_ones_empty_report_clears_none_of_the_others() {
    let mut pointer = Pointer::new(1920, 1080);
    assert_eq!(pointer.report(slot(0), relative(0, 1, 0, 0)).buttons, 1);
    assert_eq!(pointer.report(Slot::KERNEL, relative(4, 2, 0, 0)).buttons, 3, "another slot's pointer");
    assert_eq!(pointer.report(Slot::KERNEL, relative(5, 0, 0, 0)).buttons, 3, "a third pointer, holding nothing");
    assert_eq!(pointer.report(Slot::KERNEL, relative(4, 0, 0, 0)).buttons, 1);
    assert_eq!(pointer.report(slot(0), relative(1, 4, 0, 0)).buttons, 5, "one slot's two pointers");
}

#[test]
fn a_connections_end_clears_its_pointers_buttons_and_no_others() {
    let mut pointer = Pointer::new(1920, 1080);
    pointer.report(slot(2), relative(0, 1, 0, 0));
    pointer.report(slot(2), relative(9, 4, 0, 0));
    pointer.report(slot(1), relative(0, 2, 0, 0));
    assert_eq!(pointer.end(slot(2)).buttons, 2);
    assert_eq!(pointer.end(slot(1)).buttons, 0);
}

#[test]
fn relative_motion_scales_by_the_screen_and_absolute_motion_sets() {
    // 1080 rows' worth of 64 counts each, so a count is 64 down and 64 * 1080 / 1920 = 36 across.
    let mut pointer = Pointer::new(1920, 1080);
    let start = pointer.report(slot(0), relative(0, 0, 0, 0));
    assert_eq!((start.x, start.y), (16384, 16384), "it starts in the middle");
    let moved = pointer.report(slot(0), relative(0, 0, 10, -3));
    assert_eq!((moved.x, moved.y), (16384 + 10 * 36, 16384 - 3 * 64));
    let set = pointer.report(slot(1), Report { source: 0, buttons: 0, scroll: 1, motion: Motion::Absolute { x: 7, y: 9 } });
    assert_eq!(set, State { buttons: 0, scroll: 1, x: 7, y: 9 });
    let clamped = pointer.report(slot(0), relative(0, 0, i16::MIN, i16::MAX));
    assert_eq!((clamped.x, clamped.y), (0, 32767), "the edges hold it");
    pointer.screen(800, 800);
    let square = pointer.report(slot(0), relative(0, 0, 1, -1));
    assert_eq!((square.x, square.y), (64, 32767 - 64));
}
