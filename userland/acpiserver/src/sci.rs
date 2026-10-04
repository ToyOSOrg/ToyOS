//! What one SCI carried: every event whose status and enable bits are both
//! set, read off the PM1 event block and the GPE0 block, and refused by name
//! where it is one this server never enabled.

/// PM1 status and enable (ACPI 6.5 Tables 4.13, 4.14): the power button.
pub const PWRBTN: u16 = 1 << 8;

/// PM1 status bits a write of one clears (Table 4.13): timer, bus master,
/// global lock release, power button, sleep button, RTC, PCIe wake and wake.
pub const PM1_STATUS: u16 = 1 << 0 | 1 << 4 | 1 << 5 | 1 << 8 | 1 << 9 | 1 << 10 | 1 << 14 | 1 << 15;

/// One event an SCI carried.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    PowerButton,
    /// The embedded controller's GPE.
    Ec,
    /// A GPE the namespace runs.
    Runtime(u16),
}

/// What this server enabled, and so serves.
pub struct Served {
    pub power_button: bool,
    pub ec_gpe: Option<u16>,
    pub runtime: Vec<u16>,
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
                n if served.runtime.contains(&n) => Event::Runtime(n),
                n => return Err(Unserved::Gpe(n)),
            });
        }
    }
    Ok(events)
}

/// SCIs in a row that carried no event this server serves, past which the
/// line is a storm and not a machine with something to say.
pub const EMPTY_SCIS: u32 = 64;

#[cfg(test)]
mod tests {
    use super::*;

    fn served() -> Served {
        Served { power_button: true, ec_gpe: Some(0x6e), runtime: vec![0x09] }
    }

    #[test]
    fn a_press_and_the_controllers_gpe_are_both_read_off_one_sci() {
        let mut gpe0 = [(0u8, 0u8); 16];
        gpe0[0x6e / 8] = (1 << (0x6e % 8), 1 << (0x6e % 8));
        gpe0[1] = (0b10, 0b10);
        assert_eq!(
            events(&served(), (PWRBTN | 1, PWRBTN), &gpe0),
            Ok(vec![Event::PowerButton, Event::Runtime(0x09), Event::Ec]),
            "a status bit whose enable is clear (the timer's) is no event"
        );
    }

    #[test]
    fn a_status_set_with_its_enable_clear_is_no_event() {
        let gpe0 = [(0xFF, 0x00); 16];
        assert_eq!(events(&served(), (0xFFFF, 0), &gpe0), Ok(vec![]));
    }

    /// Fix 2 of the design's roast: an enabled bit nothing here enabled is a
    /// writer behind this server's back, said by its number.
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
}
