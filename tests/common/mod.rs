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
/// The cable: netstack's address, and the T14 answering the host on it.
pub mod lan;
pub mod logstream;
pub mod metal;
pub mod power;
pub mod qemu;
pub mod screen;
pub mod serial;
pub mod ssh;
pub mod usb;
