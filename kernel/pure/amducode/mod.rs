//! The AMD microcode scout's decisions: the patch block AMD's container holds
//! for one CPU ([`container`]), what the firmware's SMBIOS says it is
//! ([`smbios`]), and the measurement ladder that settles what AMD's documents
//! leave open about loading it on Family 17h ([`ladder`]).
//!
//! An instrument, not a loader: the kernel drives the ladder on real CPUs
//! only under the `amd-ucode-scout` actuator, and each reading is one log line
//! a metal row reads back.

#![forbid(unsafe_code)]

pub mod container;
pub mod ladder;
pub mod smbios;

#[cfg(test)]
mod tests;
