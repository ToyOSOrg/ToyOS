extern crate std;

use std::collections::BTreeSet;
use std::vec;
use std::vec::Vec;

use toyos_input::keys::Transition;
use toyos_input::wire::Motion;

use crate::descriptor::{self, Kind, Refused as Unread, Unbound};
use crate::report::{Hid, Pointer, Read, Refused};

/// HID 1.11 Appendix E.6, the boot keyboard's descriptor: eight modifier
/// bits, a constant byte, five LED outputs and their padding, and six array
/// slots of Logical Maximum 0x65 over Usage Minimum 0 to Maximum 0x65.
const E6_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95,
    0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05,
    0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19,
    0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
];

/// HID 1.11 Appendix E.10, the boot mouse's descriptor: three buttons, five
/// bits of padding, and X and Y as signed relative bytes.
const E10_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25,
    0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31,
    0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06, 0xC0, 0xC0,
];

fn hid(descriptor: &[u8]) -> Hid {
    Hid::new(descriptor::parse(descriptor).expect("a descriptor this reader reads"))
}

fn press(usage: u8) -> Transition {
    Transition { usage, pressed: true }
}

fn lift(usage: u8) -> Transition {
    Transition { usage, pressed: false }
}

/// What a keyboard report made, collected: `Ok((transitions, rolled))`.
fn keys(hid: &mut Hid, report: &[u8]) -> Result<(Vec<Transition>, bool), Refused> {
    match hid.report(report)? {
        Read::Keys { made, rolled, .. } => Ok((made.collect(), rolled)),
        other => panic!("{report:02x?} read as {other:?}"),
    }
}

/// A report that named every key down.
fn fed(hid: &mut Hid, report: &[u8]) -> Vec<Transition> {
    match keys(hid, report) {
        Ok((made, false)) => made,
        other => panic!("{report:02x?}: {other:?}"),
    }
}

fn pointer(hid: &mut Hid, report: &[u8]) -> Pointer {
    match hid.report(report) {
        Ok(Read::Pointer { report, .. }) => report,
        other => panic!("{report:02x?} read as {other:?}"),
    }
}

/// Descriptor items, spelled.
mod item {
    pub fn page(p: u8) -> [u8; 2] {
        [0x05, p]
    }
    pub fn usage(u: u8) -> [u8; 2] {
        [0x09, u]
    }
    pub fn usage_min(u: u8) -> [u8; 2] {
        [0x19, u]
    }
    pub fn usage_max(u: u8) -> [u8; 2] {
        [0x29, u]
    }
    pub fn logical_min(v: i8) -> [u8; 2] {
        [0x15, v as u8]
    }
    pub fn logical_max(v: u8) -> [u8; 2] {
        [0x25, v]
    }
    pub fn logical_min16(v: i16) -> [u8; 3] {
        let [a, b] = v.to_le_bytes();
        [0x16, a, b]
    }
    pub fn logical_max16(v: i16) -> [u8; 3] {
        let [a, b] = v.to_le_bytes();
        [0x26, a, b]
    }
    pub fn size(bits: u8) -> [u8; 2] {
        [0x75, bits]
    }
    pub fn count(n: u8) -> [u8; 2] {
        [0x95, n]
    }
    pub fn id(id: u8) -> [u8; 2] {
        [0x85, id]
    }
    pub const APPLICATION: [u8; 2] = [0xA1, 0x01];
    pub const END: [u8; 1] = [0xC0];
    pub const VARIABLE: [u8; 2] = [0x81, 0x02];
    pub const RELATIVE: [u8; 2] = [0x81, 0x06];
    pub const ARRAY: [u8; 2] = [0x81, 0x00];
    pub const CONSTANT: [u8; 2] = [0x81, 0x01];
}

fn spell(items: &[&[u8]]) -> Vec<u8> {
    items.concat()
}

#[test]
fn the_boot_descriptors_bind_a_keyboard_and_a_pointer() {
    let keyboard = descriptor::parse(E6_KEYBOARD).unwrap();
    assert_eq!(keyboard.collections.len(), 1);
    assert_eq!(keyboard.collections[0].bound, Ok(Kind::Keyboard));
    assert!(!keyboard.numbered);
    let mouse = descriptor::parse(E10_MOUSE).unwrap();
    assert_eq!(mouse.collections.len(), 1);
    assert_eq!(mouse.collections[0].bound, Ok(Kind::Pointer));
}

/// The boot keyboard's report byte for byte, through its descriptor: bit n
/// of byte 0 is usage 0xE0 + n, byte 1 is constant, and each slot names its
/// usage.
#[test]
fn a_keyboard_report_is_read_as_its_descriptor_lays_it_out() {
    for n in 0..8u8 {
        let mut kb = hid(E6_KEYBOARD);
        assert_eq!(fed(&mut kb, &[1 << n, 0, 0, 0, 0, 0, 0, 0]), vec![press(0xE0 + n)]);
    }
    let mut kb = hid(E6_KEYBOARD);
    assert_eq!(fed(&mut kb, &[0, 0xFF, 0, 0, 0, 0, 0, 0]), vec![], "byte 1 is constant");
    for slot in 2..8 {
        let mut kb = hid(E6_KEYBOARD);
        let mut report = [0u8; 8];
        report[slot] = 0x04;
        assert_eq!(fed(&mut kb, &report), vec![press(0x04)], "slot {slot}");
    }
}

/// Shift before the key it shifts, and a release before the press that took
/// its slot.
#[test]
fn modifiers_come_first_then_releases_then_presses() {
    let mut kb = hid(E6_KEYBOARD);
    assert_eq!(fed(&mut kb, &[0x02, 0, 0x05, 0, 0, 0, 0, 0]), vec![press(0xE1), press(0x05)]);
    assert_eq!(fed(&mut kb, &[0x00, 0, 0x06, 0, 0, 0, 0, 0]), vec![lift(0xE1), lift(0x05), press(0x06)]);
    assert_eq!(fed(&mut kb, &[0x05, 0, 0x07, 0, 0, 0, 0, 0]), vec![press(0xE0), press(0xE2), lift(0x06), press(0x07)]);
    assert_eq!(fed(&mut kb, &[0x05, 0, 0x07, 0, 0, 0, 0, 0]), vec![], "nothing changed");
}

/// A key that moves slot is not a release and a press; a usage held twice is
/// held once.
#[test]
fn a_report_is_a_set_of_keys_and_not_a_list_of_slots() {
    let mut kb = hid(E6_KEYBOARD);
    assert_eq!(fed(&mut kb, &[0, 0, 0x04, 0x05, 0, 0, 0, 0]), vec![press(0x04), press(0x05)]);
    assert_eq!(fed(&mut kb, &[0, 0, 0x05, 0x04, 0, 0, 0, 0]), vec![]);
    assert_eq!(fed(&mut kb, &[0, 0, 0x05, 0x05, 0x04, 0x05, 0, 0]), vec![]);
    assert_eq!(fed(&mut kb, &[0x01, 0, 0, 0, 0, 0, 0, 0]), vec![press(0xE0), lift(0x04), lift(0x05)]);
    assert_eq!(fed(&mut kb, &[0; 8]), vec![lift(0xE0)]);
}

/// POSTFail and ErrorUndefined in any slot refuse the report, and the keys
/// held before are still held.
#[test]
fn a_fault_usage_is_refused_and_the_keys_held_stay_held() {
    for (usage, why) in [(2, Refused::PostFail), (3, Refused::Undefined)] {
        for slot in 2..8 {
            for beside in [0x04, 1] {
                let mut kb = hid(E6_KEYBOARD);
                fed(&mut kb, &[0x02, 0, 0x04, 0x05, 0, 0, 0, 0]);
                let before = kb.clone();
                let mut report = [0x00, 0, beside, beside, beside, beside, beside, beside];
                report[slot] = usage;
                assert_eq!(keys(&mut kb, &report), Err(why), "usage {usage} in slot {slot}");
                assert_eq!(kb, before, "a refused report changed what is held");
            }
        }
    }
}

/// Appendix C's phantom state in any slot: the array is not believed, so the
/// keys it held stay held, and the modifier bitmap is, so a modifier moves.
#[test]
fn a_rollover_report_believes_its_modifiers_and_not_its_slots() {
    for slot in 2..8 {
        let mut kb = hid(E6_KEYBOARD);
        fed(&mut kb, &[0x02, 0, 0x04, 0x05, 0, 0, 0, 0]);
        let mut report = [0x00, 0, 0x06, 0, 0, 0, 0, 0];
        report[slot] = 1;
        assert_eq!(keys(&mut kb, &report), Ok((vec![lift(0xE1)], true)), "rollover in slot {slot}");
        assert_eq!(fed(&mut kb, &[0, 0, 0x04, 0x05, 0, 0, 0, 0]), vec![], "slot {slot}");
    }
    let mut kb = hid(E6_KEYBOARD);
    fed(&mut kb, &[0x02, 0, 0x04, 0x05, 0, 0, 0, 0]);
    assert_eq!(keys(&mut kb, &[0x00, 0, 1, 1, 1, 1, 1, 1]), Ok((vec![lift(0xE1)], true)));
    assert_eq!(keys(&mut kb, &[0x01, 0, 1, 1, 1, 1, 1, 1]), Ok((vec![press(0xE0)], true)));
    assert_eq!(keys(&mut kb, &[0x00, 0, 1, 1, 1, 1, 1, 1]), Ok((vec![lift(0xE0)], true)));
    assert_eq!(
        fed(&mut kb, &[0x00, 0, 0x05, 0, 0, 0, 0, 0]),
        vec![lift(0x04)],
        "the next whole report is diffed against the keys held through the rollover"
    );
}

/// A report shorter than its fields is refused; a longer one is read for the
/// fields it holds.
#[test]
fn a_report_shorter_than_its_descriptor_is_refused() {
    for len in 0..8 {
        let mut kb = hid(E6_KEYBOARD);
        fed(&mut kb, &[0, 0, 0x04, 0, 0, 0, 0, 0]);
        let before = kb.clone();
        assert_eq!(keys(&mut kb, &vec![0x02; len]), Err(Refused::Short { id: 0, len }));
        assert_eq!(kb, before);
    }
    let mut kb = hid(E6_KEYBOARD);
    assert_eq!(fed(&mut kb, &[0, 0, 0x04, 0, 0, 0, 0, 0, 0xFF]), vec![press(0x04)]);
}

#[test]
fn a_keyboard_leaving_releases_everything_it_held() {
    let mut kb = hid(E6_KEYBOARD);
    fed(&mut kb, &[0x81, 0xFF, 0x04, 0x65, 0, 0, 0, 0]);
    let released: Vec<_> = kb.release(0).unwrap().collect();
    assert_eq!(released, vec![lift(0xE0), lift(0xE7), lift(0x04), lift(0x65)]);
    assert_eq!(kb.release(0).unwrap().count(), 0);
}

/// Every report QEMU 11.1's `usb-kbd` delivered over xHCI for QMP key
/// events, in the boot layout its descriptor shares: `a` down and up,
/// Shift+B, then Q W E R T Y U down, U up, and the rest up. The seventh key
/// is ErrorRollOver in every slot.
#[test]
fn reports_qemu_sent_read_as_the_keys_that_were_pressed() {
    let mut kb = hid(E6_KEYBOARD);
    let captured: &[(&[u8], Vec<Transition>, bool)] = &[
        (&[0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00], vec![press(0x04)], false),
        (&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], vec![lift(0x04)], false),
        (&[0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], vec![press(0xE1)], false),
        (&[0x02, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00], vec![press(0x05)], false),
        (&[0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], vec![lift(0x05)], false),
        (&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], vec![lift(0xE1)], false),
        (&[0x00, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00], vec![press(0x14)], false),
        (&[0x00, 0x00, 0x14, 0x1A, 0x00, 0x00, 0x00, 0x00], vec![press(0x1A)], false),
        (&[0x00, 0x00, 0x14, 0x1A, 0x08, 0x00, 0x00, 0x00], vec![press(0x08)], false),
        (&[0x00, 0x00, 0x14, 0x1A, 0x08, 0x15, 0x00, 0x00], vec![press(0x15)], false),
        (&[0x00, 0x00, 0x14, 0x1A, 0x08, 0x15, 0x17, 0x00], vec![press(0x17)], false),
        (&[0x00, 0x00, 0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C], vec![press(0x1C)], false),
        (&[0x00, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01], vec![], true),
        (&[0x00, 0x00, 0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C], vec![], false),
        (&[0x00, 0x00, 0x1C, 0x1A, 0x08, 0x15, 0x17, 0x00], vec![lift(0x14)], false),
        (&[0x00, 0x00, 0x1C, 0x17, 0x08, 0x15, 0x00, 0x00], vec![lift(0x1A)], false),
        (&[0x00, 0x00, 0x1C, 0x17, 0x15, 0x00, 0x00, 0x00], vec![lift(0x08)], false),
        (&[0x00, 0x00, 0x1C, 0x17, 0x00, 0x00, 0x00, 0x00], vec![lift(0x15)], false),
        (&[0x00, 0x00, 0x1C, 0x00, 0x00, 0x00, 0x00, 0x00], vec![lift(0x17)], false),
        (&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], vec![lift(0x1C)], false),
    ];
    for (at, (report, made, rolled)) in captured.iter().enumerate() {
        assert_eq!(keys(&mut kb, report), Ok((made.clone(), *rolled)), "report {at}: {report:02x?}");
    }
}

/// §6.2.2.5: an array reports indexes. With Logical Minimum 1 against Usage
/// Minimum 0x04, index 1 is `a` (0x04) and index 0x62 is usage 0x65; index 0
/// and index 0x63 are outside the logical range and assert nothing.
#[test]
fn an_array_whose_usage_minimum_is_not_its_logical_minimum_reads_indexes() {
    use item::*;
    let d = spell(&[
        &page(1), &usage(6), &APPLICATION, &page(7), &size(8), &count(2),
        &logical_min(1), &logical_max(0x62), &usage_min(0x04), &usage_max(0x65), &ARRAY, &END,
    ]);
    let mut kb = hid(&d);
    assert_eq!(fed(&mut kb, &[1, 0]), vec![press(0x04)], "index 1 is usage 0x04");
    assert_eq!(fed(&mut kb, &[1, 0x62]), vec![press(0x65)], "index 0x62 is usage 0x65");
    assert_eq!(fed(&mut kb, &[0, 0x63]), vec![lift(0x04), lift(0x65)], "indexes outside the range assert nothing");
    assert_eq!(fed(&mut kb, &[0x04, 0]), vec![press(0x07)], "index 4 is usage 0x07, not 0x04");
}

/// A Keyboard/Keypad variable field is a bitmap of usages: a bit set is that
/// usage held, and cleared released.
#[test]
fn a_bitmap_keyboard_is_bound_and_each_bit_is_its_usage() {
    use item::*;
    // The modifier byte, then one bit for each of 0x04..=0x65 and two bits
    // of padding to a byte.
    let d = spell(&[
        &page(1), &usage(6), &APPLICATION, &page(7), &logical_min(0), &logical_max(1), &size(1),
        &count(8), &usage_min(0xE0), &usage_max(0xE7), &VARIABLE,
        &count(0x62), &usage_min(0x04), &usage_max(0x65), &VARIABLE,
        &count(2), &CONSTANT, &END,
    ]);
    let interface = descriptor::parse(&d).unwrap();
    assert_eq!(interface.collections[0].bound, Ok(Kind::Keyboard));
    let mut kb = Hid::new(interface);
    let bit = |usage: u8| -> Vec<u8> {
        let mut r = vec![0u8; 14];
        let at = 8 + usize::from(usage - 0x04);
        r[at / 8] |= 1 << (at % 8);
        r
    };
    assert_eq!(fed(&mut kb, &bit(0x04)), vec![press(0x04)]);
    assert_eq!(fed(&mut kb, &bit(0x65)), vec![lift(0x04), press(0x65)]);
    let mut shifted = bit(0x1D);
    shifted[0] = 0x02;
    assert_eq!(fed(&mut kb, &shifted), vec![press(0xE1), lift(0x65), press(0x1D)]);
    assert_eq!(fed(&mut kb, &[0; 14]), vec![lift(0xE1), lift(0x1D)]);
}

/// A bitmap from Usage Minimum 0 has controls at `00` to `03`, which are no
/// physical keys (HUT 1.5 p.96): their bits press nothing, and ErrorRollOver
/// never reaches the owner as a press.
#[test]
fn a_bitmaps_controls_at_00_to_03_press_nothing() {
    use item::*;
    let d = spell(&[
        &page(1), &usage(6), &APPLICATION, &page(7), &logical_min(0), &logical_max(1), &size(1),
        &count(8), &usage_min(0x00), &usage_max(0x07), &VARIABLE, &END,
    ]);
    let mut kb = hid(&d);
    assert_eq!(fed(&mut kb, &[0x0F]), vec![]);
    assert_eq!(fed(&mut kb, &[0x1F]), vec![press(0x04)]);
}

/// HUT 1.5 reserves `A5-AF`, `DE-DF` and `E8-FFFF`: each reached by an array's
/// index or a bitmap's bit is counted and not delivered, while the usage
/// beside each range is; an idle array's 00 slots assert nothing and count
/// nothing.
#[test]
fn a_reserved_usage_names_no_key_through_an_array_or_a_bitmap() {
    use item::*;
    let reserved = [0xA5, 0xAF, 0xDE, 0xDF, 0xE8];
    let beside = [0xA4, 0xB0, 0xDD, 0xE7];
    // An array over the whole page, index equal to usage.
    let array = spell(&[
        &page(1), &usage(6), &APPLICATION, &page(7), &size(8), &count(2), &logical_min(0),
        &logical_max(0xFF), &usage_min(0), &usage_max(0xFF), &ARRAY, &END,
    ]);
    for usage in reserved {
        let mut kb = hid(&array);
        assert_eq!(fed(&mut kb, &[usage, 0]), vec![], "{usage:#04x} through an array");
        assert_eq!(kb.counts.reserved, 1, "{usage:#04x}");
    }
    for usage in beside {
        let mut kb = hid(&array);
        assert_eq!(fed(&mut kb, &[usage, 0]), vec![press(usage)], "{usage:#04x} through an array");
        assert_eq!(kb.counts.reserved, 0);
    }
    let mut kb = hid(&array);
    assert_eq!(fed(&mut kb, &[0, 0]), vec![]);
    assert_eq!(kb.counts, crate::report::Counts::default(), "an idle array counts nothing");

    // A bitmap over the whole page: 256 bits.
    let bitmap = spell(&[
        &page(1), &usage(6), &APPLICATION, &page(7), &logical_min(0), &logical_max(1), &size(1),
        &[0x96, 0x00, 0x01], &usage_min(0), &[0x2A, 0xFF, 0x00], &VARIABLE, &END,
    ]);
    let bit = |usage: u8| -> Vec<u8> {
        let mut r = vec![0u8; 32];
        r[usize::from(usage / 8)] |= 1 << (usage % 8);
        r
    };
    for usage in reserved {
        let mut kb = hid(&bitmap);
        assert_eq!(fed(&mut kb, &bit(usage)), vec![], "{usage:#04x} through a bitmap");
        assert_eq!(kb.counts.reserved, 1, "{usage:#04x}");
    }
    for usage in beside {
        let mut kb = hid(&bitmap);
        assert_eq!(fed(&mut kb, &bit(usage)), vec![press(usage)], "{usage:#04x} through a bitmap");
    }
}

/// An array index whose entry is past the field's usage list names no key.
#[test]
fn an_index_past_the_usage_list_is_counted_unlisted() {
    use item::*;
    let d = spell(&[
        &page(1), &usage(6), &APPLICATION, &page(7), &size(8), &count(1), &logical_min(0),
        &logical_max(10), &usage(0x04), &usage(0x05), &ARRAY, &END,
    ]);
    let mut kb = hid(&d);
    assert_eq!(fed(&mut kb, &[1]), vec![press(0x05)]);
    assert_eq!(fed(&mut kb, &[2]), vec![lift(0x05)]);
    assert_eq!(kb.counts.unlisted, 1);
}

/// A keyboard and a Consumer Control collection under two Report IDs: the
/// keyboard's reports are delivered and the other's counted and dropped.
#[test]
fn a_collection_this_reader_cannot_bind_costs_only_its_own_reports() {
    use item::*;
    let d = spell(&[
        &page(1), &usage(6), &APPLICATION, &id(1), &page(7), &size(8), &count(1), &logical_min(0),
        &logical_max(0x65), &usage_min(0), &usage_max(0x65), &ARRAY, &END,
        &page(0x0C), &usage(0x01), &APPLICATION, &id(2), &logical_min(0), &[0x26, 0xFF, 0x03], &size(16),
        &count(1), &usage_min(0), &[0x2A, 0xFF, 0x03], &ARRAY, &END,
    ]);
    let interface = descriptor::parse(&d).unwrap();
    assert_eq!(interface.collections[0].bound, Ok(Kind::Keyboard));
    assert_eq!(interface.collections[1].bound, Err(Unbound::NotAKeyboardOrPointer { page: 0x0C, usage: 0x01 }));
    let mut kb = Hid::new(interface);
    assert_eq!(fed(&mut kb, &[1, 0x04]), vec![press(0x04)]);
    assert_eq!(kb.report(&[2, 0xE9, 0x00]), Ok(Read::Unbound { collection: 1 }));
    assert_eq!(kb.counts.unbound, 1);
    assert_eq!(fed(&mut kb, &[1, 0x00]), vec![lift(0x04)]);
    assert_eq!(kb.report(&[3, 0]), Err(Refused::UnknownReport(3)));
}

/// §8.4: a Report ID in two top-level collections refuses the interface, as
/// binding one half would read the other's fields as absent.
#[test]
fn a_report_id_spanning_two_collections_refuses_the_interface() {
    use item::*;
    let d = spell(&[
        &page(1), &usage(6), &APPLICATION, &id(1), &page(7), &size(8), &count(1), &logical_min(0),
        &logical_max(0x65), &usage_min(0), &usage_max(0x65), &ARRAY, &END,
        &page(1), &usage(2), &APPLICATION, &id(1), &page(1), &usage(0x30), &logical_min(-127),
        &logical_max(127), &size(8), &count(1), &RELATIVE, &END,
    ]);
    assert_eq!(descriptor::parse(&d), Err(Unread::SpansCollections { id: 1 }));
}

/// Button page usages 1 to 8: button 6 pressed is counted and not sent, and
/// button 1 in the same report is.
#[test]
fn a_button_past_the_fifth_is_counted_and_the_rest_delivered() {
    use item::*;
    let d = spell(&[
        &page(1), &usage(2), &APPLICATION, &page(9), &usage_min(1), &usage_max(8), &logical_min(0),
        &logical_max(1), &size(1), &count(8), &VARIABLE, &page(1), &usage(0x30), &usage(0x31),
        &logical_min(-127), &logical_max(127), &size(8), &count(2), &RELATIVE, &END,
    ]);
    let mut mouse = hid(&d);
    let read = pointer(&mut mouse, &[0b0010_0001, 3, 0xFD]);
    assert_eq!(read, Pointer { buttons: 0x01, scroll: 0, motion: Motion::Relative { dx: 3, dy: -3 } });
    assert_eq!(mouse.counts.unsent_button, 1);
}

/// An absolute axis of Logical Minimum −100 to Maximum 100 is scaled onto the
/// record's range, and a value outside it leaves the position as it was.
#[test]
fn an_absolute_axis_is_scaled_onto_the_records_range() {
    use item::*;
    let d = spell(&[
        &page(1), &usage(2), &APPLICATION, &page(1), &usage(0x30), &usage(0x31), &logical_min16(-100),
        &logical_max16(100), &size(16), &count(2), &VARIABLE, &END,
    ]);
    let mut tablet = hid(&d);
    let at = |x: i16, y: i16| -> Vec<u8> { [x.to_le_bytes(), y.to_le_bytes()].concat() };
    let abs = |x, y| Pointer { buttons: 0, scroll: 0, motion: Motion::Absolute { x, y } };
    assert_eq!(pointer(&mut tablet, &at(-100, 100)), abs(0, 32767));
    assert_eq!(pointer(&mut tablet, &at(0, -100)), abs(16383, 0));
    assert_eq!(pointer(&mut tablet, &at(101, 100)), abs(16383, 32767), "101 leaves X as it was");
}

/// The wheel is the Generic Desktop Wheel usage wherever the descriptor puts
/// it: here before the axes.
#[test]
fn the_wheel_is_where_the_descriptor_puts_it() {
    use item::*;
    let d = spell(&[
        &page(1), &usage(2), &APPLICATION, &page(1), &usage(0x38), &usage(0x30), &usage(0x31),
        &logical_min(-127), &logical_max(127), &size(8), &count(3), &RELATIVE, &END,
    ]);
    let mut mouse = hid(&d);
    assert_eq!(pointer(&mut mouse, &[0xFF, 2, 3]), Pointer { buttons: 0, scroll: -1, motion: Motion::Relative { dx: 2, dy: 3 } });
}

/// The boot mouse through its descriptor, with the reports QEMU 11.1's
/// `usb-mouse` delivered over xHCI for QMP moves of (5, −5) and (−200, 200),
/// which QEMU splits at a byte's range, and the left button down and up.
#[test]
fn a_mouse_report_is_read_as_its_descriptor_lays_it_out() {
    let mut mouse = hid(E10_MOUSE);
    let rel = |buttons, dx, dy| Pointer { buttons, scroll: 0, motion: Motion::Relative { dx, dy } };
    assert_eq!(pointer(&mut mouse, &[0x00, 0x05, 0xFB, 0x00]), rel(0, 5, -5));
    assert_eq!(pointer(&mut mouse, &[0x00, 0x81, 0x7F, 0x00]), rel(0, -127, 127));
    assert_eq!(pointer(&mut mouse, &[0x00, 0xB7, 0x49, 0x00]), rel(0, -73, 73));
    assert_eq!(pointer(&mut mouse, &[0x01, 0x00, 0x00, 0x00]), rel(1, 0, 0));
    assert_eq!(pointer(&mut mouse, &[0x00, 0x00, 0x00]), rel(0, 0, 0));
    assert_eq!(pointer(&mut mouse, &[0xFF, 0x01, 0x00]), rel(7, 1, 0), "the padding bits are not buttons");
    assert_eq!(pointer(&mut mouse, &[0x00, 0x80, 0x01]), rel(0, 0, 1), "-128 is outside −127..=127 and moves nothing");
}

#[test]
fn what_breaks_the_reading_refuses_the_interface_by_name() {
    use item::*;
    let cases: &[(Vec<u8>, Unread)] = &[
        (vec![0x05], Unread::Truncated { at: 0 }),
        (vec![0xA1, 0x01, 0x27, 0x00], Unread::Truncated { at: 2 }),
        (vec![0xC0], Unread::Unbalanced { at: 0 }),
        (spell(&[&page(1), &usage(6), &APPLICATION]), Unread::Unbalanced { at: 6 }),
        (spell(&[&size(8), &count(1), &VARIABLE]), Unread::Outside { at: 4 }),
        (spell(&[&id(0)]), Unread::ReportId { at: 0 }),
        (vec![0xB4], Unread::GlobalStack { at: 0 }),
        (vec![0xA4, 0xA4, 0xA4, 0xA4, 0xA4], Unread::GlobalStack { at: 4 }),
        (
            spell(&[&APPLICATION, &size(8), &count(1), &VARIABLE, &id(1), &VARIABLE, &END]),
            Unread::Unnumbered,
        ),
    ];
    for (d, why) in cases {
        assert_eq!(descriptor::parse(d), Err(*why), "{d:02x?}");
    }
    let deep: Vec<u8> = [APPLICATION; 9].concat();
    assert_eq!(descriptor::parse(&deep), Err(Unread::TooDeep { at: 16 }));
}

/// A long item is stepped over: no tag is defined for one (§6.2.2.3).
#[test]
fn a_long_item_is_stepped_over() {
    let mut d = vec![0xFE, 0x02, 0x10, 0xAA, 0xBB];
    d.extend_from_slice(E6_KEYBOARD);
    assert_eq!(descriptor::parse(&d).unwrap().collections[0].bound, Ok(Kind::Keyboard));
    assert_eq!(descriptor::parse(&[0xFE, 0x05, 0x10, 0xAA]), Err(Unread::Truncated { at: 0 }));
}

#[test]
fn a_collection_whose_report_size_or_length_this_reader_cannot_hold_is_not_bound() {
    use item::*;
    let wide = spell(&[&page(1), &usage(6), &APPLICATION, &page(7), &size(33), &count(1), &VARIABLE, &END]);
    assert_eq!(descriptor::parse(&wide).unwrap().collections[0].bound, Err(Unbound::ReportSize { bits: 33 }));
    let long = spell(&[
        &page(1), &usage(6), &APPLICATION, &page(7), &logical_min(0), &logical_max(0x65), &usage_min(0),
        &usage_max(0x65), &size(8), &count(65), &ARRAY, &END,
    ]);
    assert_eq!(descriptor::parse(&long).unwrap().collections[0].bound, Err(Unbound::ReportTooLong { id: 0 }));
    let empty = spell(&[
        &page(1), &usage(2), &APPLICATION, &usage(0x30), &logical_min(0), &logical_max(0), &size(8), &count(1),
        &VARIABLE, &END,
    ]);
    assert_eq!(
        descriptor::parse(&empty).unwrap().collections[0].bound,
        Err(Unbound::EmptyRange { page: 1, usage: 0x30 })
    );
}

/// xorshift64, seeded per test so a failure names the case that made it.
struct Bytes(u64);

impl Bytes {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn byte(&mut self, edges: &[u8]) -> u8 {
        let roll = self.next();
        if roll & 1 == 0 { edges[(roll >> 1) as usize % edges.len()] } else { (roll >> 8) as u8 }
    }
}

/// What an E.6 report reads as, written from Appendices B.1, C and E.6 and not
/// as [`Hid`] does: `None` is a refusal; otherwise the set held after it and
/// whether it rolled over.
fn read_by(report: &[u8], held: &BTreeSet<u8>) -> Option<(BTreeSet<u8>, bool)> {
    let slots = report.get(2..8).filter(|_| report.len() >= 8)?;
    if slots.iter().any(|&u| u == 2 || u == 3) {
        return None;
    }
    let modifiers = (0..8).filter(|n| report[0] & (1 << n) != 0).map(|n| 0xE0 + n);
    if slots.contains(&1) {
        let kept = held.iter().copied().filter(|u| !(0xE0..=0xE7).contains(u));
        return Some((kept.chain(modifiers).collect(), true));
    }
    let named = slots.iter().copied().filter(|&u| (4..=0x65).contains(&u));
    Some((modifiers.chain(named).collect(), false))
}

/// Every report of any length and bytes is believed or refused by name; a
/// believed one moves the keyboard to exactly the set the oracle says, one
/// transition per usage that changed, and a refused one moves nothing.
#[test]
fn every_keyboard_report_is_believed_or_refused_and_nothing_else() {
    const EDGES: &[u8] = &[0, 1, 2, 3, 4, 0x39, 0x65, 0x66, 0xDF, 0xE0, 0xE3, 0xE7, 0xE8, 0xFF];
    let mut bytes = Bytes(0x7573_6268_6964);
    let mut kb = hid(E6_KEYBOARD);
    let mut held = BTreeSet::new();
    let (mut whole, mut rolled, mut refused) = (0, 0, 0);
    for case in 0..200_000 {
        let len = if bytes.next().is_multiple_of(8) { (bytes.next() % 12) as usize } else { 8 };
        let report: Vec<u8> = (0..len).map(|_| bytes.byte(EDGES)).collect();
        let before = kb.clone();
        match (keys(&mut kb, &report), read_by(&report, &held)) {
            (Ok((made, r)), Some((now, rolled_over))) => {
                assert_eq!(r, rolled_over, "case {case}: {report:02x?}");
                if r { rolled += 1 } else { whole += 1 }
                let changed: BTreeSet<u8> = held.symmetric_difference(&now).copied().collect();
                let named: BTreeSet<u8> = made.iter().map(|t| t.usage).collect();
                assert_eq!(named.len(), made.len(), "case {case}: a usage twice in {made:?}");
                assert_eq!(named, changed, "case {case}: {report:02x?}");
                assert!(made.iter().all(|t| t.pressed == now.contains(&t.usage)), "case {case}");
                held = now;
            }
            (Err(why), None) => {
                refused += 1;
                assert_eq!(kb, before, "case {case}: {why:?} moved the keyboard");
            }
            (read, want) => panic!("case {case}: {report:02x?} read as {read:?}, the oracle says {want:?}"),
        }
    }
    assert!(whole > 10_000 && rolled > 10_000 && refused > 10_000, "{whole} whole, {rolled} rolled, {refused} refused");
}

/// Any descriptor bytes are read or refused, and every report through any
/// descriptor read is believed or refused: nothing panics, and a pointer
/// report never leaves the record's range.
#[test]
fn no_descriptor_or_report_panics() {
    // Item prefixes the parser acts on, so random descriptors reach past
    // their first byte.
    const ITEMS: &[u8] = &[
        0x05, 0x09, 0x15, 0x16, 0x19, 0x25, 0x26, 0x29, 0x75, 0x85, 0x95, 0x81, 0x91, 0xA1, 0xC0, 0xA4, 0xB4, 0xFE,
        0x0B, 0x27, 0x17, 0x2A, 0x1A,
    ];
    let mut bytes = Bytes(0x6465_7363_7269);
    let mut read = 0;
    for case in 0..20_000 {
        let mut d: Vec<u8> = Vec::new();
        match case % 3 {
            0 => d.extend_from_slice(E6_KEYBOARD),
            1 => d.extend_from_slice(E10_MOUSE),
            _ => {}
        }
        for _ in 0..(bytes.next() % 6) {
            let at = (bytes.next() as usize) % (d.len() + 1);
            d.insert(at, bytes.byte(ITEMS));
            d.insert(at + 1, bytes.byte(&[0, 1, 2, 6, 7, 8, 0x30, 0x31, 0x38, 0x7F, 0x80, 0xFF]));
        }
        let Ok(interface) = descriptor::parse(&d) else { continue };
        read += 1;
        let mut device = Hid::new(interface);
        for _ in 0..32 {
            let len = (bytes.next() % 12) as usize;
            let report: Vec<u8> = (0..len).map(|_| bytes.byte(&[0, 1, 2, 3, 0x7F, 0x80, 0xFF])).collect();
            if let Ok(Read::Pointer { report: p, .. }) = device.report(&report) {
                assert_eq!(p.buttons & !0x1F, 0, "case {case}");
                if let Motion::Absolute { x, y } = p.motion {
                    assert!(x <= 32767 && y <= 32767, "case {case}: ({x}, {y})");
                }
            }
        }
    }
    assert!(read > 5_000, "{read} descriptors read");
}
