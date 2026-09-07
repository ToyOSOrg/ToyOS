//! Where an access into a bounded window lands, and whether it may land there.
//!
//! Two windows: the DMA region a driver reads and writes through, and the
//! register file a *process* names an offset in through a syscall. The
//! arithmetic is the same shape and gets wrong in the same ways, so it is asked
//! in one place — on the host, as a value — rather than survived in a guest.
//!
//! Three questions, and a caller gets them wrong in three different ways.
//!
//! **[`within`] — the length, not the offset.** The bound this replaces
//! (`KernelSlice::ptr_at`) asserted `offset <= size` and said nothing about what
//! was read through the pointer it returned, so a `u32` read at `size - 1` ran
//! three bytes past the region and passed. Every access here names the number of
//! bytes it will touch, and the addition is `checked_add`: an `offset + len` that
//! wraps is refused rather than compared after wrapping.
//!
//! **[`aligned`] — a volatile access is not a `memcpy`.** `read_volatile` and
//! `write_volatile` require the pointer to be naturally aligned for the type; on
//! x86-64 a misaligned one usually *works*, which is exactly why nothing catches
//! it. The unaligned discipline (`read_unaligned`/`write_unaligned`) has no such
//! requirement and does not ask this question at all.
//!
//! **[`register`] — the offset is the caller's.** `within` and `aligned` bound a
//! kernel driver's own arithmetic, which is a kernel bug when it is wrong.
//! `register` bounds a number that crossed the user/kernel boundary, which is
//! *never* a bug in this kernel: it answers a [`Register`] whose only
//! constructor this is, so an offset nobody checked cannot reach a device's
//! register file at all.
//!
//! Pure. No I/O, no allocation, no `unsafe`, nothing read from a device and
//! nothing named outside this crate.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

/// Why an access into a DMA region is refused.
///
/// A refusal here is a kernel bug and not a device one: every offset and length
/// that reaches it is the driver's own arithmetic, so `mm::dma` panics on one.
/// It is a value rather than a panic *here* so that the table below can ask the
/// question on the host instead of surviving the answer in a guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// `len` bytes at `offset` are not all inside a region of `size` bytes —
    /// including the case where `offset + len` does not fit a `usize`.
    Past { offset: usize, len: usize, size: usize },
    /// The address a `T` would be read or written at is not a multiple of
    /// `align`, which a volatile access requires.
    Unaligned { at: usize, align: usize },
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Past { offset, len, size } => write!(
                f,
                "{len} byte(s) at {offset:#x} run past a region of {size:#x}"
            ),
            Self::Unaligned { at, align } => {
                write!(f, "{at:#x} is not {align}-byte aligned")
            }
        }
    }
}

/// Whether `len` bytes at `offset` are all inside a region of `size` bytes.
///
/// `len` is the number of bytes the access will touch — `size_of::<T>()` for a
/// typed read or write, `src.len()` for a byte copy — and never zero-by-default:
/// the whole point is that the length is part of the question.
///
/// A zero-length access at `offset == size` is allowed. It touches nothing, and
/// the one-past-the-end address is the natural end of an empty subview.
#[inline]
pub fn within(offset: usize, len: usize, size: usize) -> Result<(), Refused> {
    match offset.checked_add(len) {
        Some(end) if end <= size => Ok(()),
        _ => Err(Refused::Past { offset, len, size }),
    }
}

/// Whether a value of alignment `align` at `base + offset` is naturally aligned.
///
/// `base` is the region's own address, because alignment is a property of the
/// address the access lands on and not of the offset alone — a region placed at
/// an odd address makes every even offset in it odd.
///
/// `align` is `align_of::<T>()`, which is a power of two for every `T`; the
/// mask below is that fact and not an assumption about the caller.
#[inline]
pub fn aligned(base: usize, offset: usize, align: usize) -> Result<(), Refused> {
    let at = base.wrapping_add(offset);
    if align != 0 && at & (align - 1) == 0 {
        Ok(())
    } else {
        Err(Refused::Unaligned { at, align })
    }
}

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

    /// The hole the drivers sweep closed by hand at 35 sites, asked directly:
    /// a four-byte read one byte before the end of the region.
    #[test]
    fn a_u32_at_size_minus_one_is_refused() {
        assert_eq!(
            within(0x1000 - 1, 4, 0x1000),
            Err(Refused::Past { offset: 0xFFF, len: 4, size: 0x1000 })
        );
    }

    /// And the access the old bound and the new one agree about: the last whole
    /// `T` in the region.
    #[test]
    fn a_u32_at_size_minus_four_is_allowed() {
        assert_eq!(within(0x1000 - 4, 4, 0x1000), Ok(()));
    }

    /// The offset alone is what `ptr_at` checked, and it is not the question:
    /// every one of these offsets is `<= size` and every one of these accesses
    /// runs past the end.
    #[test]
    fn an_offset_inside_the_region_does_not_make_the_access_inside_it() {
        for (len, offset) in [(2usize, 0xFFFusize), (4, 0xFFD), (8, 0xFF9), (256, 0xF01)] {
            assert!(offset <= 0x1000, "the offset itself is in range");
            assert!(
                within(offset, len, 0x1000).is_err(),
                "{len} bytes at {offset:#x} must be refused"
            );
        }
    }

    #[test]
    fn the_whole_region_is_inside_itself() {
        assert_eq!(within(0, 0x1000, 0x1000), Ok(()));
        assert_eq!(within(0, 0x1001, 0x1000), Err(Refused::Past {
            offset: 0,
            len: 0x1001,
            size: 0x1000,
        }));
    }

    /// An empty access at the end is not out of bounds; one past it is.
    #[test]
    fn a_zero_length_access_lands_at_the_end_and_no_further() {
        assert_eq!(within(0x1000, 0, 0x1000), Ok(()));
        assert!(within(0x1001, 0, 0x1000).is_err());
    }

    /// The addition is checked, not wrapped. Compared after wrapping, this pair
    /// is `Ok` — which is the shape that turns a length into an oracle for the
    /// whole address space.
    #[test]
    fn an_offset_plus_length_that_wraps_is_refused() {
        assert!(usize::MAX.wrapping_add(2) < 0x1000, "the wrapped sum looks in range");
        assert_eq!(
            within(usize::MAX, 2, 0x1000),
            Err(Refused::Past { offset: usize::MAX, len: 2, size: 0x1000 })
        );
    }

    /// A region of nothing holds nothing.
    #[test]
    fn an_empty_region_admits_only_an_empty_access() {
        assert_eq!(within(0, 0, 0), Ok(()));
        assert!(within(0, 1, 0).is_err());
    }

    #[test]
    fn a_natural_offset_from_a_natural_base_is_aligned() {
        // A page-aligned region, the placement every DMA structure in the tree
        // has, and the offsets the rings actually use.
        assert_eq!(aligned(0x2000, 0, 8), Ok(()));
        assert_eq!(aligned(0x2000, 16, 8), Ok(()));
        assert_eq!(aligned(0x2000, 4, 4), Ok(()));
        assert_eq!(aligned(0x2000, 1, 1), Ok(()));
    }

    #[test]
    fn an_unaligned_volatile_access_is_refused() {
        assert_eq!(aligned(0x2000, 1, 4), Err(Refused::Unaligned { at: 0x2001, align: 4 }));
        assert_eq!(aligned(0x2000, 2, 4), Err(Refused::Unaligned { at: 0x2002, align: 4 }));
        assert_eq!(aligned(0x2000, 4, 8), Err(Refused::Unaligned { at: 0x2004, align: 8 }));
    }

    /// Alignment is about the address and not about the offset: a region that is
    /// itself misplaced makes an offset that looks fine land wrong.
    #[test]
    fn the_base_is_part_of_the_question() {
        assert_eq!(aligned(0x2002, 0, 4), Err(Refused::Unaligned { at: 0x2002, align: 4 }));
        assert_eq!(aligned(0x2002, 2, 4), Ok(()));
    }

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

    /// The refusals are what a person reading a kernel panic gets, so they say
    /// the numbers rather than naming a variant.
    #[test]
    fn a_refusal_names_its_numbers() {
        extern crate std;
        use std::string::ToString;
        assert_eq!(
            Refused::Past { offset: 0xFFF, len: 4, size: 0x1000 }.to_string(),
            "4 byte(s) at 0xfff run past a region of 0x1000"
        );
        assert_eq!(
            Refused::Unaligned { at: 0x2001, align: 4 }.to_string(),
            "0x2001 is not 4-byte aligned"
        );
    }

    /// And the same for the three a caller reads back out of a syscall.
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
