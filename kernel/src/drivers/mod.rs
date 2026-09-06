//! Device drivers.
//! A `SAFETY:` comment here must justify why the block could not be removed, not just why it is sound.
//! `warn` suffices here because CI's clippy step already runs with `-D warnings`.
//! No driver may touch DMA memory except through [`crate::mm::Dma`], whose constructor is private to `mm::dma`.
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod serial;
pub mod acpi;
pub mod i8042;
pub mod ioapic;
pub mod pci;
pub mod nvme;
pub mod xhci;
pub mod usb_storage;
pub mod virtio;
pub mod virtio_console;
pub mod virtio_gpu;
pub mod virtio_net;
pub mod virtio_sound;
pub mod gop;
pub mod hda;
pub mod panic_console;
pub mod watchdog;

/// The pool every driver here allocates its DMA out of.
pub use crate::mm::DmaPool;
