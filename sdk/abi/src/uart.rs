//! What a claim on the machine's console UART hands the process that drives
//! it.
//!
//! **The UART is the kernel's before userland runs and after it stops**: the
//! kernel puts the boot on it until the hand-over that comes before any
//! process can claim it, and writes it again from the machine's stop, once
//! userland has stopped, and on every fatal path. Between the two it is the
//! holder's, and the kernel writes nothing to it, so a holder that drives
//! the line it finds keeps the kernel's programming.
//!
//! **The holder reaches the registers as its architecture places them**
//! ([`UartInfo`]): a 16550's at the ports it may `in` and `out` from the
//! claim's first read on, a PL011's in the register frame that read installs.
//! Its interrupt is records on the claim, as an ISA function's are; a level
//! line is masked by the kernel each time it is taken and unmasked by the
//! holder's acknowledgement ([`ACK`]).

use crate::handle::RawHandle;

/// A 16550 at the port [`UartInfo::port`] names and the seven after it.
pub const NS16550: u32 = 1;
/// A PL011, or an SBSA generic UART, whose registers sit at the same offsets
/// (Arm PL011 TRM r1p5, §3.3; Arm BSA 1.0, Appendix B), in [`UartInfo::frame`].
pub const PL011: u32 = 2;

crate::user_safe! {
    /// The claim's description.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct UartInfo {
        /// [`NS16550`] or [`PL011`].
        pub kind: u32,
        /// A 16550's first port; 0 for a PL011.
        pub port: u32,
        /// A PL011's register frame, to map: carries `MAP` and `WRITE` and
        /// neither `DUP` nor `TRANSFER`. Invalid for a 16550.
        pub frame: RawHandle,
        /// The frame's length in bytes; 0 for a 16550.
        pub frame_len: u32,
        /// Nonzero where its line is level-triggered: each record is
        /// acknowledged ([`ACK`]) once what raised it is served.
        pub level: u32,
    }
}

/// The word a holder writes to its claim to have a level line unmasked.
pub const ACK: u32 = 1;
