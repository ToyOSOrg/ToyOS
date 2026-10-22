//! Device drivers.
//! A `SAFETY:` comment here must justify why the block could not be removed, not just why it is sound.
//! `warn` suffices here because CI's clippy step already runs with `-D warnings`.
//! No driver may touch DMA memory except through [`crate::mm::Dma`], whose constructor is private to `mm::dma`.
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod serial;
pub mod serial_lock;
pub mod acpi;
pub mod pci;
pub mod virtio;
pub mod virtio_console;
pub mod gop;
pub mod panic_console;

/// The pool every driver here allocates its DMA out of.
pub use crate::mm::DmaPool;
