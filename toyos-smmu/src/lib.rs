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
//! 48-bit input. No stream bypasses: [`config::Ste`] is made only as an
//! abort or as that translation, and holds its words where no other crate
//! can write them.
//!
//! What a unit reports about itself is a device's word and is refused by name
//! where the driver could not act on it; nothing here panics on a register's
//! or a record's value. An address is a [`toyos_phys::Phys`], and whatever
//! takes one for the unit to read answers `None` past the unit's output size.
//!
//! `no_std`, no allocation, no `unsafe`.

#![no_std]
#![forbid(unsafe_code)]

pub mod config;
pub mod queue;
pub mod table;
pub mod unit;

/// A context's tag in the unit's TLB, no wider than the unit's ASIDs: made by
/// [`unit::Unit::asid`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Asid(u16);

impl Asid {
    pub const fn get(self) -> u16 {
        self.0
    }
}
