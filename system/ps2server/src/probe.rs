//! The probe: one sequence, the same on a boot's first start and on a
//! restart, which reads nothing firmware or a predecessor left.
//!
//! **C** is IBM's 84F9735 "Keyboard and Auxiliary Device Controller"
//! section, **K** its "Keyboards" section and **K88** the May 1988 edition's,
//! each by its own page; **TP** is IBM's *TrackPoint System Version 4.0
//! Engineering Specification*, the auxiliary device's protocol.
//!
//! 1. **Quiet:** the output buffer drained, `0xAD`, `0xA7` (C 12), and it
//!    drained again.
//! 2. **Keyboard:** the command byte with the keyboard's interrupt alone,
//!    translation on and both interfaces still disabled; `0xAE`; then IBM's
//!    Reset, `0xFF` (K 13), answered `0xFA` and the completion code `0xAA`,
//!    after which the keyboard is in set 2 and scans (K 11, K 13). Translated
//!    (C 10), set 2 is set 1 on the wire, whatever set firmware or a
//!    predecessor left. The `0xAA` is taken whenever it comes inside the
//!    stage's ceiling: K 11's 300-500 ms window decides nothing here.
//! 3. **Aux:** the command byte with the aux interrupt alone and the keyboard
//!    interface disabled again, so the keyboard holds what it would send
//!    (K88 39); `0xA8`; `0xD4 0xFF`, answered `0xFA 0xAA` and its id (TP 9,
//!    12), and `0xD4 0xF4` (TP 13). A device that does not answer its reset
//!    is absent.
//! 4. **Arming:** the command byte whole, read back with `0x20` while the
//!    keyboard interface is disabled, so the answer is the controller's; then
//!    `0xAE`, and the keyboard sends what it held.
//!
//! Every command byte holds the System Flag (C 11) and states the keyboard
//! interface disabled (bit 4): one written with it clear would enable the
//! keyboard as `0xAE` does. A device's answers are waited for on its
//! interrupt, and a byte found waiting before a write is drained, by name,
//! until the keyboard scans, and from then on is a keystroke for the stream.

use crate::bus::*;

/// The controller-only stages' ceiling: the quiet stage and the arming.
const CONTROLLER_NS: u64 = 250_000_000;
/// The keyboard stage's, inside which its reset's `0xAA` is taken.
pub const KEYBOARD_NS: u64 = 750_000_000;
/// The aux stage's: TP 9's 500 ms of diagnostics, and its acknowledgements.
const AUX_NS: u64 = 600_000_000;

/// The most one drain reads before it calls the output buffer wedged.
const DRAIN: usize = 32;

/// The keyboard's Reset, its acknowledgement and its completion code (K 13).
const RESET: u8 = 0xFF;
const ACK: u8 = 0xFA;
const PASSED: u8 = 0xAA;
/// The auxiliary device's Enable (TP 13).
const ENABLE: u8 = 0xF4;

/// The command bytes the stages write, each with the System Flag and the
/// keyboard interface disabled.
const KEYBOARD_STAGE: u8 = CB_KEYBOARD_IRQ | CB_SYSTEM_FLAG | CB_KEYBOARD_DISABLED | CB_AUX_DISABLED | CB_TRANSLATE;
const AUX_STAGE: u8 = CB_AUX_IRQ | CB_SYSTEM_FLAG | CB_KEYBOARD_DISABLED | CB_TRANSLATE;

/// What the probe left.
#[derive(Debug, PartialEq, Eq)]
pub enum Probed {
    /// Nothing decodes the controller's ports.
    Absent,
    /// The keyboard, the pointer or both scan, and the armed line says which;
    /// `aux` is whether the pointer is served. `backlog` is every byte read
    /// since they did, in order, for the stream's decoders first.
    Armed { aux: bool, backlog: Vec<Byte> },
    /// Neither is served, and each was said by name.
    Failed,
}

/// Run the probe over `c`, saying what each stage found through `say`.
pub fn probe(c: &mut impl Controller, say: &mut dyn FnMut(String)) -> Probed {
    if c.status() == FLOATING {
        say(format!("ps2server: absent — port 0x64 reads {FLOATING:#04x}, nothing decodes it"));
        return Probed::Absent;
    }
    let mut probe = Probe { c, say, drained: Vec::new(), backlog: Vec::new(), scanning: false, aux_reporting: false };
    match probe.run() {
        Ok(probed) => probed,
        Err(Stopped) => Probed::Failed,
    }
}

/// A stage said why the probe ends here.
struct Stopped;

struct Probe<'a, C: Controller> {
    c: &'a mut C,
    say: &'a mut dyn FnMut(String),
    /// Bytes read before they could be anybody's input, not yet said.
    drained: Vec<Byte>,
    backlog: Vec<Byte>,
    /// The keyboard's reset completed: what it sends now is typed.
    scanning: bool,
    /// The aux device reports: what it sends now is motion.
    aux_reporting: bool,
}

impl<C: Controller> Probe<'_, C> {
    fn run(&mut self) -> Result<Probed, Stopped> {
        self.quiet()?;
        let keyboard = self.keyboard()?;
        let aux = self.aux()?;
        if !keyboard && !aux {
            (self.say)("ps2server: neither the keyboard nor a pointer to serve".to_string());
            return Err(Stopped);
        }
        self.arm(keyboard, aux)?;
        Ok(Probed::Armed { aux, backlog: core::mem::take(&mut self.backlog) })
    }

    fn deadline(&mut self, ns: u64) -> u64 {
        self.c.now() + ns
    }

    /// Where a byte read off the output buffer goes, by when it was read.
    fn route(&mut self, byte: Byte) {
        let input = if byte.aux { self.aux_reporting } else { self.scanning };
        if input {
            self.backlog.push(byte);
        } else {
            self.drained.push(byte);
        }
    }

    /// Say what was drained since the last time, by name.
    fn said_drained(&mut self, when: &str) {
        if self.drained.is_empty() {
            return;
        }
        let bytes: Vec<String> = self
            .drained
            .drain(..)
            .map(|b| if b.aux { format!("aux {:#04x}", b.value) } else { format!("{:#04x}", b.value) })
            .collect();
        (self.say)(format!("ps2server: drained {} byte(s) {when}: [{}]", bytes.len(), bytes.join(", ")));
    }

    fn send(&mut self, port: Port, byte: u8, deadline: u64, what: &str) -> Result<(), Stopped> {
        let mut found = Vec::new();
        let sent = self.c.send(port, byte, deadline, &mut |b| found.push(b));
        for b in found {
            self.route(b);
        }
        sent.map_err(|NotTaken { status }| {
            self.said_drained(&format!("waiting to write {what}"));
            (self.say)(format!("ps2server: the controller never took {what} (status {status:#04x})"));
            Stopped
        })
    }

    fn command(&mut self, byte: u8, deadline: u64, what: &str) -> Result<(), Stopped> {
        self.send(Port::Command, byte, deadline, what)
    }

    fn command_byte(&mut self, byte: u8, deadline: u64) -> Result<(), Stopped> {
        self.command(WRITE_COMMAND_BYTE, deadline, "0x60")?;
        self.send(Port::Data, byte, deadline, &format!("the command byte {byte:#04x}"))
    }

    /// The next byte of the device `aux` names, waited for on its interrupt
    /// until `deadline`; every other byte read on the way is routed.
    fn answer(&mut self, aux: bool, deadline: u64) -> Result<u8, Silent> {
        loop {
            let irq = self.c.wait(deadline);
            while let Some(byte) = self.c.poll() {
                if byte.aux == aux {
                    return Ok(byte.value);
                }
                self.route(byte);
            }
            if irq.is_none() {
                let status = self.c.status();
                return Err(Silent { full: status & OBF != 0 });
            }
        }
    }

    /// Step 1, drained before its first write too, so a wedged output buffer
    /// is found by [`DRAIN`] reads and no write is made into it (C 14).
    fn quiet(&mut self) -> Result<(), Stopped> {
        self.drain()?;
        let deadline = self.deadline(CONTROLLER_NS);
        self.command(DISABLE_KEYBOARD, deadline, "0xAD")?;
        self.command(DISABLE_AUX, deadline, "0xA7")?;
        self.drain()?;
        self.said_drained("before the probe");
        Ok(())
    }

    fn drain(&mut self) -> Result<(), Stopped> {
        for _ in 0..DRAIN {
            match self.c.poll() {
                Some(byte) => self.route(byte),
                None => return Ok(()),
            }
        }
        self.said_drained("before the probe");
        (self.say)(format!("ps2server: the output buffer never drains — {DRAIN} reads left it full"));
        Err(Stopped)
    }

    /// Step 2: whether the keyboard scans, set 2 translated.
    fn keyboard(&mut self) -> Result<bool, Stopped> {
        let deadline = self.deadline(KEYBOARD_NS);
        self.command_byte(KEYBOARD_STAGE, deadline)?;
        self.command(ENABLE_KEYBOARD, deadline, "0xAE")?;
        self.send(Port::Data, RESET, deadline, "the keyboard's reset")?;
        let reset = self.c.now();
        let disabled = |this: &mut Self, why: String| {
            this.said_drained("before the keyboard scanned");
            (this.say)(format!("ps2server: kbd DISABLED — {why}"));
            Ok(false)
        };
        match self.answer(false, deadline) {
            Ok(ACK) => {}
            Ok(other) => return disabled(self, format!("the reset answered {other:#04x}, not 0xfa")),
            Err(silent) => return disabled(self, format!("no 0xfa to the reset in {} ms ({silent})", KEYBOARD_NS / 1_000_000)),
        }
        let acked = self.c.now();
        match self.answer(false, deadline) {
            Ok(PASSED) => {}
            Ok(other) => return disabled(self, format!("the reset's 0xfa was followed by {other:#04x}, not 0xaa")),
            Err(silent) => return disabled(self, format!("no 0xaa in {} ms of the reset ({silent})", KEYBOARD_NS / 1_000_000)),
        }
        let passed = self.c.now();
        self.scanning = true;
        self.said_drained("before the keyboard scanned");
        (self.say)(format!(
            "ps2server: kbd reset, 0xfa {} us after the command and 0xaa {} us after the 0xfa — set 2, translated",
            (acked - reset) / 1_000,
            (passed - acked) / 1_000
        ));
        Ok(true)
    }

    /// Step 3: whether the aux device reports.
    fn aux(&mut self) -> Result<bool, Stopped> {
        let deadline = self.deadline(AUX_NS);
        self.command_byte(AUX_STAGE, deadline)?;
        self.command(ENABLE_AUX, deadline, "0xA8")?;
        self.command(WRITE_AUX, deadline, "0xD4")?;
        self.send(Port::Data, RESET, deadline, "the aux device's reset")?;
        match self.answer(true, deadline) {
            Ok(ACK) => {}
            Ok(other) => return self.aux_off(format!("aux DISABLED — the reset answered {other:#04x}, not 0xfa")),
            Err(silent) => {
                let ms = AUX_NS / 1_000_000;
                return self.aux_off(format!("aux absent — its reset went unanswered in {ms} ms ({silent})"));
            }
        }
        let acked = self.c.now();
        let completion = self.answer(true, deadline);
        let passed = self.c.now();
        let id = match (&completion, self.answer(true, deadline)) {
            (Ok(PASSED), Ok(id)) => id,
            (_, id) => {
                return self.aux_off(format!(
                    "aux DISABLED — the reset's 0xfa was followed by {completion:?} and {id:?}, not 0xaa and an id"
                ))
            }
        };
        self.command(WRITE_AUX, deadline, "0xD4")?;
        self.send(Port::Data, ENABLE, deadline, "the aux device's enable")?;
        // From the enable on, what the device sends is motion.
        self.aux_reporting = true;
        match self.answer(true, deadline) {
            Ok(ACK) => {}
            other => {
                self.aux_reporting = false;
                return self.aux_off(format!("aux DISABLED — its enable answered {other:?}, not 0xfa"));
            }
        }
        self.said_drained("before the aux device reported");
        (self.say)(format!(
            "ps2server: aux reset, 0xaa {} us after its 0xfa, id {id:#04x} — reporting",
            (passed - acked) / 1_000
        ));
        Ok(true)
    }

    /// The aux device is not served, and `why` is said.
    fn aux_off(&mut self, why: String) -> Result<bool, Stopped> {
        self.said_drained("before the aux device reported");
        (self.say)(format!("ps2server: {why}"));
        Ok(false)
    }

    /// Step 4.
    fn arm(&mut self, keyboard: bool, aux: bool) -> Result<(), Stopped> {
        let deadline = self.deadline(CONTROLLER_NS);
        let mut byte = CB_SYSTEM_FLAG | CB_KEYBOARD_DISABLED | CB_TRANSLATE;
        if keyboard {
            byte |= CB_KEYBOARD_IRQ;
        }
        byte |= if aux { CB_AUX_IRQ } else { CB_AUX_DISABLED };
        self.command_byte(byte, deadline)?;
        self.command(READ_COMMAND_BYTE, deadline, "0x20")?;
        let read = loop {
            let past = self.c.now() >= deadline;
            match self.c.poll() {
                Some(Byte { value, aux: false }) => break Some(value),
                Some(motion) => self.route(motion),
                None if past => break None,
                None => core::hint::spin_loop(),
            }
        };
        if read != Some(byte) {
            let read = read.map_or("nothing".to_string(), |v| format!("{v:#04x}"));
            (self.say)(format!("ps2server: DISABLED — command byte {byte:#04x} did not take (read back {read})"));
            return Err(Stopped);
        }
        if keyboard {
            self.command(ENABLE_KEYBOARD, deadline, "0xAE")?;
        }
        (self.say)(format!(
            "ps2server: armed, command byte {byte:#04x} — keyboard {}, aux {}",
            if keyboard { "on" } else { "off" },
            if aux { "on" } else { "off" }
        ));
        Ok(())
    }
}

/// No answer by the deadline; whether the output buffer held a byte anyway.
#[derive(Debug)]
struct Silent {
    full: bool,
}

impl core::fmt::Display for Silent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(if self.full { "the output buffer full" } else { "the output buffer empty" })
    }
}

#[cfg(test)]
mod tests {
    use toyos_input::wire::Record;

    use super::*;
    use crate::fake::{Fake, Reset};
    use crate::stream::Stream;

    const MS: u64 = 1_000_000;
    const US: u64 = 1_000;

    fn probed(fake: &mut Fake) -> (Probed, Vec<String>) {
        let mut said = Vec::new();
        let probed = probe(fake, &mut |line| said.push(line));
        (probed, said)
    }

    fn armed(fake: &mut Fake) -> (bool, Vec<Byte>, Vec<String>) {
        match probed(fake) {
            (Probed::Armed { aux, backlog }, said) => (aux, backlog, said),
            (other, said) => panic!("probed {other:?}: {said:#?}"),
        }
    }

    /// Every record the stream makes until `until`, from what the probe left.
    fn records(fake: &mut Fake, aux: bool, backlog: &[Byte], until: u64) -> Vec<Record> {
        let mut said = |line: String| eprintln!("{line}");
        let (mut stream, mut out) = Stream::new(fake, aux, backlog, &mut said);
        loop {
            let irq = fake.wait(stream.deadline().min(until));
            if irq.is_none() && fake.now >= until {
                return out;
            }
            out.extend(stream.wake(fake, irq, &mut said).expect("the stream ended"));
        }
    }

    fn typed_a(records: &[Record]) -> bool {
        records.iter().any(|r| matches!(r, Record::Key(k) if k.keycode == 0x04 && k.pressed()))
    }

    fn line<'a>(said: &'a [String], start: &str) -> &'a str {
        said.iter().find(|l| l.starts_with(start)).unwrap_or_else(|| panic!("no {start:?} in {said:#?}"))
    }

    /// The two times the keyboard's line names, in microseconds.
    fn reset_times(said: &[String]) -> (u64, u64) {
        let words: Vec<&str> = line(said, "ps2server: kbd reset").split(' ').collect();
        let at = |after: &str| -> u64 {
            let i = words.iter().position(|w| *w == after).expect("the line's shape");
            words[i + 1].parse().expect("a time")
        };
        (at("0xfa"), at("0xaa"))
    }

    #[test]
    fn qemu_arms_its_keyboard_set_2_translated_and_its_mouse() {
        let mut fake = Fake::qemu();
        let (aux, backlog, said) = armed(&mut fake);
        assert!(aux, "{said:#?}");
        assert_eq!(fake.cb, 0x47, "both interrupts, the System Flag, translation, both clocks on");
        line(&said, "ps2server: armed, command byte 0x57 — keyboard on, aux on");
        line(&said, "ps2server: aux reset");
        let at = fake.now + MS;
        fake.type_a(at);
        assert!(typed_a(&records(&mut fake, aux, &backlog, at + 10 * MS)));
    }

    #[test]
    fn the_t14_arms_its_keyboard_and_its_trackpoint() {
        let mut fake = Fake::t14();
        let (aux, _, said) = armed(&mut fake);
        assert!(aux, "{said:#?}");
        let (acked, passed) = reset_times(&said);
        assert!((80..100).contains(&acked), "the 0xfa {acked} us after the reset");
        assert!((516_000..516_100).contains(&passed), "the 0xaa {passed} us after the 0xfa");
        assert!(line(&said, "ps2server: aux reset").contains("0xaa 367"), "{said:#?}");
        assert_eq!(fake.cb, 0x47);
        line(&said, "ps2server: armed, command byte 0x57 — keyboard on, aux on");
    }

    #[test]
    fn the_amd_bench_arms_its_keyboard_and_finds_no_pointer() {
        let mut fake = Fake::amd();
        let (aux, backlog, said) = armed(&mut fake);
        assert!(!aux, "{said:#?}");
        let (_, passed) = reset_times(&said);
        assert!((2_200..2_300).contains(&passed), "the 0xaa {passed} us after the 0xfa");
        line(&said, "ps2server: aux absent — its reset went unanswered in 600 ms");
        assert_eq!(fake.cb, 0x65, "the keyboard's interrupt, the System Flag, translation, the aux clock off");
        line(&said, "ps2server: armed, command byte 0x75 — keyboard on, aux off");
        let at = fake.now + MS;
        fake.type_a(at);
        assert!(typed_a(&records(&mut fake, aux, &backlog, at + 10 * MS)));
    }

    /// A key pressed while the TrackPoint runs its reset: the keyboard holds
    /// it until the arming's `0xAE`, so it is never drained, never read as the
    /// command byte and never holds the output buffer against the aux answer.
    #[test]
    fn a_keystroke_during_the_aux_reset_reaches_the_keyboard_after_the_arming() {
        let mut fake = Fake::t14();
        // The keyboard stage ends 516 ms in; the aux device answers its reset
        // 369.5 ms after that.
        fake.type_a(700 * MS);
        let (aux, backlog, said) = armed(&mut fake);
        assert!(aux, "{said:#?}");
        assert!(!said.iter().any(|l| l.contains("drained")), "{said:#?}");
        let until = fake.now + 10 * MS;
        let records = records(&mut fake, aux, &backlog, until);
        assert!(typed_a(&records), "{records:?}");
        let loaded = fake.kbd_loads.iter().find(|(_, v)| *v == 0x1E).expect("the keystroke loaded");
        assert!(loaded.0 >= fake.kbd_enabled_at, "it loaded at {} ns, the arming's 0xAE at {}", loaded.0, fake.kbd_enabled_at);
    }

    /// K 1: a keyboard POST left in set 1, behind a controller that does not
    /// translate. The stage translates before the reset, which leaves set 2.
    #[test]
    fn a_start_after_set_1_with_translation_off_arms_set_2_translated() {
        let mut fake = Fake::t14();
        fake.cb = 0x07;
        fake.kbd.set = 1;
        let (aux, backlog, said) = armed(&mut fake);
        assert!(aux, "{said:#?}");
        let at = fake.now + MS;
        fake.type_a(at);
        assert!(typed_a(&records(&mut fake, aux, &backlog, at + 10 * MS)));
    }

    #[test]
    fn a_reset_answered_0xfc_disables_the_keyboard_by_name() {
        let mut fake = Fake::t14();
        fake.kbd.reset = Reset { ack: Some(80 * US), then: Some((516 * MS, 0xFC)) };
        let (aux, _, said) = armed(&mut fake);
        assert!(aux, "{said:#?}");
        line(&said, "ps2server: kbd DISABLED — the reset's 0xfa was followed by 0xfc, not 0xaa");
        assert_eq!(fake.cb, 0x56, "the keyboard's interrupt off and its interface disabled");
        line(&said, "ps2server: armed, command byte 0x56 — keyboard off, aux on");
    }

    #[test]
    fn an_0xaa_past_the_keyboard_stages_ceiling_disables_the_keyboard_by_name() {
        let mut fake = Fake::t14();
        fake.kbd.reset = Reset::answered(80 * US, KEYBOARD_NS + MS);
        let (_, _, said) = armed(&mut fake);
        line(&said, "ps2server: kbd DISABLED — no 0xaa in 750 ms of the reset (the output buffer empty)");
    }

    #[test]
    fn a_reset_with_no_0xfa_disables_the_keyboard_by_name() {
        let mut fake = Fake::t14();
        fake.kbd.reset = Reset { ack: None, then: None };
        let (_, _, said) = armed(&mut fake);
        line(&said, "ps2server: kbd DISABLED — no 0xfa to the reset in 750 ms (the output buffer empty)");
    }

    #[test]
    fn a_floating_status_port_is_no_controller() {
        let mut fake = Fake::t14();
        fake.absent = true;
        let (probed, said) = probed(&mut fake);
        assert_eq!(probed, Probed::Absent);
        line(&said, "ps2server: absent — port 0x64 reads 0xff");
        assert!(fake.commands.is_empty(), "a byte written to ports nothing decodes: {:02x?}", fake.commands);
    }

    /// The arming's own byte dropped: the aux stage's is what keeps the
    /// keyboard interface disabled at the `0x20`, and is read back.
    #[test]
    fn a_command_byte_the_controller_dropped_is_said_by_name() {
        let mut fake = Fake::t14();
        fake.drops_command_byte = Some(0x57);
        let (probed, said) = probed(&mut fake);
        assert_eq!(probed, Probed::Failed);
        line(&said, "ps2server: DISABLED — command byte 0x57 did not take (read back 0x56)");
    }

    #[test]
    fn a_wedged_output_buffer_is_said_after_32_reads_with_nothing_written() {
        let mut fake = Fake::t14();
        fake.wedged = true;
        let (probed, said) = probed(&mut fake);
        assert_eq!(probed, Probed::Failed);
        line(&said, "ps2server: drained 32 byte(s) before the probe: [0x55, 0x55");
        line(&said, "ps2server: the output buffer never drains — 32 reads left it full");
        assert!(fake.commands.is_empty(), "C 14: a write into a full output buffer: {:02x?}", fake.commands);
    }

    #[test]
    fn bytes_waiting_before_the_probe_are_drained_by_name() {
        let mut fake = Fake::t14();
        fake.kbd_sends(0, &[0x1C, 0x1C]);
        let (_, backlog, said) = armed(&mut fake);
        assert!(backlog.is_empty(), "{backlog:?}");
        assert!(said.iter().any(|l| l.starts_with("ps2server: drained") && l.contains("0x1e")), "{said:#?}");
    }
}
