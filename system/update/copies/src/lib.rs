//! update's install on the host, over partitions in memory: a signed image
//! written into the idle slot and marked, ROOT byte for byte and the volume
//! judged by `toyos-fat32-check`, and an image another key signed writing
//! nothing.

#[cfg(test)]
#[path = "../../src/install/mod.rs"]
pub mod install;

#[cfg(test)]
mod installing;
