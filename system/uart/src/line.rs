//! The line between the log and the UART: what is owed the transmitter, what
//! the receiver has, and which interrupt the UART is asked for, decided over
//! any [`Uart`] — the machine's (`crate::hw`) or a test's model.
//!
//! **The transmitter's interrupt is asked for only while bytes are owed it and
//! it has refused one.** A 16550 raises THRE once its FIFO is empty, and a
//! PL011 its TX interrupt as its FIFO falls through the trigger level (PL011
//! TRM r1p5, "Interrupts"), so a transmitter left full always raises one; one
//! asked for with nothing owed would raise for ever.
//!
//! **A service ends with the UART raising nothing it was asked for**, or says
//! it does not: a 16550's line, an ISA edge, rises again only once every cause
//! behind it is served, so a cause left standing would never be raised again.

use std::collections::VecDeque;

/// The UART's registers, as the line reaches them.
pub trait Uart {
    /// How many bytes the transmitter takes now, without waiting.
    fn room(&mut self) -> usize;
    /// One byte into the transmitter; only within [`Self::room`].
    fn send(&mut self, byte: u8);
    /// A byte the receiver holds, if one does.
    fn receive(&mut self) -> Option<u8>;
    /// Ask for the transmitter's interrupt, or stop asking; the receiver's is
    /// always asked for.
    fn ask_tx(&mut self, on: bool);
    /// Whether a cause of an interrupt it was asked for still stands.
    fn raising(&mut self) -> bool;
}

/// The most rounds one service makes before it hands back: a receiver that
/// never empties is a host typing faster than the UART's own rate allows.
const ROUNDS: usize = 64;

#[derive(Default)]
pub struct Line {
    owed: VecDeque<u8>,
    /// Bytes put into the transmitter since the line began.
    sent: u64,
    asking: bool,
}

impl Line {
    /// Owe the transmitter `bytes`, after what it is owed already.
    pub fn owe(&mut self, bytes: &[u8]) {
        self.owed.extend(bytes);
    }

    /// Whether the transmitter is still owed bytes.
    pub fn owes(&self) -> bool {
        !self.owed.is_empty()
    }

    /// Bytes the transmitter has taken since the line began.
    pub fn sent(&self) -> u64 {
        self.sent
    }

    /// Serve `uart`: what it received onto `received`, what it is owed into
    /// its transmitter while it takes it, and its transmitter's interrupt
    /// asked for exactly while it is owed bytes it refused. Whether a cause
    /// still stands past [`ROUNDS`], which the caller serves again before it
    /// waits.
    pub fn serve(&mut self, uart: &mut impl Uart, received: &mut Vec<u8>) -> bool {
        for _ in 0..ROUNDS {
            while let Some(byte) = uart.receive() {
                received.push(byte);
            }
            while !self.owed.is_empty() {
                let room = uart.room();
                if room == 0 {
                    break;
                }
                for byte in self.owed.drain(..room.min(self.owed.len())) {
                    uart.send(byte);
                    self.sent += 1;
                }
            }
            let asking = !self.owed.is_empty();
            if asking != self.asking {
                uart.ask_tx(asking);
                self.asking = asking;
            }
            if !uart.raising() {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16550 in FIFO mode (PC16550D): sixteen bytes of transmit FIFO,
    /// `THRE` once it is empty, its interrupt standing while asked for and
    /// `THRE` set, and the receiver's while a byte waits.
    #[derive(Default)]
    struct Ns16550 {
        fifo: VecDeque<u8>,
        wire: Vec<u8>,
        rx: VecDeque<u8>,
        thre_asked: bool,
    }

    impl Ns16550 {
        /// The transmitter shifts out `n` bytes.
        fn shift(&mut self, n: usize) {
            for _ in 0..n.min(self.fifo.len()) {
                let byte = self.fifo.pop_front().expect("bounded by its length");
                self.wire.push(byte);
            }
        }
    }

    impl Uart for Ns16550 {
        fn room(&mut self) -> usize {
            if self.fifo.is_empty() { 16 } else { 0 }
        }

        fn send(&mut self, byte: u8) {
            assert!(self.fifo.len() < 16, "a byte past the FIFO's sixteen");
            self.fifo.push_back(byte);
        }

        fn receive(&mut self) -> Option<u8> {
            self.rx.pop_front()
        }

        fn ask_tx(&mut self, on: bool) {
            self.thre_asked = on;
        }

        fn raising(&mut self) -> bool {
            !self.rx.is_empty() || (self.thre_asked && self.fifo.is_empty())
        }
    }

    /// Every byte owed reaches the wire in order, through a FIFO of sixteen,
    /// and the transmitter's interrupt is asked for only while it is owed.
    #[test]
    fn what_is_owed_goes_out_in_order_and_the_interrupt_ends_with_it() {
        let mut uart = Ns16550::default();
        let mut line = Line::default();
        let text: Vec<u8> = (0..100u8).collect();
        line.owe(&text);
        let mut received = Vec::new();
        assert!(!line.serve(&mut uart, &mut received));
        assert_eq!(uart.fifo.len(), 16);
        assert!(uart.thre_asked, "owed bytes and a full FIFO ask for THRE");
        while line.owes() {
            uart.shift(16);
            assert!(uart.raising(), "an emptied FIFO raises THRE while asked for");
            assert!(!line.serve(&mut uart, &mut received));
        }
        uart.shift(16);
        assert_eq!(uart.wire, text);
        assert_eq!(line.sent(), 100);
        assert!(!uart.thre_asked, "nothing owed asks for nothing");
        assert!(!uart.raising(), "an empty FIFO with nothing owed raises nothing");
    }

    /// What the receiver holds is handed on whatever the transmitter does.
    #[test]
    fn what_is_received_is_handed_on() {
        let mut uart = Ns16550 { rx: b"run x\n".iter().copied().collect(), ..Default::default() };
        let mut line = Line::default();
        let mut received = Vec::new();
        assert!(!line.serve(&mut uart, &mut received));
        assert_eq!(received, b"run x\n");
        assert!(!uart.thre_asked);
    }

    /// A receiver that never empties is served [`ROUNDS`] times and handed
    /// back standing, so the caller serves it again before it waits.
    #[test]
    fn a_cause_that_stands_past_the_rounds_is_said() {
        struct Flood;
        impl Uart for Flood {
            fn room(&mut self) -> usize {
                16
            }
            fn send(&mut self, _: u8) {}
            fn receive(&mut self) -> Option<u8> {
                None
            }
            fn ask_tx(&mut self, _: bool) {}
            fn raising(&mut self) -> bool {
                true
            }
        }
        assert!(Line::default().serve(&mut Flood, &mut Vec::new()));
    }

    /// A PL011's transmitter takes one byte per look while `TXFF` is clear;
    /// the line looks again for each.
    #[test]
    fn a_transmitter_that_takes_one_at_a_time_is_filled_one_at_a_time() {
        #[derive(Default)]
        struct Pl011 {
            fifo: Vec<u8>,
        }
        impl Uart for Pl011 {
            fn room(&mut self) -> usize {
                usize::from(self.fifo.len() < 32)
            }
            fn send(&mut self, byte: u8) {
                self.fifo.push(byte);
            }
            fn receive(&mut self) -> Option<u8> {
                None
            }
            fn ask_tx(&mut self, _: bool) {}
            fn raising(&mut self) -> bool {
                false
            }
        }
        let mut uart = Pl011::default();
        let mut line = Line::default();
        line.owe(&[7; 40]);
        line.serve(&mut uart, &mut Vec::new());
        assert_eq!(uart.fifo.len(), 32);
        assert!(line.owes());
    }
}
