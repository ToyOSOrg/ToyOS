//! What one SCI carried: every event whose status and enable bits are both
//! set, read off the PM1 event block and the GPE0 block, and refused by name
//! where it is one this server never enabled; what a press is whose
//! power-off came back refused ([`unstopped`]); and the wait for the
//! firmware's release of the Global Lock ([`await_release`]).

use std::time::Instant;

use toyos::power::Refused;
use toyos_abi::syscall::SyscallError;

/// PM1 status and enable (ACPI 6.5 Tables 4.13, 4.14): the power button.
pub const PWRBTN: u16 = 1 << 8;

/// PM1 status and enable (Tables 4.13, 4.14): `GBL_STS`, which the firmware
/// sets when it gives back a Global Lock it found its pending bit set in
/// (§5.2.10.1), and `GBL_EN`, which lets that raise the SCI.
pub const GBL: u16 = 1 << 5;

/// PM1 status bits a write of one clears (Table 4.13): timer, bus master,
/// global lock release, power button, sleep button, RTC, PCIe wake and wake.
pub const PM1_STATUS: u16 = 1 << 0 | 1 << 4 | 1 << 5 | 1 << 8 | 1 << 9 | 1 << 10 | 1 << 14 | 1 << 15;

/// One event an SCI carried.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    PowerButton,
    /// The embedded controller's GPE.
    Ec,
}

/// What this server enabled, and so serves.
pub struct Served {
    pub power_button: bool,
    pub ec_gpe: Option<u16>,
}

/// An event enabled and set that this server never enabled: the firmware, or
/// another writer, enabled it behind the server's back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unserved {
    Pm1 { status: u16, enable: u16 },
    Gpe(u16),
}

/// The events of one SCI, from the PM1 status and enable registers and each
/// GPE0 status byte with its enable byte.
pub fn events(served: &Served, pm1: (u16, u16), gpe0: &[(u8, u8)]) -> Result<Vec<Event>, Unserved> {
    let mut events = Vec::new();
    let (status, enable) = pm1;
    let fired = status & enable;
    if fired & !(if served.power_button { PWRBTN } else { 0 }) != 0 {
        return Err(Unserved::Pm1 { status, enable });
    }
    if fired & PWRBTN != 0 {
        events.push(Event::PowerButton);
    }
    for (byte, &(status, enable)) in gpe0.iter().enumerate() {
        let fired = status & enable;
        for bit in (0..8).filter(|bit| fired & 1 << bit != 0) {
            let n = (byte * 8 + bit) as u16;
            events.push(match n {
                n if served.ec_gpe == Some(n) => Event::Ec,
                n => return Err(Unserved::Gpe(n)),
            });
        }
    }
    Ok(events)
}

/// Every enable this server sets: PM1's and each GPE0 byte's.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Enables {
    pub pm1: u16,
    pub gpe0: Vec<u8>,
}

/// The fixed hardware and the SCI, as the wait for the firmware's release
/// drives them.
pub trait Fixed {
    fn pm1_status(&mut self) -> u16;
    /// Write `bits` to PM1 status, which clears each one set (Table 4.13).
    fn pm1_clear(&mut self, bits: u16);
    fn enables(&mut self) -> Enables;
    fn enable(&mut self, enables: &Enables);
    /// Waits for the SCI until `until` and takes its record: `false` is the
    /// deadline, and a wake with no record is `true`.
    fn sci(&mut self, until: Instant) -> bool;
    /// Unmasks the SCI.
    fn ack(&mut self);
}

/// Waits until `until` for `GBL_STS`, after a take of the Global Lock left
/// the firmware the pending bit (§5.2.10.1): `true` where it was set, and is
/// now clear. While it waits `GBL_EN` is the one enable set, so no other
/// event raises the SCI it waits on, whose line is level; each enable is put
/// back as it was, so an event latched meanwhile raises the SCI again for
/// whoever serves it. A `GBL_STS` left from an earlier release ends the wait
/// too, which costs the caller one take more and loses no release.
pub fn await_release(fixed: &mut impl Fixed, until: Instant) -> bool {
    let kept = fixed.enables();
    fixed.enable(&Enables { pm1: GBL, gpe0: vec![0; kept.gpe0.len()] });
    let released = loop {
        let set = fixed.pm1_status() & GBL != 0;
        if set {
            fixed.pm1_clear(GBL);
        }
        // Acknowledged with nothing but `GBL_STS` enabled, and that clear.
        fixed.ack();
        if set {
            break true;
        }
        if !fixed.sci(until) {
            break false;
        }
    };
    fixed.enable(&kept);
    released
}

/// SCIs in a row that carried no event this server serves, past which the
/// line is a storm and not a machine with something to say.
pub const EMPTY_SCIS: u32 = 64;

/// What a press is whose power-off came back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unstopped {
    /// The machine has no power-off: the press is said and dropped.
    Dropped,
    /// This server's defect, or the supervisor's.
    Defect,
}

/// A press whose power-off was `refused`, on a machine the kernel was handed
/// `\_S5`'s sleep type on (`power_off`) or was not. Only the kernel's own
/// word for a machine without one drops the press, and only where this server
/// handed it none: every other refusal is of a stop that should have
/// happened.
pub fn unstopped(refused: Refused, power_off: bool) -> Unstopped {
    match refused {
        Refused::Kernel(SyscallError::NotSupported) if !power_off => Unstopped::Dropped,
        Refused::Kernel(_) | Refused::NotEndowed | Refused::Unanswered => Unstopped::Defect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn served() -> Served {
        Served { power_button: true, ec_gpe: Some(0x6e) }
    }

    #[test]
    fn a_press_and_the_controllers_gpe_are_both_read_off_one_sci() {
        let mut gpe0 = [(0u8, 0u8); 16];
        gpe0[0x6e / 8] = (1 << (0x6e % 8), 1 << (0x6e % 8));
        assert_eq!(
            events(&served(), (PWRBTN | 1, PWRBTN), &gpe0),
            Ok(vec![Event::PowerButton, Event::Ec]),
            "a status bit whose enable is clear (the timer's) is no event"
        );
    }

    #[test]
    fn a_status_set_with_its_enable_clear_is_no_event() {
        let gpe0 = [(0xFF, 0x00); 16];
        assert_eq!(events(&served(), (0xFFFF, 0), &gpe0), Ok(vec![]));
    }

    #[test]
    fn an_enabled_event_outside_the_served_set_is_refused_by_name() {
        let mut gpe0 = [(0u8, 0u8); 16];
        gpe0[13] = (0x80, 0x80);
        assert_eq!(events(&served(), (0, 0), &gpe0), Err(Unserved::Gpe(13 * 8 + 7)));
        let sleep_button = 1 << 9;
        assert_eq!(
            events(&served(), (sleep_button, sleep_button | PWRBTN), &[]),
            Err(Unserved::Pm1 { status: sleep_button, enable: sleep_button | PWRBTN })
        );
        let no_button = Served { power_button: false, ..served() };
        assert_eq!(
            events(&no_button, (PWRBTN, PWRBTN), &[]),
            Err(Unserved::Pm1 { status: PWRBTN, enable: PWRBTN }),
            "a power button that is not the fixed one is never enabled here"
        );
    }

    /// What the firmware or a device sets, at one wait for the SCI.
    #[derive(Clone, Copy)]
    enum Set {
        Pm1(u16),
        /// A GPE0 status byte, and its bits.
        Gpe(usize, u8),
    }

    /// A PM1 event block and a GPE0 block, whose status bits a write of one
    /// clears and whose SCI is level: asserted while any status bit and its
    /// enable are both set (§4.8.3.1), so an acknowledgement then is one the
    /// line answers at once. A wait for the SCI lets the script set one thing
    /// after another until the line is raised; past the script the wait
    /// meets its deadline.
    struct Block {
        status: u16,
        gpe0: Vec<u8>,
        enables: Enables,
        script: Vec<Set>,
        sets: usize,
        acks: usize,
    }

    impl Block {
        fn new(script: &[Set]) -> Self {
            Block {
                status: 0,
                gpe0: vec![0; 3],
                enables: Enables { pm1: PWRBTN, gpe0: vec![0x40, 0, 0x01] },
                script: script.to_vec(),
                sets: 0,
                acks: 0,
            }
        }
    }

    impl Fixed for Block {
        fn pm1_status(&mut self) -> u16 {
            self.status
        }
        fn pm1_clear(&mut self, bits: u16) {
            self.status &= !bits;
        }
        fn enables(&mut self) -> Enables {
            self.enables.clone()
        }
        fn enable(&mut self, enables: &Enables) {
            self.enables = enables.clone();
        }
        fn sci(&mut self, _: Instant) -> bool {
            while let Some(&set) = self.script.get(self.sets) {
                match set {
                    Set::Pm1(bits) => self.status |= bits,
                    Set::Gpe(byte, bits) => self.gpe0[byte] |= bits,
                }
                self.sets += 1;
                if self.raised() {
                    return true;
                }
            }
            false
        }
        fn ack(&mut self) {
            self.acks += 1;
            assert!(!self.raised(), "acknowledged with PM1 status {:#06x} or GPE0 status {:x?} raising the line", self.status, self.gpe0);
        }
    }

    impl Block {
        fn raised(&self) -> bool {
            self.status & self.enables.pm1 != 0 || self.gpe0.iter().zip(&self.enables.gpe0).any(|(status, enable)| status & enable != 0)
        }
    }

    #[test]
    fn the_wait_ends_on_the_firmwares_release_and_leaves_every_other_event_latched_and_enabled() {
        // A press and the controller's GPE come while the firmware holds the
        // lock; then it lets go.
        let mut block = Block::new(&[Set::Pm1(PWRBTN), Set::Gpe(0, 0x40), Set::Pm1(GBL)]);
        let kept = block.enables.clone();
        assert!(await_release(&mut block, Instant::now()));
        assert_eq!(block.status, PWRBTN, "GBL_STS left set, or an event the wait does not serve cleared");
        assert_eq!(block.gpe0, [0x40, 0, 0], "the controller's GPE cleared by the wait");
        assert_eq!(block.enables, kept, "an enable was not put back");
        assert_eq!((block.sets, block.acks), (3, 2), "the wait woke for an event it had disabled");
    }

    #[test]
    fn a_release_already_latched_ends_the_wait_without_one() {
        let mut block = Block::new(&[]);
        block.status = GBL;
        assert!(await_release(&mut block, Instant::now()));
        assert_eq!((block.status, block.sets, block.acks), (0, 0, 1));
    }

    #[test]
    fn a_firmware_that_never_lets_go_meets_the_deadline_with_its_enables_back() {
        let mut block = Block::new(&[Set::Pm1(PWRBTN)]);
        let kept = block.enables.clone();
        assert!(!await_release(&mut block, Instant::now()));
        assert_eq!(block.enables, kept);
        assert_eq!(block.status, PWRBTN);
    }

    #[test]
    fn only_a_press_on_a_machine_handed_no_sleep_type_is_dropped() {
        let no_sleep_type = Refused::Kernel(SyscallError::NotSupported);
        assert_eq!(unstopped(no_sleep_type, false), Unstopped::Dropped);
        assert_eq!(unstopped(no_sleep_type, true), Unstopped::Defect, "the kernel lost the sleep type this server handed it");
        for refused in [
            Refused::Kernel(SyscallError::PermissionDenied),
            Refused::Kernel(SyscallError::Gone),
            Refused::Kernel(SyscallError::InvalidArgument),
            Refused::NotEndowed,
            Refused::Unanswered,
        ] {
            for power_off in [false, true] {
                assert_eq!(unstopped(refused, power_off), Unstopped::Defect, "{refused:?} with power_off {power_off}");
            }
        }
    }
}
