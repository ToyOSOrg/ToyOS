//! Device drivers.
//! A `SAFETY:` comment here must justify why the block could not be removed, not just why it is sound.
//! `warn` suffices here because CI's clippy step already runs with `-D warnings`.
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod serial;
pub mod serial_lock;
pub mod acpi;
pub mod pci;
pub mod gop;
pub mod panic_console;
