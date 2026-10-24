//! Whether a register access a *process* named lands inside the window it
//! has to: the offset crossed the user/kernel boundary, so a bad one is never
//! a bug in this kernel. [`register`] answers a [`Register`] whose only
//! constructor it is, so an offset nobody checked cannot reach a device's
//! register file at all. Asked on the host, as a value, rather than survived
//! in a guest.
//!
//! Pure. No I/O, no allocation, no `unsafe`, nothing read from a device and
//! nothing named outside this crate.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

/// One register access a caller named, already checked against the window it
/// has to land in.
///
/// **The offset came from userland**, so there is no public field and no
/// constructor but [`register`]: a kernel register read or write that takes one
/// of these cannot be reached with an offset nobody asked the question about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Register {
    offset: u64,
    bytes: u64,
}

impl Register {
    pub const fn offset(self) -> u64 {
        self.offset
    }

    pub const fn bytes(self) -> u64 {
        self.bytes
    }
}

/// Why a register access is refused. A caller's bad argument and never a
/// kernel bug, so every one of these is a value the syscall answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedRegister {
    /// An access of no width names no register.
    Widthless { offset: u64 },
    /// Not a multiple of its own width. A register file is addressed by whole
    /// registers, and a straddling access reads half of two of them.
    Unaligned { offset: u64, bytes: u64 },
    /// `bytes` at `offset` are not all inside a window of `window` bytes —
    /// including the case where `offset + bytes` does not fit a `u64`, which is
    /// the one a comparison made after wrapping lets through.
    Past { offset: u64, bytes: u64, window: u64 },
}

impl fmt::Display for RefusedRegister {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Widthless { offset } => {
                write!(f, "an access of no width at {offset:#x} names no register")
            }
            Self::Unaligned { offset, bytes } => {
                write!(f, "{offset:#x} is not a multiple of the {bytes} byte(s) read there")
            }
            Self::Past { offset, bytes, window } => write!(
                f,
                "{bytes} byte(s) at {offset:#x} run past a register window of {window:#x}"
            ),
        }
    }
}

/// Whether `bytes` at `offset` name a whole register inside a window of
/// `window` bytes, and the witness that says so.
///
/// All three questions in one answer, because a caller that has the witness has
/// had all three asked: the width is not zero, the offset is a multiple of it,
/// and the access ends inside the window. The addition is `checked_add`, so an
/// `offset` near `u64::MAX` is refused rather than compared after wrapping —
/// that is the shape that turns a caller-chosen number into a panic in a kernel
/// built with `overflow-checks`.
#[inline]
pub fn register(offset: u64, bytes: u64, window: u64) -> Result<Register, RefusedRegister> {
    if bytes == 0 {
        return Err(RefusedRegister::Widthless { offset });
    }
    if !offset.is_multiple_of(bytes) {
        return Err(RefusedRegister::Unaligned { offset, bytes });
    }
    match offset.checked_add(bytes) {
        Some(end) if end <= window => Ok(Register { offset, bytes }),
        _ => Err(RefusedRegister::Past { offset, bytes, window }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dword at the last dword of a 4 KiB register file, and the same dword
    /// one register further on.
    #[test]
    fn the_last_whole_register_is_inside_the_window() {
        assert_eq!(register(0xFFC, 4, 0x1000), Ok(Register { offset: 0xFFC, bytes: 4 }));
        assert_eq!(
            register(0x1000, 4, 0x1000),
            Err(RefusedRegister::Past { offset: 0x1000, bytes: 4, window: 0x1000 })
        );
    }

    /// **The one that panics a kernel built with `overflow-checks`.** Every
    /// pair here has `offset + bytes` wrapping to a small number, which a
    /// comparison made after the addition reads as in range — and which the
    /// addition itself does not survive.
    #[test]
    fn an_offset_that_wraps_its_own_width_is_refused() {
        for (offset, bytes) in [(u64::MAX, 1u64), (u64::MAX - 1, 2), (0xFFFF_FFFF_FFFF_FFFC, 4)] {
            assert_eq!(
                register(offset, bytes, 0x1000),
                Err(RefusedRegister::Past { offset, bytes, window: 0x1000 }),
                "{bytes} byte(s) at {offset:#x} must be refused"
            );
        }
    }

    /// A register file is addressed by whole registers: half of one and half of
    /// the next is not an access this answers.
    #[test]
    fn an_access_that_straddles_two_registers_is_refused() {
        assert_eq!(
            register(1, 2, 0x1000),
            Err(RefusedRegister::Unaligned { offset: 1, bytes: 2 })
        );
        assert_eq!(
            register(2, 4, 0x1000),
            Err(RefusedRegister::Unaligned { offset: 2, bytes: 4 })
        );
        assert_eq!(register(2, 2, 0x1000), Ok(Register { offset: 2, bytes: 2 }));
        assert_eq!(register(0xFFF, 1, 0x1000), Ok(Register { offset: 0xFFF, bytes: 1 }));
    }

    /// Asked before the modulo, which would divide by zero, and before the
    /// addition, which would admit every offset in the space.
    #[test]
    fn an_access_of_no_width_is_refused() {
        assert_eq!(register(0, 0, 0x1000), Err(RefusedRegister::Widthless { offset: 0 }));
        assert_eq!(
            register(u64::MAX, 0, 0x1000),
            Err(RefusedRegister::Widthless { offset: u64::MAX })
        );
    }

    /// A window of nothing holds no register.
    #[test]
    fn an_empty_window_admits_no_register() {
        assert_eq!(
            register(0, 1, 0),
            Err(RefusedRegister::Past { offset: 0, bytes: 1, window: 0 })
        );
    }

    /// The refusals are what a caller reads back out of a syscall, so they
    /// say the numbers rather than naming a variant.
    #[test]
    fn a_refused_register_names_its_numbers() {
        extern crate std;
        use std::string::ToString;
        assert_eq!(
            RefusedRegister::Widthless { offset: 0x10 }.to_string(),
            "an access of no width at 0x10 names no register"
        );
        assert_eq!(
            RefusedRegister::Unaligned { offset: 1, bytes: 2 }.to_string(),
            "0x1 is not a multiple of the 2 byte(s) read there"
        );
        assert_eq!(
            RefusedRegister::Past { offset: 0x1000, bytes: 4, window: 0x1000 }.to_string(),
            "4 byte(s) at 0x1000 run past a register window of 0x1000"
        );
    }
}
