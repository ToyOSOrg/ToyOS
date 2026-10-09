//! Every word an SMMUv3 and its driver exchange, as Arm IHI 0070 issue H.a
//! lays it out: what the identification registers say a unit can do
//! ([`unit`]), the stream table entry and context descriptor that put a
//! stream under stage 1 translation or abort it ([`config`]), the commands and
//! the event records ([`queue`]), and the stage 1 tables a context descriptor
//! names ([`table`]).
//!
//! The crate encodes one configuration and no other: a stream either aborts
//! or is translated by stage 1 alone through one context descriptor, under
//! the Non-secure EL1 regime, with 4 KiB-granule VMSAv8-64 tables over a
//! 48-bit input. No stream bypasses, and nothing here spells a bypass.
//!
//! What a unit reports about itself is a device's word and is refused by name
//! where the driver could not act on it; nothing here panics on a register's
//! or a record's value.
//!
//! `no_std`, no allocation, no `unsafe`.

#![no_std]
#![forbid(unsafe_code)]

pub mod config;
pub mod queue;
pub mod table;
pub mod unit;

/// A physical address the unit is given, aligned to `1 << ALIGN` bytes and
/// below 2^48: the widest address a 4 KiB-granule VMSAv8-64 descriptor
/// carries without `DS`, and inside every address field written here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Phys<const ALIGN: u32>(u64);

impl<const ALIGN: u32> Phys<ALIGN> {
    pub const fn new(address: u64) -> Option<Self> {
        if address >> 48 == 0 && address & ((1 << ALIGN) - 1) == 0 {
            Some(Self(address))
        } else {
            None
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A context's tag in the unit's TLB, no wider than the unit's ASIDs: made by
/// [`unit::Unit::asid`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Asid(u16);

impl Asid {
    pub const fn get(self) -> u16 {
        self.0
    }
}
