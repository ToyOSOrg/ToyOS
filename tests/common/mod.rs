pub mod audio;
/// A claimed function at the unit, on the T14.
pub mod claims;
pub mod clock;
pub mod lane;
pub mod compile;
/// The device boot: what `tests/metaldevicecase` measures.
pub mod devices;
pub mod faults;
pub mod iommu;
pub mod irqcensus;
/// The `isa` claim's rows on the T14.
pub mod isa;
pub mod metal;
/// The T14 on its own cable: its router and the internet.
pub mod outbound;
pub mod power;
pub mod qemu;
pub mod screen;
pub mod serial;
pub mod usb;
