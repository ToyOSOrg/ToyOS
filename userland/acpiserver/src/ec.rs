//! The embedded controller's transaction protocol (ACPI 6.5 §12.2, §12.3),
//! with no I/O: a [`Transaction`] is told what the status register reads and
//! answers what its host does next, so how long a host waits, and how, is the
//! host's.
//!
//! A transaction is a command byte written to the command register, then the
//! bytes its command takes written to the data register, each once the
//! controller's input buffer is empty; then a read of the one byte it answers
//! once its output buffer is full, or, where it answers none, a wait until
//! the controller has taken the last byte written.

/// The status register's bits (Table 12.3).
pub const OBF: u8 = 1 << 0;
pub const IBF: u8 = 1 << 1;
/// An event is waiting to be queried.
pub const SCI_EVT: u8 = 1 << 5;

/// Table 12.6: read a byte of the controller's space (§12.3.1).
const RD_EC: u8 = 0x80;
/// Table 12.6: write a byte of the controller's space (§12.3.2).
const WR_EC: u8 = 0x81;
/// Table 12.6: query, which answers the number of the event waiting, or 0
/// for none (§12.3.5).
const QR_EC: u8 = 0x84;

/// What the host does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Do {
    /// Read the status register again, until it says otherwise.
    Wait(Wait),
    WriteCommand(u8),
    WriteData(u8),
    /// Read the data register and hand the byte to [`Transaction::read`].
    ReadData,
    /// Finished, with the byte read back, or the byte written.
    Done(u8),
}

/// What a [`Do::Wait`] waits for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wait {
    InputEmpty,
    OutputFull,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Writing the byte at this index of `bytes`.
    Send(usize),
    Answer,
    /// The last byte is written, and the controller has not taken it yet.
    Settle,
    Done(u8),
}

/// One transaction, from its command to its answer.
#[derive(Debug)]
pub struct Transaction {
    /// The command, then what it takes.
    bytes: [u8; 3],
    len: usize,
    answers: bool,
    phase: Phase,
}

impl Transaction {
    fn new(sent: &[u8], answers: bool) -> Self {
        let mut bytes = [0; 3];
        bytes[..sent.len()].copy_from_slice(sent);
        Self { bytes, len: sent.len(), answers, phase: Phase::Send(0) }
    }

    /// Ask for the waiting event's number.
    pub fn query() -> Self {
        Self::new(&[QR_EC], true)
    }

    /// The byte at `address` of the controller's space.
    pub fn read_at(address: u8) -> Self {
        Self::new(&[RD_EC, address], true)
    }

    /// `value` to `address` of the controller's space.
    pub fn write_at(address: u8, value: u8) -> Self {
        Self::new(&[WR_EC, address, value], false)
    }

    /// What to do, given what the status register reads now.
    pub fn step(&mut self, status: u8) -> Do {
        match self.phase {
            Phase::Send(_) | Phase::Settle if status & IBF != 0 => Do::Wait(Wait::InputEmpty),
            Phase::Send(i) => {
                self.phase = match i + 1 {
                    next if next < self.len => Phase::Send(next),
                    _ if self.answers => Phase::Answer,
                    _ => Phase::Settle,
                };
                if i == 0 { Do::WriteCommand(self.bytes[0]) } else { Do::WriteData(self.bytes[i]) }
            }
            Phase::Answer if status & OBF == 0 => Do::Wait(Wait::OutputFull),
            Phase::Answer => Do::ReadData,
            Phase::Settle => {
                let written = self.bytes[self.len - 1];
                self.phase = Phase::Done(written);
                Do::Done(written)
            }
            Phase::Done(byte) => Do::Done(byte),
        }
    }

    /// The byte a [`Do::ReadData`] read.
    pub fn read(&mut self, byte: u8) {
        assert_eq!(self.phase, Phase::Answer, "ec: a byte read that nothing asked for");
        self.phase = Phase::Done(byte);
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A host that runs `tx` against a controller saying `statuses` in turn
    /// and answering `answer` to a read of its data register.
    fn run(mut tx: Transaction, statuses: &[u8], answer: u8) -> (Vec<Do>, u8) {
        let mut said = Vec::new();
        let mut statuses = statuses.iter().copied();
        loop {
            let status = statuses.next().expect("the controller said enough");
            let next = tx.step(status);
            said.push(next);
            match next {
                Do::ReadData => tx.read(answer),
                Do::Done(byte) => return (said, byte),
                Do::Wait(_) | Do::WriteCommand(_) | Do::WriteData(_) => {}
            }
        }
    }

    /// A controller as §12.2 and §12.3 have it: 256 bytes of space; a byte
    /// written fills the input buffer until the controller takes it, which
    /// it does `slow` status reads later; a command's answer fills the output
    /// buffer as it takes the command's last byte. It refuses, by panic,
    /// a write while its input buffer is full, a read of its data register
    /// while its output buffer is empty, and a data byte no command asked for.
    #[derive(Debug)]
    pub struct Emulated {
        pub space: [u8; 256],
        pub slow: u32,
        pending: u32,
        input: bool,
        output: Option<u8>,
        /// The command taken, and the bytes it has taken after it.
        taken: Vec<u8>,
        pub queries: Vec<u8>,
        /// Every command taken whole: `(command, address, value)`.
        pub done: Vec<(u8, u8, u8)>,
    }

    impl Emulated {
        pub fn new(space: [u8; 256]) -> Self {
            Emulated { space, slow: 2, pending: 0, input: false, output: None, taken: Vec::new(), queries: Vec::new(), done: Vec::new() }
        }

        pub fn status(&mut self) -> u8 {
            if self.input {
                if self.pending == 0 {
                    self.input = false;
                    self.take();
                } else {
                    self.pending -= 1;
                }
            }
            u8::from(self.output.is_some()) * OBF | u8::from(self.input) * IBF
        }

        fn take(&mut self) {
            let wants = match self.taken[0] {
                RD_EC => 2,
                WR_EC => 3,
                QR_EC => 1,
                other => panic!("the controller was sent command {other:#04x}"),
            };
            if self.taken.len() < wants {
                return;
            }
            let (command, address, value) = (self.taken[0], self.taken.get(1).copied().unwrap_or(0), self.taken.get(2).copied().unwrap_or(0));
            match command {
                RD_EC => self.output = Some(self.space[usize::from(address)]),
                WR_EC => self.space[usize::from(address)] = value,
                _ => self.output = Some(self.queries.pop().unwrap_or(0)),
            }
            self.done.push((command, address, value));
            self.taken.clear();
        }

        fn written(&mut self, byte: u8) {
            assert!(!self.input, "a byte was written while the controller's input buffer was full");
            self.input = true;
            self.pending = self.slow;
            self.taken.push(byte);
        }

        pub fn command(&mut self, byte: u8) {
            assert!(self.taken.is_empty(), "a command was written inside another's transaction");
            self.written(byte);
        }

        pub fn data(&mut self, byte: u8) {
            assert!(!self.taken.is_empty(), "a data byte was written that no command asked for");
            self.written(byte);
        }

        pub fn read(&mut self) -> u8 {
            self.output.take().expect("the data register was read with the output buffer empty")
        }

        /// One transaction, driven as the server's host drives it.
        pub fn transact(&mut self, mut tx: Transaction) -> u8 {
            for _ in 0..1000 {
                let status = self.status();
                match tx.step(status) {
                    Do::Wait(_) => {}
                    Do::WriteCommand(byte) => self.command(byte),
                    Do::WriteData(byte) => self.data(byte),
                    Do::ReadData => tx.read(self.read()),
                    Do::Done(byte) => return byte,
                }
            }
            panic!("the transaction never ended: {tx:?} against {self:?}");
        }
    }

    #[test]
    fn a_query_writes_its_command_once_the_input_is_empty_and_reads_once_the_output_is_full() {
        let (said, byte) = run(Transaction::query(), &[IBF, IBF, 0, IBF, 0, OBF, 0], 0x1c);
        assert_eq!(
            said,
            [
                Do::Wait(Wait::InputEmpty),
                Do::Wait(Wait::InputEmpty),
                Do::WriteCommand(0x84),
                Do::Wait(Wait::OutputFull),
                Do::Wait(Wait::OutputFull),
                Do::ReadData,
                Do::Done(0x1c),
            ]
        );
        assert_eq!(byte, 0x1c);
    }

    /// §12.3.1: RD_EC, then the address once the command is taken, then the
    /// byte once the output buffer is full.
    #[test]
    fn a_read_writes_its_address_only_once_the_command_is_taken() {
        let (said, byte) = run(Transaction::read_at(0xA8), &[IBF, 0, IBF, IBF, 0, 0, OBF, 0], 0x5A);
        assert_eq!(
            said,
            [
                Do::Wait(Wait::InputEmpty),
                Do::WriteCommand(0x80),
                Do::Wait(Wait::InputEmpty),
                Do::Wait(Wait::InputEmpty),
                Do::WriteData(0xA8),
                Do::Wait(Wait::OutputFull),
                Do::ReadData,
                Do::Done(0x5A),
            ]
        );
        assert_eq!(byte, 0x5A);
    }

    /// §12.3.2: WR_EC, the address, the value, each once the input buffer is
    /// empty, and done only once the controller has taken the value.
    #[test]
    fn a_write_ends_once_the_controller_has_taken_its_value() {
        let (said, byte) = run(Transaction::write_at(0x81, 0x01), &[0, IBF, 0, 0, IBF, IBF, OBF], 0);
        assert_eq!(
            said,
            [
                Do::WriteCommand(0x81),
                Do::Wait(Wait::InputEmpty),
                Do::WriteData(0x81),
                Do::WriteData(0x01),
                Do::Wait(Wait::InputEmpty),
                Do::Wait(Wait::InputEmpty),
                Do::Done(0x01),
            ],
            "a full output buffer is no answer a write waits for"
        );
        assert_eq!(byte, 0x01);
    }

    #[test]
    fn every_transaction_runs_against_a_controller_that_takes_each_byte_late() {
        let mut space = [0; 256];
        space[0xA0] = 0x34;
        for slow in [0, 1, 5] {
            let mut ec = Emulated::new(space);
            ec.slow = slow;
            assert_eq!(ec.transact(Transaction::read_at(0xA0)), 0x34);
            assert_eq!(ec.transact(Transaction::write_at(0x81, 0x02)), 0x02);
            assert_eq!(ec.space[0x81], 0x02);
            ec.queries.push(0x1C);
            assert_eq!(ec.transact(Transaction::query()), 0x1C);
            assert_eq!(ec.transact(Transaction::query()), 0, "no event waiting");
            assert_eq!(ec.done, [(0x80, 0xA0, 0), (0x81, 0x81, 0x02), (0x84, 0, 0), (0x84, 0, 0)]);
        }
    }

    #[test]
    #[should_panic(expected = "a byte read that nothing asked for")]
    fn a_byte_before_the_command_is_refused() {
        Transaction::query().read(0);
    }

    #[test]
    #[should_panic(expected = "a byte read that nothing asked for")]
    fn a_write_reads_no_byte() {
        let mut tx = Transaction::write_at(0, 0);
        for _ in 0..3 {
            tx.step(0);
        }
        tx.read(0);
    }
}
