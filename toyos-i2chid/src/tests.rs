extern crate std;

use std::vec::Vec;

use crate::report::{self, Field, Motion, Mouse, Refused, Unread};
use crate::*;

/// HID 1.11 Appendix E.10's example mouse: three buttons, five bits of
/// padding, X and Y as signed bytes, no report id.
const APPENDIX_E10: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06,
    0xC0, 0xC0,
];

/// Built after a precision touchpad's shape, not captured from one: a
/// digitizer collection (report id 4) before the mouse collection (report id
/// 1: two buttons, six bits of padding, X and Y as signed 16 bits, a wheel
/// byte), and a vendor feature after it.
const TOUCHPAD: &[u8] = &[
    // Touch Pad application collection, report id 4: a contact's tip and
    // id byte, then X and Y absolute 16 bits.
    0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x04, 0x09, 0x22, 0xA1, 0x02, 0x15, 0x00, 0x25,
    0x01, 0x09, 0x42, 0x95, 0x01, 0x75, 0x01, 0x81, 0x02, 0x95, 0x07, 0x81, 0x03, 0x05, 0x01,
    0x26, 0xFF, 0x0F, 0x75, 0x10, 0x95, 0x01, 0x09, 0x30, 0x81, 0x02, 0x09, 0x31, 0x81, 0x02,
    0xC0, 0xC0,
    // Mouse, report id 1.
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19,
    0x01, 0x29, 0x02, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0x95, 0x06,
    0x81, 0x03, 0x05, 0x01, 0x16, 0x01, 0x80, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x02, 0x09,
    0x30, 0x09, 0x31, 0x81, 0x06, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x09, 0x38,
    0x81, 0x06, 0xC0, 0xC0,
    // A vendor feature, report id 5.
    0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x05, 0x09, 0x02, 0x75, 0x08, 0x95, 0x03,
    0xB1, 0x02, 0xC0,
];

#[test]
fn appendix_e10_reads_as_its_three_bytes() {
    let m = report::mouse(APPENDIX_E10).unwrap();
    assert_eq!(m.report_id, None);
    assert_eq!(m.buttons[..4], [Some(0), Some(1), Some(2), None]);
    assert_eq!(m.x, Field { bit: 8, size: 8, signed: true });
    assert_eq!(m.y, Field { bit: 16, size: 8, signed: true });
    assert_eq!(m.wheel, None);
    assert_eq!(m.len, 3);
    assert_eq!(m.read(&[0x05, 0x80, 0x7F]), Ok(Motion { buttons: 0b101, dx: -128, dy: 127, wheel: 0 }));
    assert_eq!(m.read(&[0x00, 0x01]), Err(Unread::Short(2)));
}

#[test]
fn the_touchpad_mouse_is_found_behind_another_collection() {
    let m = report::mouse(TOUCHPAD).unwrap();
    assert_eq!(m.report_id, Some(1));
    assert_eq!(m.buttons[..3], [Some(0), Some(1), None]);
    assert_eq!(m.x, Field { bit: 8, size: 16, signed: true });
    assert_eq!(m.y, Field { bit: 24, size: 16, signed: true });
    assert_eq!(m.wheel, Some(Field { bit: 40, size: 8, signed: true }));
    assert_eq!(m.len, 6);
    assert_eq!(m.read_len(), 9);
    assert_eq!(
        m.read(&[0x01, 0x02, 0xFE, 0xFF, 0x00, 0x01, 0xFF]),
        Ok(Motion { buttons: 0b10, dx: -2, dy: 256, wheel: -1 })
    );
    assert_eq!(m.read(&[0x04, 0, 0, 0, 0, 0, 0]), Err(Unread::OtherReport(4)));
    assert_eq!(m.read(&[0x01, 0, 0]), Err(Unread::Short(3)));
    assert_eq!(m.read(&[]), Err(Unread::Short(0)));
}

#[test]
fn a_mouse_with_absolute_motion_is_refused() {
    let mut d = APPENDIX_E10.to_vec();
    // X and Y's Input(Data,Var,Rel) becomes Input(Data,Var,Abs).
    assert_eq!(d[47], 0x06);
    d[47] = 0x02;
    assert_eq!(report::mouse(&d), Err(Refused::Absolute));
}

#[test]
fn a_descriptor_with_no_mouse_is_refused() {
    assert_eq!(report::mouse(&TOUCHPAD[..47]), Err(Refused::NoMouse));
    assert_eq!(report::mouse(&[]), Err(Refused::NoMouse));
}

#[test]
fn malformed_descriptors_are_refused_by_name() {
    assert_eq!(report::mouse(&[0xC0]), Err(Refused::Unbalanced(0)));
    assert_eq!(report::mouse(&[0x05, 0x01, 0x26, 0xFF]), Err(Refused::Truncated(2)));
    assert_eq!(report::mouse(&[0xFE, 0x10, 0x00]), Err(Refused::Truncated(19)));
    assert_eq!(report::mouse(&[0xA1, 0x00].repeat(17)), Err(Refused::TooDeep(32)));
    let mut wide = APPENDIX_E10.to_vec();
    // Y's Report Size 8 becomes 33.
    assert_eq!(wide[43], 0x08);
    wide[43] = 33;
    assert_eq!(report::mouse(&wide), Err(Refused::FieldSize(46)));
}

/// Total over every byte string: refused or read, never a panic, and a read
/// one's fields lie inside the report it says it is.
#[test]
fn every_descriptor_is_read_or_refused() {
    let mut state = 0x6932_6368_6964u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for case in 0..100_000 {
        let base: &[u8] = if case % 2 == 0 { APPENDIX_E10 } else { TOUCHPAD };
        let mut d: Vec<u8> = base.to_vec();
        for _ in 0..(next() % 6) {
            let at = (next() as usize) % d.len();
            d[at] = next() as u8;
        }
        d.truncate((next() as usize) % (d.len() + 1));
        if let Ok(m) = report::mouse(&d) {
            for f in [Some(m.x), Some(m.y), m.wheel].into_iter().flatten() {
                assert!((f.bit + f.size as u32) as usize <= m.len * 8, "case {case}: {m:?}");
            }
            let report: Vec<u8> = (0..(next() % 12)).map(|_| next() as u8).collect();
            let _ = m.read(&report);
        }
    }
}

fn hid_descriptor() -> [u8; 30] {
    let words: [u16; 13] = [30, 0x0100, 0x02B6, 0x0002, 0x0003, 0x0020, 0x0004, 0, 0x0005, 0x0006, 0x04F3, 0x3195, 0x0001];
    let mut b = [0u8; 30];
    for (i, w) in words.iter().enumerate() {
        b[2 * i..2 * i + 2].copy_from_slice(&w.to_le_bytes());
    }
    b
}

#[test]
fn a_hid_descriptor_reads_as_its_fields() {
    assert_eq!(
        HidDescriptor::parse(&hid_descriptor()),
        Ok(HidDescriptor {
            report_descriptor_len: 0x02B6,
            report_descriptor_register: 2,
            input_register: 3,
            max_input_len: 0x20,
            command_register: 5,
            data_register: 6,
            vendor: 0x04F3,
            product: 0x3195,
            version: 1,
        })
    );
}

#[test]
fn a_hid_descriptor_that_is_not_one_is_refused() {
    assert_eq!(HidDescriptor::parse(&[0x1E; 29]), Err(DescriptorRefused::Short(29)));
    assert_eq!(HidDescriptor::parse(&[0xFF; 30]), Err(DescriptorRefused::Length(0xFFFF)));
    let mut d = hid_descriptor();
    d[3] = 2;
    assert_eq!(HidDescriptor::parse(&d), Err(DescriptorRefused::Version(0x0200)));
    let mut d = hid_descriptor();
    d[10] = 1;
    d[11] = 0;
    assert_eq!(HidDescriptor::parse(&d), Err(DescriptorRefused::MaxInput(1)));
}

#[test]
fn an_input_read_splits_at_its_length() {
    assert_eq!(input(&[0, 0, 9, 9]), Input::Nothing);
    assert_eq!(input(&[5, 0, 1, 2, 3, 9]), Input::Report(&[1, 2, 3]));
    assert_eq!(input(&[2, 0]), Input::Report(&[]));
    assert_eq!(input(&[1, 0, 7]), Input::Refused { len: 1 });
    assert_eq!(input(&[7, 0, 1, 2]), Input::Refused { len: 7 });
    assert_eq!(input(&[0xFF, 0xFF, 0]), Input::Refused { len: 0xFFFF });
    assert_eq!(input(&[1]), Input::Refused { len: 0 });
}

#[test]
fn commands_are_the_register_then_the_word() {
    assert_eq!(command(0x0005, SET_POWER, POWER_ON), [0x05, 0x00, 0x00, 0x08]);
    assert_eq!(command(0x0105, RESET, 0), [0x05, 0x01, 0x00, 0x01]);
}

#[test]
fn a_mouse_layout_reads_nothing_past_its_report() {
    let m = Mouse {
        report_id: None,
        buttons: [None; report::BUTTONS],
        x: Field { bit: 0, size: 32, signed: true },
        y: Field { bit: 32, size: 12, signed: true },
        wheel: None,
        len: 6,
    };
    assert_eq!(m.read(&[0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x08]), Ok(Motion { buttons: 0, dx: -1, dy: -2048, wheel: 0 }));
}
