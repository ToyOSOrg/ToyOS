//! [`Phys`]: a physical address aligned to `1 << ALIGN` bytes and below 2^48,
//! so a register or descriptor field that takes an address's bits `[47:ALIGN]`
//! holds every bit of one.
//!
//! `no_std`, no allocation, no `unsafe`.

#![no_std]
#![forbid(unsafe_code)]

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Phys<const ALIGN: u32>(u64);

impl<const ALIGN: u32> Phys<ALIGN> {
    const FIELD: u64 = (1 << 48) - (1 << ALIGN);

    pub const fn new(address: u64) -> Option<Self> {
        if address & !Self::FIELD == 0 {
            Some(Self(address))
        } else {
            None
        }
    }

    /// The address a word holds in its bits `[47:ALIGN]`, whatever its other
    /// bits are.
    pub const fn of(word: u64) -> Self {
        Self(word & Self::FIELD)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}
