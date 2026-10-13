//! A scripted i8042, its keyboard and its auxiliary device, for the probe's
//! and the stream's tests: each shape is a machine's as IBM's references and
//! the benches' readings give it, and every IBM rule the controller states is
//! a panic here when ps2server breaks it.
//!
//! - **C 14**: a data read sooner than 7 µs after the status read that showed
//!   the byte; a write while the input or the output buffer is full.
//! - **C 9**: a controller command C does not list. The commands this fake
//!   models are the ones ps2server's design sends; one C lists and no shape
//!   models is refused too, by its own message.
//! - **C 12**: `0x20` written while the keyboard interface is enabled, where
//!   its answer could be a keystroke.
//! - **C 11**: a device's answer read before the interrupt it raised was taken.
//!
//! The keyboard interface is command-byte bit 4, which `0xAD` sets, `0xAE`
//! clears and `0x60` writes (C 11, C 12); while it is set the keyboard holds
//! what it would send (K88 39).

use std::collections::VecDeque;

use crate::bus::{Controller, Irq, AUXB, IBF, OBF, READ_DELAY_NS};

const MS: u64 = 1_000_000;
const US: u64 = 1_000;

/// How long the controller takes a written byte.
const TAKES: u64 = 5 * US;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Src {
    Kbd,
    Aux,
    Controller,
}

#[derive(Clone, Copy, Debug)]
struct Loaded {
    value: u8,
    src: Src,
    /// An answer to a command, which is read only after its interrupt.
    answer: bool,
    raised: bool,
    taken: bool,
}

#[derive(Clone, Copy, Debug)]
struct Queued {
    ready: u64,
    value: u8,
    answer: bool,
}

/// What a device does with its reset: `0xFA` this long after the command,
/// then `then` this long after the `0xFA`.
#[derive(Clone, Copy, Debug)]
pub struct Reset {
    pub ack: Option<u64>,
    pub then: Option<(u64, u8)>,
}

impl Reset {
    pub const fn answered(ack: u64, then: u64) -> Self {
        Self { ack: Some(ack), then: Some((then, 0xAA)) }
    }
}

pub struct Kbd {
    /// The scan code set it sends in.
    pub set: u8,
    pub scanning: bool,
    pub reset: Reset,
    /// Its next byte loads at least this long after the read that freed the
    /// buffer: the EC's queue.
    pub gap: u64,
    queue: VecDeque<Queued>,
    /// `a` pressed at each time.
    typed: VecDeque<u64>,
}

pub struct Aux {
    pub reset: Option<Reset>,
    /// The id after its `0xAA`.
    pub id: u8,
    /// Its next byte loads this long after the read that freed the buffer: a
    /// device held off while the buffer is full (TP 50).
    pub gap: u64,
    /// What each enable is answered, oldest first; `0xFA` once these run out.
    pub enables: VecDeque<Option<u8>>,
    reporting: bool,
    queue: VecDeque<Queued>,
    /// Packets, and self-resets, sent at each time.
    sends: VecDeque<(u64, Vec<u8>)>,
}

#[derive(Clone, Copy)]
enum Arg {
    CommandByte,
    Aux,
}

pub struct Fake {
    pub now: u64,
    pub cb: u8,
    pub kbd: Kbd,
    pub aux: Aux,
    /// Nothing decodes the ports.
    pub absent: bool,
    /// The controller drops a `0x60` whose byte is this one.
    pub drops_command_byte: Option<u8>,
    /// The output buffer is full again the moment it is read.
    pub wedged: bool,
    /// The keyboard's bytes are translated whatever bit 6 says.
    pub translates_always: bool,
    /// Added to every wait that an interrupt ends: a reader that runs late.
    pub latency: u64,
    /// Every controller command written, in order.
    pub commands: Vec<u8>,
    /// When each keyboard byte loaded, and when the keyboard interface was
    /// last enabled.
    pub kbd_loads: Vec<(u64, u8)>,
    pub kbd_enabled_at: u64,
    obf: Option<Loaded>,
    shown: Option<u64>,
    freed: u64,
    ibf_until: u64,
    arg: Option<Arg>,
    controller: Option<(u64, u8)>,
    irqs: Vec<u64>,
    /// The translation saw `0xF0`.
    released: bool,
}

impl Fake {
    /// A controller as firmware hands it over, with `cb` its command byte.
    pub fn new(cb: u8, kbd_reset: Reset, aux_reset: Option<Reset>) -> Self {
        Self {
            now: 0,
            cb,
            kbd: Kbd { set: 2, scanning: true, reset: kbd_reset, gap: 0, queue: VecDeque::new(), typed: VecDeque::new() },
            aux: Aux {
                reset: aux_reset,
                id: 0,
                gap: 0,
                enables: VecDeque::new(),
                reporting: false,
                queue: VecDeque::new(),
                sends: VecDeque::new(),
            },
            absent: false,
            drops_command_byte: None,
            wedged: false,
            translates_always: false,
            latency: 0,
            commands: Vec::new(),
            kbd_loads: Vec::new(),
            kbd_enabled_at: 0,
            obf: None,
            shown: None,
            freed: 0,
            ibf_until: 0,
            arg: None,
            controller: None,
            irqs: Vec::new(),
            released: false,
        }
    }

    /// QEMU's: the keyboard and a PS/2 mouse each answer a reset at once.
    pub fn qemu() -> Self {
        Self::new(0x47, Reset::answered(50 * US, 300 * US), Some(Reset::answered(50 * US, 300 * US)))
    }

    /// The T14's, as M1 and M2 read it: the keyboard's `0xFA` 80 µs after its
    /// reset and its `0xAA` 516 ms after that, its EC queueing keyboard bytes;
    /// the TrackPoint's `0xFA` at 2.5 ms and `0xAA` 367 ms later, held off
    /// while the buffer is full.
    pub fn t14() -> Self {
        let mut fake = Self::new(0x47, Reset::answered(80 * US, 516 * MS), Some(Reset::answered(2_500 * US, 367 * MS)));
        fake.kbd.gap = 30 * US;
        fake.aux.gap = MS;
        fake
    }

    /// The AMD bench's: the keyboard's `0xFA` 581 µs after its reset and its
    /// `0xAA` 2.2 ms after that; no aux answer; its EC translating whatever
    /// bit 6 says.
    pub fn amd() -> Self {
        let mut fake = Self::new(0x47, Reset::answered(581 * US, 2_200 * US), None);
        fake.translates_always = true;
        fake
    }

    /// Press `a` at `at`.
    pub fn type_a(&mut self, at: u64) {
        self.kbd.typed.push_back(at);
    }

    /// The keyboard sends `bytes`, each when the last has been read, from `at`.
    pub fn kbd_sends(&mut self, at: u64, bytes: &[u8]) {
        for &value in bytes {
            self.kbd.queue.push_back(Queued { ready: at, value, answer: false });
        }
    }

    /// The aux device sends `bytes`, a millisecond apart, from `at`.
    pub fn aux_sends(&mut self, at: u64, bytes: &[u8]) {
        self.aux.sends.push_back((at, bytes.to_vec()));
    }

    fn kbd_enabled(&self) -> bool {
        self.cb & 0x10 == 0
    }

    fn aux_enabled(&self) -> bool {
        self.cb & 0x20 == 0
    }

    /// Run the machine up to `to`.
    fn advance(&mut self, to: u64) {
        while let Some(&at) = self.kbd.typed.front().filter(|&&at| at <= to) {
            self.kbd.typed.pop_front();
            if self.kbd.scanning {
                let make = if self.kbd.set == 2 { 0x1C } else { 0x1E };
                self.kbd.queue.push_back(Queued { ready: at, value: make, answer: false });
            }
        }
        while let Some((at, _)) = self.aux.sends.front().filter(|(at, _)| *at <= to) {
            let at = *at;
            let (_, bytes) = self.aux.sends.pop_front().expect("a send was just seen");
            if bytes == [0xAA, 0x00] {
                self.aux.reporting = false;
            } else if !self.aux.reporting {
                continue;
            }
            for (i, &value) in bytes.iter().enumerate() {
                self.aux.queue.push_back(Queued { ready: at + i as u64 * MS, value, answer: false });
            }
        }
        loop {
            if self.wedged && self.obf.is_none() {
                self.obf = Some(Loaded { value: 0x55, src: Src::Kbd, answer: false, raised: false, taken: false });
            }
            if self.obf.is_some() {
                return;
            }
            let controller = self.controller.map(|(ready, _)| ready);
            let kbd = self.kbd.queue.front().filter(|_| self.kbd_enabled()).map(|q| q.ready.max(self.freed + self.kbd.gap));
            let aux = self.aux.queue.front().filter(|_| self.aux_enabled()).map(|q| q.ready.max(self.freed + self.aux.gap));
            let next = [controller, kbd, aux].into_iter().flatten().min();
            let Some(at) = next.filter(|&at| at <= to) else { return };
            let (src, value, answer) = if Some(at) == controller {
                let (_, value) = self.controller.take().expect("a controller answer was just seen");
                (Src::Controller, value, false)
            } else if Some(at) == kbd {
                let q = self.kbd.queue.pop_front().expect("a keyboard byte was just seen");
                (Src::Kbd, q.value, q.answer)
            } else {
                let q = self.aux.queue.pop_front().expect("an aux byte was just seen");
                (Src::Aux, q.value, q.answer)
            };
            let value = match src {
                Src::Kbd if self.cb & 0x40 != 0 || self.translates_always => match self.translate(value) {
                    Some(value) => value,
                    None => continue,
                },
                _ => value,
            };
            if src == Src::Kbd {
                self.kbd_loads.push((at, value));
            }
            let raised = match src {
                Src::Kbd | Src::Controller => self.cb & 0x01 != 0,
                Src::Aux => self.cb & 0x02 != 0,
            };
            if raised {
                self.irqs.push(at);
            }
            self.obf = Some(Loaded { value, src, answer, raised, taken: false });
        }
    }

    /// The controller's translation (C 5), for the bytes the shapes send:
    /// `None` for the `0xF0` that makes the next byte a release.
    fn translate(&mut self, value: u8) -> Option<u8> {
        let released = std::mem::take(&mut self.released);
        let translated = match value {
            0xF0 => {
                self.released = true;
                return None;
            }
            0x00 | 0xAA | 0xE0 | 0xEE | 0xFA | 0xFC | 0xFE => return Some(value),
            // Set 2's `a`, and set 2's `2`, which is set 1's `a` read as set 2.
            0x1C => 0x1E,
            0x1E => 0x03,
            // Set 2's left Shift.
            0x12 => 0x2A,
            other => panic!("fake: no translation of {other:#04x} is modelled"),
        };
        Some(if released { translated | 0x80 } else { translated })
    }

    fn written(&mut self, what: &str) {
        self.advance(self.now);
        assert!(self.obf.is_none(), "C 14: {what} written with the output buffer full");
        assert!(self.now >= self.ibf_until, "C 14: {what} written with the input buffer full");
        self.ibf_until = self.now + TAKES;
    }

    fn keyboard_command(&mut self, byte: u8) {
        assert_eq!(byte, 0xFF, "fake: the keyboard was sent {byte:#04x}, and no shape models it");
        let Reset { ack, then } = self.kbd.reset;
        self.kbd.queue.clear();
        self.kbd.scanning = false;
        let Some(ack) = ack else { return };
        let acked = self.now + ack;
        self.kbd.queue.push_back(Queued { ready: acked, value: 0xFA, answer: true });
        if let Some((after, value)) = then {
            self.kbd.queue.push_back(Queued { ready: acked + after, value, answer: true });
            if value == 0xAA {
                self.kbd.set = 2;
                self.kbd.scanning = true;
            }
        }
    }

    fn aux_command(&mut self, byte: u8) {
        let at = self.now;
        match byte {
            0xFF => {
                self.aux.reporting = false;
                self.aux.queue.clear();
                let Some(Reset { ack: Some(ack), then }) = self.aux.reset else { return };
                self.aux.queue.push_back(Queued { ready: at + ack, value: 0xFA, answer: true });
                if let Some((after, value)) = then {
                    self.aux.queue.push_back(Queued { ready: at + ack + after, value, answer: true });
                    self.aux.queue.push_back(Queued { ready: at + ack + after + 100 * US, value: self.aux.id, answer: true });
                }
            }
            0xF4 => {
                if self.aux.reset.is_none() {
                    return;
                }
                match self.aux.enables.pop_front().unwrap_or(Some(0xFA)) {
                    Some(0xFA) => {
                        self.aux.reporting = true;
                        self.aux.queue.push_back(Queued { ready: at + 100 * US, value: 0xFA, answer: true });
                    }
                    Some(other) => self.aux.queue.push_back(Queued { ready: at + 100 * US, value: other, answer: true }),
                    None => {}
                }
            }
            other => panic!("fake: the aux device was sent {other:#04x}, and no shape models it"),
        }
    }
}

impl Controller for Fake {
    fn status(&mut self) -> u8 {
        if self.absent {
            return 0xFF;
        }
        self.advance(self.now);
        let mut status = self.cb & 0x04;
        if let Some(loaded) = self.obf {
            status |= OBF;
            if loaded.src == Src::Aux {
                status |= AUXB;
            }
            self.shown = Some(self.now);
        }
        if self.now < self.ibf_until {
            status |= IBF;
        }
        status
    }

    fn data(&mut self) -> u8 {
        let loaded = self.obf.take().expect("fake: a data read with the output buffer empty");
        let shown = self.shown.take().expect("fake: a data read no status read showed");
        assert!(
            self.now >= shown + READ_DELAY_NS,
            "C 14: a data read {} ns after the status read that showed it",
            self.now - shown
        );
        assert!(!loaded.answer || loaded.taken, "C 11: the answer {:#04x} was read before its interrupt", loaded.value);
        self.freed = self.now;
        loaded.value
    }

    fn command(&mut self, byte: u8) {
        self.written(&format!("the command {byte:#04x}"));
        self.commands.push(byte);
        match byte {
            0x20 => {
                assert!(!self.kbd_enabled(), "C 12: 0x20 written while the keyboard interface is enabled");
                self.controller = Some((self.now + 20 * US, self.cb));
            }
            0x60 => self.arg = Some(Arg::CommandByte),
            0xA7 => self.cb |= 0x20,
            0xA8 => self.cb &= !0x20,
            0xAD => self.cb |= 0x10,
            0xAE => {
                self.cb &= !0x10;
                self.kbd_enabled_at = self.now;
            }
            0xD4 => self.arg = Some(Arg::Aux),
            0xA9 | 0xC0 | 0xD0 | 0xD1 | 0xE0 | 0xF0..=0xFF => {
                panic!("fake: the controller command {byte:#04x} is one C lists, and no shape models it")
            }
            other => panic!("C 9: the controller command {other:#04x} is reserved"),
        }
    }

    fn write(&mut self, byte: u8) {
        self.written(&format!("the data byte {byte:#04x}"));
        match self.arg.take() {
            Some(Arg::CommandByte) if self.drops_command_byte == Some(byte) => {}
            Some(Arg::CommandByte) => {
                if self.cb & 0x10 != 0 && byte & 0x10 == 0 {
                    self.kbd_enabled_at = self.now;
                }
                self.cb = byte;
            }
            Some(Arg::Aux) => self.aux_command(byte),
            None => self.keyboard_command(byte),
        }
    }

    fn now(&mut self) -> u64 {
        self.now += US;
        self.now
    }

    fn wait(&mut self, deadline: u64) -> Option<Irq> {
        loop {
            self.advance(self.now);
            if let Some(&first) = self.irqs.iter().min() {
                self.irqs.clear();
                if let Some(loaded) = self.obf.as_mut().filter(|l| l.raised) {
                    loaded.taken = true;
                }
                self.now += self.latency;
                return Some(Irq { first_nanos: first });
            }
            let next = self.next_event().filter(|&at| at <= deadline);
            match next {
                Some(at) => self.now = self.now.max(at),
                None if self.now >= deadline => return None,
                None => self.now = deadline,
            }
        }
    }
}

impl Fake {
    /// When the machine next changes on its own, with nothing written.
    fn next_event(&self) -> Option<u64> {
        if self.obf.is_some() {
            return None;
        }
        let kbd = self.kbd.queue.front().filter(|_| self.kbd_enabled()).map(|q| q.ready.max(self.freed + self.kbd.gap));
        let aux = self.aux.queue.front().filter(|_| self.aux_enabled()).map(|q| q.ready.max(self.freed + self.aux.gap));
        let typed = self.kbd.typed.front().copied();
        let sends = self.aux.sends.front().map(|(at, _)| *at);
        [self.controller.map(|(at, _)| at), kbd, aux, typed, sends].into_iter().flatten().min()
    }
}
