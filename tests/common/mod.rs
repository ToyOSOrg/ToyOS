pub mod audio;
/// blockd: the NVMe driver in userland, judged off its disk and the device's
/// own trace.
pub mod blockd;
/// The C toolchain, end to end: a program the toolchain's clang built, judged
/// as the loader reads it and then run.
pub mod clang;
pub mod clock;
pub mod lane;
pub mod compile;
pub mod console;
/// The device boot: what `tests/metaldevicecase` measures.
pub mod devices;
pub mod faults;
pub mod fwvars;
pub mod gpt;
pub mod https;
pub mod iommu;
pub mod inspect;
pub mod irqcensus;
/// The cable: netd's address, and the T14 answering the host on it.
pub mod lan;
pub mod logread;
pub mod logstream;
pub mod metal;
pub mod origin;
pub mod orphan;
pub mod partclaim;
pub mod pkg;
pub mod power;
pub mod qemu;
pub mod screen;
/// The host as a neighbour on a guest's own Ethernet segment.
pub mod segment;
pub mod serial;
pub mod ssh;
pub mod storage;
pub mod swap;
pub mod update;
pub mod usb;
pub mod volumes;
pub mod wallclock;
