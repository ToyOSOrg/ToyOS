//! `toyos_tco::fch`'s sequences against a model of the FCH built from the
//! register descriptions it cites (55570 §9.2.6, PMx000, PMx0C0; 58264
//! §12.3.6), so a sequence the hardware would ignore fails here.

use toyos_tco::fch::{
    self, AcpiMmio, Control, Refused, Register, ResetStat, Unsurveyed, Window, TIMER,
    WATCHDOG_ISSUE_RESET,
};

/// `PMx000`'s reset value on Family 17h (55570): `WatchdogTmrEn` clear, the
/// 1 s tick.
const DECODE_EN_RESET: u32 = 0x2302_0B10;

/// One FCH, in microseconds.
struct Model {
    decode_en: u32,
    /// Firmware that holds `WatchdogTmrEn` clear.
    decode_locked: bool,
    /// Firmware that clears `WatchdogTmrEn` at every reset, as the AMD
    /// bench's was read to.
    decode_clears_on_reset: bool,
    /// An FCH that ignores a write of `PMx000`'s byte 3.
    tick_locked: bool,
    alternate: u8,
    select: u8,
    reset_stat: u32,
    run: bool,
    fired: bool,
    power_off: bool,
    /// Family 1Ah's `watchdogactionen`; `None` on 17h, where bit 4 is reserved.
    action_en: Option<bool>,
    reload: u16,
    counter: u16,
    /// Whether a count interval is running: only `WatchdogTrigger` starts one,
    /// and only while the timer runs.
    counting: bool,
    now_us: u64,
    resets: u32,
    writes: u32,
}

impl Model {
    fn new() -> Self {
        Self {
            decode_en: DECODE_EN_RESET,
            decode_locked: false,
            decode_clears_on_reset: false,
            tick_locked: false,
            alternate: 0,
            select: 0,
            reset_stat: 0,
            run: false,
            fired: false,
            power_off: false,
            action_en: None,
            reload: 0x5555,
            counter: 0x5555,
            counting: false,
            now_us: 0,
            resets: 0,
            writes: 0,
        }
    }

    fn enabled(&self) -> bool {
        self.decode_en & fch::WATCHDOG_TMR_EN != 0
    }

    fn tick_us(&self) -> u64 {
        match (self.decode_en >> 24) & 3 {
            0 => 32,
            1 => 10_000,
            2 => 100_000,
            _ => 1_000_000,
        }
    }

    /// Time passes; the tick is free-running, so the first one after a
    /// trigger comes anywhere inside its period.
    fn advance_ms(&mut self, ms: u64) {
        let end = self.now_us + ms * 1_000;
        while self.now_us < end {
            let tick = self.tick_us();
            let next = (self.now_us / tick + 1) * tick;
            if next > end {
                self.now_us = end;
                break;
            }
            self.now_us = next;
            if self.enabled() && self.run && self.counting {
                self.counter = self.counter.saturating_sub(1);
                if self.counter == 0 {
                    self.expire();
                }
            }
        }
    }

    fn expire(&mut self) {
        self.counting = false;
        self.fired = true;
        if self.action_en == Some(false) {
            return;
        }
        // A reset of type 1: the WDT block is in the S5 domain and keeps its
        // registers; the reset's own bit is the one `PMx0C0` keeps.
        self.reset_stat = self.reset_stat & !0x0FFF_0000 | WATCHDOG_ISSUE_RESET;
        self.reset();
    }

    /// A CF9 reset, which the S5-domain block also survives.
    fn cf9(&mut self) {
        self.reset_stat = self.reset_stat & !0x0FFF_0000 | 1 << 19;
        self.reset();
    }

    fn reset(&mut self) {
        self.resets += 1;
        if self.decode_clears_on_reset {
            self.decode_en &= !fch::WATCHDOG_TMR_EN;
        }
    }

    fn control(&self) -> u32 {
        if !self.enabled() {
            return Control::DISABLED;
        }
        u32::from(self.run)
            | u32::from(self.fired) << 1
            | u32::from(self.power_off) << 2
            | u32::from(self.action_en == Some(true)) << 4
    }
}

impl AcpiMmio for Model {
    fn read_u8(&mut self, offset: u16) -> u8 {
        match offset {
            0x300..=0x303 => (self.decode_en >> ((offset - 0x300) * 8)) as u8,
            0x3C4 => self.select,
            0x3D5 => self.alternate,
            _ => panic!("a byte read at ACPI-MMIO {offset:#x}, which the model does not hold"),
        }
    }

    fn write_u8(&mut self, offset: u16, value: u8) {
        self.writes += 1;
        match offset {
            0x300 => {
                let value = if self.decode_locked { value & !(fch::WATCHDOG_TMR_EN as u8) } else { value };
                self.decode_en = self.decode_en & !0xff | u32::from(value);
            }
            0x303 if self.tick_locked => {}
            0x303 => self.decode_en = self.decode_en & 0x00ff_ffff | u32::from(value) << 24,
            _ => panic!("a byte write at ACPI-MMIO {offset:#x}, which nothing here should write"),
        }
    }

    fn read_u32(&mut self, offset: u16) -> u32 {
        match offset {
            0x300 => self.decode_en,
            0x3C0 if self.select & fch::SELECT_DEBUG != 0 => 0xdead_beef,
            0x3C0 => self.reset_stat,
            _ => panic!("a dword read at ACPI-MMIO {offset:#x}, which the model does not hold"),
        }
    }

    fn write_u32(&mut self, offset: u16, value: u32) {
        self.writes += 1;
        match offset {
            0x3C0 => self.reset_stat &= !value,
            _ => panic!("a dword write at ACPI-MMIO {offset:#x}, which nothing here should write"),
        }
    }
}

impl Window for Model {
    fn read(&mut self, register: Register) -> u32 {
        if !self.enabled() {
            return u32::MAX;
        }
        match register {
            Register::Control => self.control(),
            Register::Count => u32::from(self.counter),
        }
    }

    fn write(&mut self, register: Register, value: u32) {
        self.writes += 1;
        if !self.enabled() {
            return;
        }
        match register {
            Register::Count => self.reload = value as u16,
            Register::Control => {
                let was_running = self.run;
                self.run = value & Control::RUN != 0;
                if value & Control::FIRED != 0 {
                    self.fired = false;
                }
                self.power_off = value & Control::POWER_OFF != 0;
                if self.action_en.is_some() {
                    self.action_en = Some(value & Control::ACTION_EN != 0);
                }
                if !self.run {
                    self.counting = false;
                } else if value & Control::TRIGGER != 0 && was_running {
                    self.counter = self.reload;
                    self.counting = true;
                }
            }
        }
    }
}

/// The loader's survey and arm, then the kernel's take-over, as one boot
/// runs them; the kernel's half is `kernel/src/arch/x86_64/watchdog.rs`'s.
fn boot(model: &mut Model) -> fch::Armed {
    fch::survey(model).expect("ACPI-MMIO answers");
    let armed = fch::arm(model).expect("armed");
    take_over(model);
    armed
}

fn take_over(model: &mut Model) {
    let found = fch::Found::read(model);
    assert!(found.armed(), "the kernel finds the loader's arm: {found:?}");
    fch::feed(model);
}

/// Fed every quarter of the bound, as the kernel's scheduler pass feeds it.
fn fed_for(model: &mut Model, ms: u64) {
    let every = toyos_tco::BOUND_MS / 4;
    for _ in 0..ms / every {
        model.advance_ms(every);
        fch::feed(model);
    }
}

#[test]
fn the_tick_holds_the_bound_exactly() {
    assert_eq!(fch::TICK_MS, 100);
    assert_eq!(fch::TIMER, 96);
    assert_eq!(fch::bound_of(TIMER), toyos_tco::BOUND_MS);
}

/// The whole claim: armed out of firmware's reset state, fed it stays up, and
/// unfed it resets the machine inside one tick of the bound.
#[test]
fn an_unfed_timer_resets_the_machine_at_its_bound() {
    let mut model = Model::new();
    let armed = boot(&mut model);
    assert_eq!(armed.decode_en & fch::WATCHDOG_TMR_EN, fch::WATCHDOG_TMR_EN);
    assert_eq!((armed.decode_en >> 24) & 0xf, fch::TICK as u32);
    assert_eq!(armed.decode_en >> 28, DECODE_EN_RESET >> 28, "byte 3's high nibble is not the watchdog's");
    assert_eq!(armed.count, TIMER);

    fed_for(&mut model, 60_000);
    assert_eq!(model.resets, 0, "a fed timer reset the machine");

    model.advance_ms(toyos_tco::BOUND_MS - fch::TICK_MS - 1);
    assert_eq!(model.resets, 0, "the reset came a tick early");
    model.advance_ms(fch::TICK_MS + 1);
    assert_eq!(model.resets, 1, "no reset at the bound");
    assert!(model.fired);
}

/// Setting run and trigger in one write leaves the timer running with no
/// count, so it never expires (fact 3.2): the start takes two.
#[test]
fn one_write_of_run_and_trigger_starts_no_count() {
    let mut model = Model::new();
    fch::arm(&mut model).expect("armed");
    fch::stop(&mut model);
    Window::write(&mut model, Register::Control, Control::RUN | Control::TRIGGER);
    model.advance_ms(2 * toyos_tco::BOUND_MS);
    assert_eq!(model.resets, 0, "the model starts a count on one write, which the hardware does not");
}

#[test]
fn a_stopped_timer_resets_nothing() {
    let mut model = Model::new();
    boot(&mut model);
    fch::stop(&mut model);
    model.advance_ms(10 * toyos_tco::BOUND_MS);
    assert_eq!(model.resets, 0);
}

/// The boot after the reset: both records say so, the loader clears both,
/// and the boot after that reads neither.
#[test]
fn the_boot_after_an_expiry_reads_it_and_the_next_does_not() {
    let mut model = Model::new();
    boot(&mut model);
    model.advance_ms(toyos_tco::BOUND_MS);
    assert_eq!(model.resets, 1);

    let survey = fch::survey(&mut model).expect("ACPI-MMIO answers");
    assert!(survey.issued_reset() && survey.fired() == Some(true) && survey.watchdog_reset(), "{survey:?}");
    assert!(matches!(survey.reset_stat, ResetStat::Read { after: 0, .. }), "{survey:?}");
    assert_eq!(model.reset_stat, 0, "the loader left the FCH's record set");
    assert!(!model.fired, "the loader left the block's record set");

    // Unarmed, then a CF9 reset: no watchdog caused that one.
    model.cf9();
    let survey = fch::survey(&mut model).expect("ACPI-MMIO answers");
    assert!(!survey.watchdog_reset(), "a CF9 reset read as the watchdog's: {survey:?}");
}

/// Firmware that clears the decode at reset hides `WatchdogFired` from the
/// survey; it outlives warm resets, so `arm` clears it as stale and no later
/// pass reports it.
#[test]
fn a_fired_bit_the_survey_cannot_read_is_cleared_by_the_arm() {
    let mut model = Model { decode_clears_on_reset: true, ..Model::new() };
    boot(&mut model);
    model.advance_ms(toyos_tco::BOUND_MS);
    let survey = fch::survey(&mut model).expect("ACPI-MMIO answers");
    assert_eq!(survey.fired(), None, "{survey:?}");
    assert!(survey.issued_reset(), "{survey:?}");
    assert!(model.fired);

    let armed = fch::arm(&mut model).expect("armed");
    assert!(armed.cleared_fired && !model.fired, "{armed:?}");
    take_over(&mut model);
    fch::stop(&mut model);
    model.cf9();
    let survey = fch::survey(&mut model).expect("ACPI-MMIO answers");
    assert!(!survey.watchdog_reset(), "{survey:?}");
    assert!(!fch::arm(&mut model).expect("armed").cleared_fired);
}

/// The decode gates the timer: a count cut off by a reset that cleared it
/// stays still, the survey cannot stop it, and the arm restarts it whole.
#[test]
fn a_count_the_reset_left_undecoded_restarts_from_the_bound() {
    let mut model = Model { decode_clears_on_reset: true, ..Model::new() };
    boot(&mut model);
    model.advance_ms(toyos_tco::BOUND_MS / 2);
    model.cf9();
    assert!(model.run && model.counting, "the reset stopped the count, so this tests nothing");

    let survey = fch::survey(&mut model).expect("ACPI-MMIO answers");
    assert!(survey.found.is_none() && !survey.stopped, "{survey:?}");
    model.advance_ms(10 * toyos_tco::BOUND_MS);
    assert_eq!(model.resets, 1, "an undecoded timer counted");

    fch::arm(&mut model).expect("armed");
    take_over(&mut model);
    model.advance_ms(toyos_tco::BOUND_MS - fch::TICK_MS - 1);
    assert_eq!(model.resets, 1, "the arm resumed the old count instead of a whole bound");
    model.advance_ms(fch::TICK_MS + 1);
    assert_eq!(model.resets, 2);
}

/// A refusal once the decode is on leaves the timer stopped, so a count the
/// last boot left does not resume behind it for the kernel to take over.
#[test]
fn a_refusal_after_the_decode_leaves_the_timer_stopped() {
    let mut model = Model { decode_clears_on_reset: true, ..Model::new() };
    boot(&mut model);
    model.cf9();
    model.tick_locked = true;
    model.decode_en &= !0x0f00_0000;
    assert!(matches!(fch::arm(&mut model), Err(Refused::TickStuck(_))));
    assert!(!model.run);
    assert!(!fch::Found::read(&mut model).armed());
}

/// The block survives a CF9 reset, so a boot that reset the machine with the
/// timer armed hands the next one a count still running.
#[test]
fn a_count_the_last_boot_left_running_is_stopped() {
    let mut model = Model::new();
    boot(&mut model);
    model.cf9();
    let survey = fch::survey(&mut model).expect("ACPI-MMIO answers");
    assert!(survey.stopped, "{survey:?}");
    assert!(!survey.watchdog_reset(), "{survey:?}");
    model.advance_ms(10 * toyos_tco::BOUND_MS);
    assert_eq!(model.resets, 1, "the count the last boot left reset this one");
}

/// Family 1Ah's MP1-owned bit survives every write here: a pet that cleared it
/// would leave an expiry that only sets `WatchdogFired`.
#[test]
fn every_write_keeps_the_bits_it_does_not_own() {
    let mut model = Model { action_en: Some(true), ..Model::new() };
    boot(&mut model);
    fed_for(&mut model, 10_000);
    assert_eq!(model.action_en, Some(true));
    model.advance_ms(toyos_tco::BOUND_MS);
    assert_eq!(model.resets, 1);
    fch::survey(&mut model).expect("ACPI-MMIO answers");
    assert_eq!(model.action_en, Some(true));
}

/// Firmware's choices this cannot undo are refused with no timer written.
#[test]
fn what_firmware_holds_is_refused_by_name() {
    let mut model = Model { alternate: 1, reset_stat: WATCHDOG_ISSUE_RESET, ..Model::new() };
    assert_eq!(fch::arm(&mut model), Err(Refused::AlternateBase(1)));
    assert_eq!(fch::survey(&mut model), Err(Unsurveyed::AlternateBase(1)));
    assert_eq!(model.writes, 0, "a refusal wrote a register");

    let mut model = Model { decode_locked: true, ..Model::new() };
    assert!(matches!(fch::arm(&mut model), Err(Refused::DecodeStuck(_))));
    assert!(!model.run);

    // A machine whose ACPI-MMIO decodes nothing reads all ones.
    struct Nothing;
    impl AcpiMmio for Nothing {
        fn read_u8(&mut self, _: u16) -> u8 { u8::MAX }
        fn write_u8(&mut self, _: u16, _: u8) { panic!("written") }
        fn read_u32(&mut self, _: u16) -> u32 { u32::MAX }
        fn write_u32(&mut self, _: u16, _: u32) { panic!("written") }
    }
    impl Window for Nothing {
        fn read(&mut self, _: Register) -> u32 { u32::MAX }
        fn write(&mut self, _: Register, _: u32) { panic!("written") }
    }
    assert_eq!(fch::arm(&mut Nothing), Err(Refused::AcpiMmioAbsent));
    assert_eq!(fch::survey(&mut Nothing), Err(Unsurveyed::AcpiMmioAbsent));
}

/// Decode off: the block reads all ones, so the kernel finds nothing armed and
/// the survey reads only the FCH's record.
#[test]
fn an_undecoded_window_is_neither_armed_nor_read() {
    let mut model = Model::new();
    let found = fch::Found::read(&mut model);
    assert!(!found.armed() && !found.control.decoded(), "{found:?}");
    model.reset_stat = WATCHDOG_ISSUE_RESET;
    let survey = fch::survey(&mut model).expect("ACPI-MMIO answers");
    assert_eq!(survey.found, None);
    assert!(survey.watchdog_reset(), "{survey:?}");
}

#[test]
fn a_debug_view_of_the_reset_status_is_not_read_as_one() {
    let mut model = Model { select: fch::SELECT_DEBUG, ..Model::new() };
    let survey = fch::survey(&mut model).expect("ACPI-MMIO answers");
    assert_eq!(survey.reset_stat, ResetStat::DebugSelected(fch::SELECT_DEBUG));
    assert!(!survey.watchdog_reset());
    assert_eq!(model.writes, 0);
}

#[test]
fn the_function_is_found_by_its_revision_too() {
    assert!(fch::carries(0x1022, 0x790b, 0x51));
    assert!(fch::carries(0x1022, 0x790b, 0x61));
    assert!(!fch::carries(0x1022, 0x790b, 0x4f), "the SB800 layout, a base in PM 48h");
    assert!(!fch::carries(0x1022, 0x780b, 0x51));
    assert!(!fch::carries(0x8086, 0x790b, 0x51));
}
