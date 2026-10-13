//! The controller's two ports, and the rules IBM puts on every access to
//! them.
//!
//! Every citation is IBM's *Personal System/2 Hardware Interface Technical
//! Reference — Common Interfaces* (84F9735), its "Keyboard and Auxiliary
//! Device Controller" section by its own page: **C**.
//!
//! - **A byte is read 7 µs after the status read that showed it** (C 14).
//! - **A byte is written only with the input and output buffers both empty**
//!   (C 14). What was waiting is handed to the caller, which decides whose it
//!   is.
//! - **Every bounded wait looks at its condition once more after its
//!   deadline**, so a preempted reader lengthens a wait and never turns a late
//!   answer into silence.

/// Status: the output buffer holds a byte for the system (C 7).
pub const OBF: u8 = 1 << 0;
/// Status: the input buffer holds a byte the controller has not taken.
pub const IBF: u8 = 1 << 1;
/// Status: the byte in the output buffer is the auxiliary device's.
pub const AUXB: u8 = 1 << 5;

/// What the status port reads where nothing decodes it.
pub const FLOATING: u8 = 0xFF;

/// Controller commands (C 11-13).
pub const READ_COMMAND_BYTE: u8 = 0x20;
pub const WRITE_COMMAND_BYTE: u8 = 0x60;
pub const DISABLE_AUX: u8 = 0xA7;
pub const ENABLE_AUX: u8 = 0xA8;
pub const DISABLE_KEYBOARD: u8 = 0xAD;
pub const ENABLE_KEYBOARD: u8 = 0xAE;
pub const WRITE_AUX: u8 = 0xD4;

/// The command byte (C 11).
pub const CB_KEYBOARD_IRQ: u8 = 1 << 0;
pub const CB_AUX_IRQ: u8 = 1 << 1;
/// Placed in the status register's System Flag; 0 after a power-on reset.
pub const CB_SYSTEM_FLAG: u8 = 1 << 2;
/// "Disable Keyboard": the keyboard interface's clock held low, as `0xAD`
/// sets it and `0xAE` clears it (C 12).
pub const CB_KEYBOARD_DISABLED: u8 = 1 << 4;
pub const CB_AUX_DISABLED: u8 = 1 << 5;
/// The keyboard's bytes translated to the PC-compatible codes (C 10).
pub const CB_TRANSLATE: u8 = 1 << 6;

/// C 14's wait between a status read showing a byte and the read of it.
pub const READ_DELAY_NS: u64 = 7_000;

/// The controller's ports and the clock, as ps2server reaches them: the
/// machine's through its `isa` claim (`crate::hw`), or a test's.
pub trait Controller {
    /// Port 0x64, read.
    fn status(&mut self) -> u8;
    /// Port 0x60, read.
    fn data(&mut self) -> u8;
    /// Port 0x64, written.
    fn command(&mut self, byte: u8);
    /// Port 0x60, written.
    fn write(&mut self, byte: u8);
    /// Nanoseconds on the clock the claim's records are stamped with.
    fn now(&mut self) -> u64;
    /// The claim's interrupts since it was last asked, waiting for one until
    /// `deadline`; `None` for none by then, looked for once more after it.
    fn wait(&mut self, deadline: u64) -> Option<Irq>;
}

/// One record of the claim's interrupts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Irq {
    /// When the first of them landed.
    pub first_nanos: u64,
}

/// A byte read off the output buffer, and whose it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Byte {
    pub value: u8,
    pub aux: bool,
}

/// Which port a write is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Port {
    Command,
    Data,
}

/// The controller never took a write: its input buffer stayed full, or its
/// output buffer kept filling, until the deadline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NotTaken {
    pub status: u8,
}

impl<C: Controller + ?Sized> Bus for C {}

/// C 14's rules over any [`Controller`].
pub trait Bus: Controller {
    /// The byte in the output buffer, if the status says there is one.
    fn poll(&mut self) -> Option<Byte> {
        let status = self.status();
        let shown = self.now();
        (status & OBF != 0).then(|| Byte { value: self.read_after(shown), aux: status & AUXB != 0 })
    }

    /// Port 0x60, `READ_DELAY_NS` after `shown`, when a status read showed it full.
    fn read_after(&mut self, shown: u64) -> u8 {
        while self.now() < shown + READ_DELAY_NS {
            core::hint::spin_loop();
        }
        self.data()
    }

    /// Write `byte` to `port` once both buffers are empty, handing `found`
    /// every byte that was waiting first.
    fn send(&mut self, port: Port, byte: u8, deadline: u64, found: &mut impl FnMut(Byte)) -> Result<(), NotTaken> {
        loop {
            let past = self.now() >= deadline;
            let status = self.status();
            let shown = self.now();
            if status & OBF != 0 {
                found(Byte { value: self.read_after(shown), aux: status & AUXB != 0 });
                if past {
                    return Err(NotTaken { status });
                }
                continue;
            }
            if status & IBF == 0 {
                match port {
                    Port::Command => self.command(byte),
                    Port::Data => self.write(byte),
                }
                return Ok(());
            }
            if past {
                return Err(NotTaken { status });
            }
            core::hint::spin_loop();
        }
    }
}
