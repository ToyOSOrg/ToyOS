//! The Intel AX200's and AX210's firmware files and the messages their
//! firmware exchanges. Of a file, only what a loader parses is read: its
//! licence forbids reverse engineering.
//!
//! Every layout is the AX2xx interface specification's, revision 2, on
//! `ToyOSOrg/evidence` branch `pr-876` (`spec/ax2xx.md`): a `§` here is one of
//! its sections. What the firmware writes is refused by name when it breaks a
//! layout; the pinned files are trusted, and refused by name all the same.

#![no_std]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::as_conversions
    )
)]

extern crate alloc;

mod bytes;
pub mod command;
pub mod container;
pub mod errors;
pub mod notification;
pub mod part;
pub mod pnvm;
pub mod versions;

#[cfg(test)]
mod tests;
