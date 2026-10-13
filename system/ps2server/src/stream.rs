//! The stream: one wake per interrupt record, which empties the output
//! buffer into the decoders and hands the owner of input what they made.
//!
//! **One wake** takes the claim's record; while the output buffer is full,
//! up to [`BURST`] bytes, reads one byte and feeds the decoder `AUXB` names;
//! and reads the clock after the status read that found it empty, `t_end`.
//! Both lines are edge-triggered and the controller raises its line whenever
//! it loads a byte (C 11), so a byte that loads after that status read raises
//! its own wake. Since the output buffer empties when it is read (C 7), one
//! still full after [`BURST`] reads is no state IBM's protocol makes: the
//! stream ends by name, and its supervisor's restart limit decides the rest.
//!
//! **The pointer's framer is fed the device's own idle time.** A wake's first
//! aux byte is fed the gap from the last wake's `t_end` to its record's first
//! interrupt, every later byte of the wake none. A device held off while the
//! output buffer is full (TP 50) loads its next byte one byte's time after
//! the read that released it, which is before `t_end`, and one that queues
//! loads it in the same wake: a slow reader delays a resync and never splits a
//! packet.
//!
//! **A device that resets itself** (`0xAA 0x00`) has its reporting enabled
//! again with `0xD4 0xF4` (TP 13), whose `0xFA` comes in the stream within
//! TP 46's 25 ms; [`REENABLES`] failures in a row write the pointer off with
//! `0xA7` alone (C 12), so its line never rises again.

use toyos_abi::input::{RawKeyEvent, MOD_RELEASED};
use toyos_input::wire::{Motion, Record, Report};
use toyos_ps2::{KeyDecoder, KeyOutcome, MouseDecoder, MouseOutcome};

use crate::bus::*;

/// The most bytes one wake reads before it calls the controller wedged.
pub const BURST: usize = 32;

/// TP 46: the device acknowledges a command within 25 ms.
const ACK_NS: u64 = 25_000_000;
/// Re-enables in a row that fail before the pointer is written off.
const REENABLES: u32 = 3;
/// The device's Enable, and its acknowledgement.
const ENABLE: u8 = 0xF4;
const ACK: u8 = 0xFA;

/// How often the counters are said, at most, and only after an interrupt.
const COUNTERS_NS: u64 = 10_000_000_000;
/// The undecoded runs said one by one; past it they are counted.
const NAMED_RUNS: u32 = 8;
/// The most bytes of one undecoded run named.
const RUN: usize = 8;

/// The output buffer stayed full for [`BURST`] reads, said with what they
/// read.
#[derive(Debug)]
pub struct Wedged;

/// What the pointer's line is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Aux {
    /// Not served: absent, or written off.
    Off,
    Reporting,
    /// It reset itself; an enable is owed once the buffers are empty.
    Owed { failures: u32 },
    /// The enable is out, and its acknowledgement due by `by`.
    Enabling { by: u64, failures: u32 },
}

/// Bytes a decoder has taken and not yet resolved, kept until the byte that
/// ends the run, so an undecodable sequence is named whole.
#[derive(Default)]
struct Run(Vec<u8>);

impl Run {
    fn push(&mut self, byte: u8) {
        if self.0.len() < RUN {
            self.0.push(byte);
        }
    }

    /// The run ended in `last` and made nothing: all of it, by name.
    fn blame(&mut self, last: u8, aux: bool) -> String {
        self.push(last);
        let bytes: Vec<String> = self.0.drain(..).map(|b| format!("{b:#04x}")).collect();
        format!("{}[{}]", if aux { "aux " } else { "" }, bytes.join(", "))
    }
}

#[derive(Default)]
struct Counts {
    irqs: u64,
    bytes: u64,
    keys: u64,
    motion: u64,
    undecoded: u32,
    discarded: u64,
    overruns: u64,
}

pub struct Stream {
    keys: KeyDecoder,
    pointer: MouseDecoder,
    kbd_run: Run,
    aux_run: Run,
    aux: Aux,
    /// The running sum of the device's own idle gaps: the framer's clock.
    device_ns: u64,
    /// The last wake's `t_end`.
    end: u64,
    counts: Counts,
    /// Interrupts as of the last counters line, and when the next may be said.
    said_irqs: u64,
    next_counters: u64,
}

impl Stream {
    /// The stream after the probe, with the pointer served where `aux`, and
    /// the probe's `backlog` decoded first.
    pub fn new(c: &mut impl Controller, aux: bool, backlog: &[Byte], say: &mut dyn FnMut(String)) -> (Self, Vec<Record>) {
        let now = c.now();
        let mut stream = Self {
            keys: KeyDecoder::new(),
            pointer: MouseDecoder::new(),
            kbd_run: Run::default(),
            aux_run: Run::default(),
            aux: if aux { Aux::Reporting } else { Aux::Off },
            device_ns: 0,
            end: now,
            counts: Counts::default(),
            said_irqs: 0,
            next_counters: now + COUNTERS_NS,
        };
        let mut out = Vec::new();
        for &byte in backlog {
            stream.feed(byte, &mut out, say);
        }
        (stream, out)
    }

    /// When the stream next has to run whether or not an interrupt comes.
    pub fn deadline(&self) -> u64 {
        match self.aux {
            Aux::Enabling { by, .. } => by,
            Aux::Off | Aux::Reporting | Aux::Owed { .. } => u64::MAX,
        }
    }

    /// One wake: `irq` is the record it took, or `None` where
    /// [`Self::deadline`] passed with none.
    pub fn wake(&mut self, c: &mut impl Controller, irq: Option<Irq>, say: &mut dyn FnMut(String)) -> Result<Vec<Record>, Wedged> {
        let mut out = Vec::new();
        if irq.is_some() {
            self.counts.irqs += 1;
        }
        let mut gap = irq.map(|irq| irq.first_nanos.saturating_sub(self.end));
        let mut last = Vec::new();
        loop {
            let status = c.status();
            let shown = c.now();
            if status & OBF == 0 {
                self.end = shown;
                break;
            }
            if last.len() == BURST {
                say(format!("ps2server: the output buffer stayed full for {BURST} reads, the last {last:02x?} — the controller is wedged"));
                return Err(Wedged);
            }
            let byte = Byte { value: c.read_after(shown), aux: status & AUXB != 0 };
            last.push(byte.value);
            if byte.aux {
                self.device_ns += gap.take().unwrap_or(0);
            }
            self.feed(byte, &mut out, say);
        }
        if let Aux::Enabling { by, failures } = self.aux {
            if self.end >= by {
                say(format!("ps2server: aux reset itself, and its enable went unanswered in {} ms", ACK_NS / 1_000_000));
                self.aux = Aux::Owed { failures: failures + 1 };
            }
        }
        if let Aux::Owed { failures } = self.aux {
            self.reenable(c, failures, &mut out, say);
        }
        self.counters(say);
        Ok(out)
    }

    /// One byte to the decoder `AUXB` named.
    fn feed(&mut self, byte: Byte, out: &mut Vec<Record>, say: &mut dyn FnMut(String)) {
        self.counts.bytes += 1;
        if byte.aux {
            self.aux_byte(byte.value, out, say);
        } else {
            self.key_byte(byte.value, out, say);
        }
    }

    fn key_byte(&mut self, byte: u8, out: &mut Vec<Record>, say: &mut dyn FnMut(String)) {
        match self.keys.feed(byte) {
            KeyOutcome::Pending => return self.kbd_run.push(byte),
            KeyOutcome::Key { usage, pressed } => {
                self.counts.keys += 1;
                let modifiers = if pressed { 0 } else { MOD_RELEASED };
                out.push(Record::Key(RawKeyEvent { keycode: usage, modifiers }));
            }
            // The keyboard's overrun code: what it lost may be a release.
            KeyOutcome::Lost => {
                self.counts.overruns += 1;
                out.push(Record::Lost);
            }
            KeyOutcome::None => {
                let run = self.kbd_run.blame(byte, false);
                return self.undecoded(run, say);
            }
        }
        self.kbd_run.0.clear();
    }

    fn aux_byte(&mut self, byte: u8, out: &mut Vec<Record>, say: &mut dyn FnMut(String)) {
        match self.aux {
            Aux::Reporting => {}
            Aux::Enabling { failures, .. } => {
                if byte == ACK {
                    say("ps2server: aux reset itself, reporting enabled again".to_string());
                    self.aux = Aux::Reporting;
                } else {
                    say(format!("ps2server: aux reset itself, and its enable answered {byte:#04x}, not 0xfa"));
                    self.aux = Aux::Owed { failures: failures + 1 };
                }
                return;
            }
            // Nothing is reported until the enable is out, and nothing after
            // the write-off: a byte now is one this stream had to drop.
            Aux::Off | Aux::Owed { .. } => {
                self.counts.discarded += 1;
                return out.push(Record::Lost);
            }
        }
        match self.pointer.feed(byte, self.device_ns) {
            MouseOutcome::Pending => return self.aux_run.push(byte),
            MouseOutcome::Packet { buttons, dx, dy } => {
                self.counts.motion += 1;
                // A 9-bit count, which an `i16` holds.
                let motion = Motion::Relative { dx: dx as i16, dy: dy as i16 };
                out.push(Record::Pointer(Report { source: 0, buttons, scroll: 0, motion }));
            }
            MouseOutcome::Reset => self.aux = Aux::Owed { failures: 0 },
            MouseOutcome::Discarded => {
                self.counts.discarded += 1;
                out.push(Record::Lost);
            }
        }
        self.aux_run.0.clear();
    }

    fn undecoded(&mut self, run: String, say: &mut dyn FnMut(String)) {
        self.counts.undecoded += 1;
        if self.counts.undecoded <= NAMED_RUNS {
            say(format!("ps2server: no event from {run}"));
        }
    }

    /// Send the aux device its enable, or write it off after
    /// [`REENABLES`] failures.
    fn reenable(&mut self, c: &mut impl Controller, failures: u32, out: &mut Vec<Record>, say: &mut dyn FnMut(String)) {
        let deadline = c.now() + ACK_NS;
        if failures >= REENABLES {
            self.aux = Aux::Off;
            out.push(Record::Lost);
            let sent = self.send(c, Port::Command, DISABLE_AUX, deadline, out, say);
            say(format!("ps2server: aux enable failed {failures} times — pointer written off ({sent:?})"));
            return;
        }
        let sent = self
            .send(c, Port::Command, WRITE_AUX, deadline, out, say)
            .and_then(|()| self.send(c, Port::Data, ENABLE, deadline, out, say));
        match sent {
            Ok(()) => self.aux = Aux::Enabling { by: c.now() + ACK_NS, failures },
            Err(NotTaken { status }) => {
                say(format!("ps2server: aux reset itself, and the controller never took its enable (status {status:#04x})"));
                self.aux = Aux::Owed { failures: failures + 1 };
            }
        }
    }

    /// A write, feeding what was waiting before it to the decoders.
    fn send(
        &mut self,
        c: &mut impl Controller,
        port: Port,
        byte: u8,
        deadline: u64,
        out: &mut Vec<Record>,
        say: &mut dyn FnMut(String),
    ) -> Result<(), NotTaken> {
        let mut found = Vec::new();
        let sent = c.send(port, byte, deadline, &mut |b| found.push(b));
        for b in found {
            self.feed(b, out, say);
        }
        sent
    }

    fn counters(&mut self, say: &mut dyn FnMut(String)) {
        if self.counts.irqs == self.said_irqs || self.end < self.next_counters {
            return;
        }
        self.said_irqs = self.counts.irqs;
        self.next_counters = self.end + COUNTERS_NS;
        let Counts { irqs, bytes, keys, motion, undecoded, discarded, overruns } = self.counts;
        say(format!(
            "ps2server: {irqs} interrupts, {bytes} bytes, {keys} keys, {motion} motion, {undecoded} undecoded, \
             {discarded} discarded, {overruns} overruns"
        ));
    }
}

#[cfg(test)]
mod tests {
    use toyos_abi::input::MOD_RELEASED;
    use toyos_input::wire::{Motion, Record, Report};

    use super::*;
    use crate::fake::Fake;
    use crate::probe::{probe, Probed};

    const MS: u64 = 1_000_000;

    /// `fake` probed and armed, and the stream over it.
    fn armed(fake: &mut Fake) -> Stream {
        let mut said = |line: String| eprintln!("{line}");
        let Probed::Armed { aux, backlog, .. } = probe(fake, &mut said) else { panic!("the probe armed nothing") };
        let (stream, records) = Stream::new(fake, aux, &backlog, &mut said);
        assert!(records.is_empty(), "{records:?}");
        stream
    }

    /// Every record and line the stream makes until `until`.
    fn run(fake: &mut Fake, stream: &mut Stream, until: u64) -> (Vec<Record>, Vec<String>) {
        let (mut out, mut said) = (Vec::new(), Vec::new());
        loop {
            let irq = fake.wait(stream.deadline().min(until));
            if irq.is_none() && fake.now >= until {
                return (out, said);
            }
            out.extend(stream.wake(fake, irq, &mut |line| said.push(line)).expect("the stream ended"));
        }
    }

    #[test]
    fn a_keystroke_is_its_make_and_its_break() {
        let mut fake = Fake::qemu();
        let mut stream = armed(&mut fake);
        let at = fake.now + MS;
        fake.kbd_sends(at, &[0x1C, 0xF0, 0x1C]);
        let (records, _) = run(&mut fake, &mut stream, at + 10 * MS);
        let made = toyos_abi::input::RawKeyEvent { keycode: 0x04, modifiers: 0 };
        let broke = toyos_abi::input::RawKeyEvent { keycode: 0x04, modifiers: MOD_RELEASED };
        assert_eq!(records, [Record::Key(made), Record::Key(broke)]);
    }

    #[test]
    fn an_overrun_code_sends_lost() {
        let mut fake = Fake::qemu();
        let mut stream = armed(&mut fake);
        let at = fake.now + MS;
        fake.kbd_sends(at, &[0x00]);
        let (records, _) = run(&mut fake, &mut stream, at + 10 * MS);
        assert_eq!(records, [Record::Lost]);
    }

    /// `E0 2A` is the fake Shift around PrtScn, which maps to nothing: the run
    /// is named whole.
    #[test]
    fn an_undecoded_run_is_named_whole() {
        let mut fake = Fake::qemu();
        let mut stream = armed(&mut fake);
        let at = fake.now + MS;
        fake.kbd_sends(at, &[0xE0, 0x12]);
        let (records, said) = run(&mut fake, &mut stream, at + 10 * MS);
        assert!(records.is_empty(), "{records:?}");
        assert!(said.iter().any(|l| l == "ps2server: no event from [0xe0, 0x2a]"), "{said:#?}");
    }

    #[test]
    fn an_output_buffer_full_after_32_reads_ends_the_stream_by_name() {
        let mut fake = Fake::qemu();
        let mut stream = armed(&mut fake);
        fake.wedged = true;
        let mut said = Vec::new();
        let ended = stream.wake(&mut fake, None, &mut |line| said.push(line));
        assert!(ended.is_err(), "{ended:?}");
        assert!(said.iter().any(|l| l.contains("stayed full for 32 reads")), "{said:#?}");
    }

    /// The device resets itself, and a keystroke loads between the wake's
    /// last read and the enable's second byte: it reaches the keyboard's
    /// decoder, and the enable goes out after it.
    #[test]
    fn a_keystroke_found_waiting_before_the_reenable_reaches_the_keyboards_decoder() {
        let mut fake = Fake::qemu();
        let mut stream = armed(&mut fake);
        fake.kbd.gap = 3_000;
        let at = fake.now + MS;
        fake.aux_sends(at, &[0xAA, 0x00]);
        fake.kbd_sends(at + MS, &[0x1C, 0x1C]);
        let (records, said) = run(&mut fake, &mut stream, at + 30 * MS);
        let keys = records.iter().filter(|r| matches!(r, Record::Key(k) if k.keycode == 0x04)).count();
        assert_eq!(keys, 2, "{records:?}");
        assert!(said.iter().any(|l| l == "ps2server: aux reset itself, reporting enabled again"), "{said:#?}");
    }

    #[test]
    fn three_failed_reenables_write_the_pointer_off_with_0xa7() {
        let mut fake = Fake::qemu();
        let mut stream = armed(&mut fake);
        fake.aux.enables.extend([None, None, None]);
        let at = fake.now + MS;
        fake.aux_sends(at, &[0xAA, 0x00]);
        let before = fake.commands.len();
        let (records, said) = run(&mut fake, &mut stream, at + 200 * MS);
        assert_eq!(fake.commands[before..], [0xD4, 0xD4, 0xD4, 0xA7], "the enable thrice, then 0xA7 alone");
        assert_eq!(records, [Record::Lost], "the pointer's buttons released");
        assert!(said.iter().any(|l| l.starts_with("ps2server: aux enable failed 3 times — pointer written off")), "{said:#?}");
    }

    /// A reader 8 ms late to every interrupt, behind a TrackPoint held off
    /// while the buffer is full: the bytes arrive 9 ms apart, past
    /// `PACKET_GAP_NS`, and the device's own gaps are 1 ms.
    #[test]
    fn a_packet_survives_a_reader_later_than_the_packet_gap() {
        let mut fake = Fake::t14();
        let mut stream = armed(&mut fake);
        fake.latency = 8 * MS;
        let at = fake.now + MS;
        // Left held, 5 right and 3 up.
        fake.aux_sends(at, &[0x09, 0x05, 0x03]);
        let (records, _) = run(&mut fake, &mut stream, at + 100 * MS);
        let motion = Motion::Relative { dx: 5, dy: -3 };
        assert_eq!(records, [Record::Pointer(Report { source: 0, buttons: 1, scroll: 0, motion })]);
    }
}
