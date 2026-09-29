#[allow(dead_code)]
pub mod audio;
/// blockd: the NVMe driver in userland, judged off its disk and the device's
/// own trace.
#[allow(dead_code)]
pub mod blockd;
/// The C toolchain, end to end: a program the toolchain's clang built, judged
/// as the loader reads it and then run.
#[allow(dead_code)]
pub mod clang;
#[allow(dead_code)]
pub mod clock;
#[allow(dead_code)]
pub mod lane;
#[allow(dead_code)]
pub mod compile;
#[allow(dead_code)]
pub mod console;
/// The device boot: what `tests/metaldevicecase` measures, and its two judges.
#[allow(dead_code)]
pub mod devices;
#[allow(dead_code)]
pub mod faults;
pub mod fwvars;
#[allow(dead_code)]
pub mod gpt;
#[allow(dead_code)]
pub mod https;
#[allow(dead_code)]
pub mod iommu;
#[allow(dead_code)]
pub mod inspect;
#[allow(dead_code)]
pub mod irqcensus;
/// The cable: netd's address, and the T14 answering the host on it.
#[allow(dead_code)]
pub mod lan;
#[allow(dead_code)]
pub mod logread;
#[allow(dead_code)]
pub mod logstream;
pub mod metal;
#[allow(dead_code)]
pub mod origin;
pub mod orphan;
#[allow(dead_code)]
pub mod partclaim;
#[allow(dead_code)]
pub mod pkg;
#[allow(dead_code)]
pub mod power;
#[allow(dead_code)]
pub mod qemu;
#[allow(dead_code)]
pub mod screen;
/// The host as a neighbour on a guest's own Ethernet segment.
#[allow(dead_code)]
pub mod segment;
#[allow(dead_code)]
pub mod serial;
#[allow(dead_code)]
pub mod ssh;
#[allow(dead_code)]
pub mod storage;
pub mod swap;
#[allow(dead_code)]
pub mod toybox;
pub mod update;
#[allow(dead_code)]
pub mod usb;
#[allow(dead_code)]
pub mod volumes;
#[allow(dead_code)]
pub mod wallclock;
