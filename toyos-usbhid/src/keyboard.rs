//! The boot keyboard report (HID 1.11 Appendix B.1), as the set of keys it
//! holds.
//!
//! ```text
//! byte 0     modifier keys as bits: bit n is usage 0xE0 + n (LeftControl .. RightGUI)
//! byte 1     reserved for the maker, read by nobody
//! bytes 2-7  Keyboard/Keypad page usages held, 0 in a slot holding none
//! ```
//!
//! A report is everything held, so a transition is the difference between two
//! reports of **one** keyboard: diffing against another's would release keys
//! still down. A usage held twice — in two slots, or as a bit and in a slot —
//! is held once.
//!
//! Usages 1 to 3 are not keys but the keyboard saying it cannot say which keys
//! are down (HID Usage Tables, Keyboard/Keypad page): ErrorRollOver in every
//! slot is Appendix C's phantom state, which a keyboard reports when more keys
//! are down than it has slots. Read as keys, it is six releases of keys still
//! held, so such a report is refused and the last good one stands.

/// Bytes in a boot keyboard report, and what its interrupt transfer asks for.
pub const REPORT: usize = 8;

/// The most transitions one report can make: the eight modifiers, and six
/// slots released and six pressed.
pub const MOST: usize = 20;

const ERROR_ROLL_OVER: u8 = 0x01;
const POST_FAIL: u8 = 0x02;
const ERROR_UNDEFINED: u8 = 0x03;

/// The first modifier's usage; byte 0's bit n is `MODIFIER + n`.
const MODIFIER: u8 = 0xE0;

/// One key went down or up, named by its Keyboard/Keypad page usage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transition {
    pub usage: u8,
    pub pressed: bool,
}

/// Why a report was not believed; the keyboard's state is the last report
/// that was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// Not the [`REPORT`] bytes of a boot report.
    Length(usize),
    /// ErrorRollOver in a slot: more keys are down than the report can name.
    RollOver,
    /// POSTFail in a slot: the keyboard failed its own power-on test.
    PostFail,
    /// ErrorUndefined in a slot: an error the keyboard does not name.
    Undefined,
}

/// Every usage, one bit each.
type Usages = [u64; 4];

fn bit(usage: u8) -> (usize, u64) {
    (usize::from(usage / 64), 1 << (usage % 64))
}

/// The eight modifiers' bits, which sit in the last word.
const MODIFIERS: Usages = [0, 0, 0, 0xFF << (MODIFIER % 64)];

/// One boot keyboard: the keys its last believed report held.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Keyboard {
    held: Usages,
}

impl Keyboard {
    /// A keyboard holding nothing, as one is when it binds.
    pub const fn new() -> Self {
        Self { held: [0; 4] }
    }

    /// The transitions `report` makes from the last report believed, or why it
    /// is not believed.
    pub fn report(&mut self, report: &[u8]) -> Result<Transitions, Refused> {
        let report: &[u8; REPORT] = report.try_into().map_err(|_| Refused::Length(report.len()))?;
        let mut now = [0u64; 4];
        for n in 0..8u8 {
            if report[0] & (1 << n) != 0 {
                let (word, mask) = bit(MODIFIER + n);
                now[word] |= mask;
            }
        }
        for &usage in &report[2..] {
            match usage {
                0 => {}
                ERROR_ROLL_OVER => return Err(Refused::RollOver),
                POST_FAIL => return Err(Refused::PostFail),
                ERROR_UNDEFINED => return Err(Refused::Undefined),
                usage => {
                    let (word, mask) = bit(usage);
                    now[word] |= mask;
                }
            }
        }
        Ok(self.hold(now))
    }

    /// Every held key released: a keyboard leaving the bus says nothing more.
    pub fn release(&mut self) -> Transitions {
        self.hold([0; 4])
    }

    fn hold(&mut self, now: Usages) -> Transitions {
        let changed = core::array::from_fn(|w| self.held[w] ^ now[w]);
        self.held = now;
        Transitions { changed, now, pass: Pass::Modifiers }
    }
}

/// Which transitions [`Transitions`] is yielding. **Modifiers first**, so a
/// report pressing Shift and a key together is a shifted key and one
/// pressing Ctrl, Alt and D together is the chord; then releases, then
/// presses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pass {
    Modifiers,
    Released,
    Pressed,
}

/// The transitions one report made, at most [`MOST`], each usage at most
/// once and in ascending usage within each pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transitions {
    changed: Usages,
    now: Usages,
    pass: Pass,
}

impl Iterator for Transitions {
    type Item = Transition;

    fn next(&mut self) -> Option<Transition> {
        loop {
            // The modifiers' pass has cleared their bits before the others run.
            let wanted = |w: usize| match self.pass {
                Pass::Modifiers => self.changed[w] & MODIFIERS[w],
                Pass::Released => self.changed[w] & !self.now[w],
                Pass::Pressed => self.changed[w] & self.now[w],
            };
            if let Some(w) = (0..4).find(|&w| wanted(w) != 0) {
                let at = wanted(w).trailing_zeros();
                self.changed[w] &= !(1 << at);
                // `w < 4` and `at < 64`, so the usage is a byte.
                let usage = (w as u32 * 64 + at) as u8;
                return Some(Transition { usage, pressed: self.now[w] & (1 << at) != 0 });
            }
            self.pass = match self.pass {
                Pass::Modifiers => Pass::Released,
                Pass::Released => Pass::Pressed,
                Pass::Pressed => return None,
            };
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn press(usage: u8) -> Transition {
        Transition { usage, pressed: true }
    }

    fn lift(usage: u8) -> Transition {
        Transition { usage, pressed: false }
    }

    fn fed(keyboard: &mut Keyboard, report: &[u8]) -> Result<Vec<Transition>, Refused> {
        keyboard.report(report).map(Iterator::collect)
    }

    /// Appendix B.1 byte for byte: bit n of byte 0 is usage 0xE0 + n, byte 1
    /// is nobody's, and a slot names its usage.
    #[test]
    fn a_report_is_read_as_appendix_b_lays_it_out() {
        for n in 0..8u8 {
            let mut keyboard = Keyboard::new();
            assert_eq!(fed(&mut keyboard, &[1 << n, 0, 0, 0, 0, 0, 0, 0]), Ok(std::vec![press(0xE0 + n)]));
        }
        let mut keyboard = Keyboard::new();
        assert_eq!(fed(&mut keyboard, &[0, 0xFF, 0, 0, 0, 0, 0, 0]), Ok(std::vec![]), "byte 1 is reserved");
        for slot in 2..REPORT {
            let mut keyboard = Keyboard::new();
            let mut report = [0u8; REPORT];
            report[slot] = 0x04;
            assert_eq!(fed(&mut keyboard, &report), Ok(std::vec![press(0x04)]), "slot {slot}");
        }
    }

    /// The order a reader of the stream depends on: Shift before the key it
    /// shifts, and a release before the press that took its slot.
    #[test]
    fn modifiers_come_first_then_releases_then_presses() {
        let mut keyboard = Keyboard::new();
        assert_eq!(fed(&mut keyboard, &[0x02, 0, 0x05, 0, 0, 0, 0, 0]), Ok(std::vec![press(0xE1), press(0x05)]));
        assert_eq!(
            fed(&mut keyboard, &[0x00, 0, 0x06, 0, 0, 0, 0, 0]),
            Ok(std::vec![lift(0xE1), lift(0x05), press(0x06)])
        );
        assert_eq!(
            fed(&mut keyboard, &[0x05, 0, 0x07, 0, 0, 0, 0, 0]),
            Ok(std::vec![press(0xE0), press(0xE2), lift(0x06), press(0x07)]),
            "Ctrl+Alt+D in one report is the chord"
        );
        assert_eq!(fed(&mut keyboard, &[0x05, 0, 0x07, 0, 0, 0, 0, 0]), Ok(std::vec![]), "nothing changed");
    }

    /// A key that moves slot is not a release and a press; a usage held twice
    /// is held once and released once.
    #[test]
    fn a_report_is_a_set_of_keys_and_not_a_list_of_slots() {
        let mut keyboard = Keyboard::new();
        assert_eq!(fed(&mut keyboard, &[0, 0, 0x04, 0x05, 0, 0, 0, 0]), Ok(std::vec![press(0x04), press(0x05)]));
        assert_eq!(fed(&mut keyboard, &[0, 0, 0x05, 0x04, 0, 0, 0, 0]), Ok(std::vec![]));
        assert_eq!(fed(&mut keyboard, &[0, 0, 0x05, 0x05, 0x04, 0x05, 0, 0]), Ok(std::vec![]));
        assert_eq!(fed(&mut keyboard, &[0x01, 0, 0xE0, 0, 0, 0, 0, 0]), Ok(std::vec![press(0xE0), lift(0x04), lift(0x05)]));
        assert_eq!(fed(&mut keyboard, &[0x01, 0, 0, 0, 0, 0, 0, 0]), Ok(std::vec![]), "still held by its bit");
        assert_eq!(fed(&mut keyboard, &[0; REPORT]), Ok(std::vec![lift(0xE0)]));
    }

    /// Appendix C's phantom state, and the other two error usages, anywhere
    /// in the six slots: refused, and the keys held before are still held.
    #[test]
    fn an_error_usage_is_refused_and_the_keys_held_stay_held() {
        for (usage, why) in [(1, Refused::RollOver), (2, Refused::PostFail), (3, Refused::Undefined)] {
            for slot in 2..REPORT {
                let mut keyboard = Keyboard::new();
                fed(&mut keyboard, &[0x02, 0, 0x04, 0x05, 0, 0, 0, 0]).expect("a good report");
                let before = keyboard;
                let mut report = [0x00, 0, 0x04, 0, 0, 0, 0, 0];
                report[slot] = usage;
                assert_eq!(fed(&mut keyboard, &report), Err(why), "usage {usage} in slot {slot}");
                assert_eq!(keyboard, before, "a refused report changed what is held");
            }
        }
        let mut keyboard = Keyboard::new();
        fed(&mut keyboard, &[0x02, 0, 0x04, 0x05, 0, 0, 0, 0]).expect("a good report");
        assert_eq!(fed(&mut keyboard, &[0x02, 0, 1, 1, 1, 1, 1, 1]), Err(Refused::RollOver));
        assert_eq!(
            fed(&mut keyboard, &[0x00, 0, 0x05, 0, 0, 0, 0, 0]),
            Ok(std::vec![lift(0xE1), lift(0x04)]),
            "the next good report is diffed against the last good one"
        );
    }

    #[test]
    fn a_report_of_any_other_length_is_refused() {
        for len in (0..REPORT).chain(REPORT + 1..=64) {
            let mut keyboard = Keyboard::new();
            fed(&mut keyboard, &[0, 0, 0x04, 0, 0, 0, 0, 0]).expect("a good report");
            let before = keyboard;
            let report = std::vec![0x02; len];
            assert_eq!(fed(&mut keyboard, &report), Err(Refused::Length(len)));
            assert_eq!(keyboard, before);
        }
    }

    #[test]
    fn a_keyboard_leaving_releases_everything_it_held_and_nothing_else() {
        let mut keyboard = Keyboard::new();
        fed(&mut keyboard, &[0x81, 0xFF, 0x04, 0xFF, 0x65, 0, 0, 0]).expect("a good report");
        let released: Vec<_> = keyboard.release().collect();
        assert_eq!(released, std::vec![lift(0xE0), lift(0xE7), lift(0x04), lift(0x65), lift(0xFF)]);
        assert_eq!(keyboard.release().count(), 0);
    }

    /// Every report QEMU 11.1's `usb-kbd` delivered to the kernel's driver
    /// over xHCI, each transfer whole, for QMP key events: `a` down and up,
    /// Shift+B, then Q W E R T Y U down, U up, and the rest up. The seventh key
    /// is ErrorRollOver in every slot, and reading it as keys would release
    /// the six still held and press them again on the next report.
    #[test]
    fn reports_qemu_sent_read_as_the_keys_that_were_pressed() {
        let mut keyboard = Keyboard::new();
        type Case<'a> = (&'a [u8], Result<&'a [Transition], Refused>);
        let captured: &[Case<'_>] = &[
            (&[0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00], Ok(&[press(0x04)])),
            (&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], Ok(&[lift(0x04)])),
            (&[0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], Ok(&[press(0xE1)])),
            (&[0x02, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00], Ok(&[press(0x05)])),
            (&[0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], Ok(&[lift(0x05)])),
            (&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], Ok(&[lift(0xE1)])),
            (&[0x00, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00], Ok(&[press(0x14)])),
            (&[0x00, 0x00, 0x14, 0x1A, 0x00, 0x00, 0x00, 0x00], Ok(&[press(0x1A)])),
            (&[0x00, 0x00, 0x14, 0x1A, 0x08, 0x00, 0x00, 0x00], Ok(&[press(0x08)])),
            (&[0x00, 0x00, 0x14, 0x1A, 0x08, 0x15, 0x00, 0x00], Ok(&[press(0x15)])),
            (&[0x00, 0x00, 0x14, 0x1A, 0x08, 0x15, 0x17, 0x00], Ok(&[press(0x17)])),
            (&[0x00, 0x00, 0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C], Ok(&[press(0x1C)])),
            (&[0x00, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01], Err(Refused::RollOver)),
            (&[0x00, 0x00, 0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C], Ok(&[])),
            (&[0x00, 0x00, 0x1C, 0x1A, 0x08, 0x15, 0x17, 0x00], Ok(&[lift(0x14)])),
            (&[0x00, 0x00, 0x1C, 0x17, 0x08, 0x15, 0x00, 0x00], Ok(&[lift(0x1A)])),
            (&[0x00, 0x00, 0x1C, 0x17, 0x15, 0x00, 0x00, 0x00], Ok(&[lift(0x08)])),
            (&[0x00, 0x00, 0x1C, 0x17, 0x00, 0x00, 0x00, 0x00], Ok(&[lift(0x15)])),
            (&[0x00, 0x00, 0x1C, 0x00, 0x00, 0x00, 0x00, 0x00], Ok(&[lift(0x17)])),
            (&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], Ok(&[lift(0x1C)])),
        ];
        for (at, (report, want)) in captured.iter().enumerate() {
            assert_eq!(fed(&mut keyboard, report), want.map(<[Transition]>::to_vec), "report {at}: {report:02x?}");
        }
    }

    /// A small generator with no dependency: xorshift64, seeded per test so a
    /// failure names the case that made it.
    pub(crate) struct Bytes(u64);

    impl Bytes {
        pub(crate) fn new(seed: u64) -> Self {
            Self(seed | 1)
        }

        pub(crate) fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// A byte drawn half the time from `edges` and otherwise from any.
        pub(crate) fn byte(&mut self, edges: &[u8]) -> u8 {
            let roll = self.next();
            if roll & 1 == 0 {
                edges[(roll >> 1) as usize % edges.len()]
            } else {
                (roll >> 8) as u8
            }
        }
    }

    /// What a report holds, written as a reader of Appendix B.1 would and not
    /// as [`Keyboard::report`] does: the oracle the fuzz holds it to.
    fn held_by(report: &[u8]) -> std::collections::BTreeSet<u8> {
        let mut held: std::collections::BTreeSet<u8> =
            (0..8).filter(|n| report[0] & (1 << n) != 0).map(|n| 0xE0 + n).collect();
        held.extend(report[2..].iter().copied().filter(|&u| u != 0));
        held
    }

    /// Every report, of any length and any bytes, is believed or refused by
    /// name; a believed one moves the keyboard to exactly the set it holds,
    /// one transition per usage that changed, and a refused one moves nothing.
    #[test]
    fn every_report_is_believed_or_refused_and_nothing_else() {
        const EDGES: &[u8] = &[0, 1, 2, 3, 4, 0x39, 0x65, 0x66, 0xDF, 0xE0, 0xE3, 0xE7, 0xE8, 0xFF];
        let mut bytes = Bytes::new(0x7573_6268_6964);
        let mut keyboard = Keyboard::new();
        let mut held = std::collections::BTreeSet::new();
        let (mut believed, mut refused) = (0, 0);
        for case in 0..200_000 {
            let len = if bytes.next().is_multiple_of(8) { (bytes.next() % 12) as usize } else { REPORT };
            let report: Vec<u8> = (0..len).map(|_| bytes.byte(EDGES)).collect();
            let before = keyboard;
            match keyboard.report(&report) {
                Ok(transitions) => {
                    believed += 1;
                    let now = held_by(&report);
                    let made: Vec<Transition> = transitions.collect();
                    assert!(made.len() <= MOST, "case {case}: {} transitions", made.len());
                    let changed: std::collections::BTreeSet<u8> = held.symmetric_difference(&now).copied().collect();
                    let named: std::collections::BTreeSet<u8> = made.iter().map(|t| t.usage).collect();
                    assert_eq!(named.len(), made.len(), "case {case}: a usage twice in {made:?}");
                    assert_eq!(named, changed, "case {case}: {report:02x?}");
                    assert!(made.iter().all(|t| t.pressed == now.contains(&t.usage)), "case {case}");
                    held = now;
                }
                Err(why) => {
                    refused += 1;
                    assert_eq!(keyboard, before, "case {case}: {why:?} moved the keyboard");
                    let error = report.get(2..).is_some_and(|slots| slots.iter().any(|u| (1..=3).contains(u)));
                    assert!(report.len() != REPORT || error, "case {case}: {why:?} of {report:02x?}");
                }
            }
        }
        assert!(believed > 10_000 && refused > 10_000, "{believed} believed, {refused} refused");
    }
}
