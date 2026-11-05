//! bcachefs on-disk storage and ToyOS's explicit formatting designation.

#![cfg_attr(not(feature = "std"), no_std)]
extern crate alloc;
mod crc32c;
mod designation;
pub mod engine;
pub use designation::{designated, designation};
pub use engine::carries_volume;
