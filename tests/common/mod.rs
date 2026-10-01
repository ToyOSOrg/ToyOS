pub mod audio;
pub mod clock;
pub mod lane;
pub mod compile;
/// The device boot: what `tests/metaldevicecase` measures.
pub mod devices;
pub mod faults;
pub mod iommu;
pub mod irqcensus;
/// The cable: netd's address, and the T14 answering the host on it.
pub mod lan;
pub mod logstream;
pub mod metal;
pub mod power;
pub mod qemu;
pub mod screen;
pub mod serial;
pub mod ssh;
pub mod steal;
pub mod usb;
pub mod volumes;
