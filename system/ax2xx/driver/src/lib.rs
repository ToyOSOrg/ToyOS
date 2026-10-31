//! The Intel AX200's and AX210's firmware, as the `ax2xx` driver meets it: the
//! files it loads and the messages it exchanges once they run.
//!
//! [`container::Firmware::parse`] reads a firmware file's container, and
//! [`pnvm::Pnvm::parse`] the AX210's platform-NVM file; of either, only what a
//! loader parses, as the files' licence forbids reverse engineering. [`part`]
//! is which radio a function is and whether its file serves it.
//! [`versions::Versions::of`] holds every message the driver exchanges to the
//! versions a file declares, refusing a file that declares one at a version no
//! layout here is written for; [`command`] encodes what the driver sends and
//! [`notification`] decodes what the firmware answers.
//!
//! Every layout is the AX2xx interface specification's, revision 1, on
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
pub mod notification;
pub mod part;
pub mod pnvm;
pub mod versions;

#[cfg(test)]
mod tests;
