//! The embedded controller's transaction protocol (ACPI 6.5 §12.2, §12.3),
//! with no I/O: a [`Transaction`] is told what the status register reads and
//! answers what its host does next, so how long a host waits, and how, is the
//! host's.
//!
//! A transaction is a command byte written to the command register and the one
//! byte read back, the write once the controller's input buffer is empty and
//! the read once its output buffer is full. Stage 1 asks only for queries;
//! reads and writes of the controller's space join when AML reaches it.

/// The status register's bits (Table 12.3).
pub const OBF: u8 = 1 << 0;
pub const IBF: u8 = 1 << 1;
/// An event is waiting to be queried.
pub const SCI_EVT: u8 = 1 << 5;

/// Table 12.6: query, which answers the number of the event waiting, or 0
/// for none.
const QR_EC: u8 = 0x84;

/// What the host does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Do {
    /// Read the status register again, until it says otherwise.
    Wait(Wait),
    WriteCommand(u8),
    /// Read the data register and hand the byte to [`Transaction::read`].
    ReadData,
    /// Finished, with the byte read back.
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
    Command,
    Answer,
    Done(u8),
}

/// One transaction, from its command to its answer.
#[derive(Debug)]
pub struct Transaction {
    command: u8,
    phase: Phase,
}

impl Transaction {
    /// Ask for the waiting event's number.
    pub fn query() -> Self {
        Self { command: QR_EC, phase: Phase::Command }
    }

    /// What to do, given what the status register reads now.
    pub fn step(&mut self, status: u8) -> Do {
        match self.phase {
            Phase::Command if status & IBF != 0 => Do::Wait(Wait::InputEmpty),
            Phase::Command => {
                self.phase = Phase::Answer;
                Do::WriteCommand(self.command)
            }
            Phase::Answer if status & OBF == 0 => Do::Wait(Wait::OutputFull),
            Phase::Answer => Do::ReadData,
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
mod tests {
    use super::*;

    /// A controller as Table 12.3 has it: the host's write fills the input
    /// buffer, the controller takes it and later fills the output buffer.
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
                Do::Wait(_) | Do::WriteCommand(_) => {}
            }
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

    #[test]
    #[should_panic(expected = "a byte read that nothing asked for")]
    fn a_byte_before_the_command_is_refused() {
        Transaction::query().read(0);
    }
}
