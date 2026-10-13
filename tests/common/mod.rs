pub mod audio;
/// A claimed function at the unit, on the T14.
pub mod claims;
pub mod clock;
pub mod lane;
pub mod compile;
/// The device boot: what `tests/images/metaldevicecase.toml` measures.
pub mod devices;
pub mod faults;
pub mod hostedclang;
pub mod iommu;
pub mod irqcensus;
/// The `isa` claim's rows on the T14.
pub mod isa;
pub mod metal;
pub mod power;
/// What ps2server says of the i8042 it armed.
pub mod ps2server;
pub mod qemu;
pub mod screen;
pub mod serial;
pub mod usb;
