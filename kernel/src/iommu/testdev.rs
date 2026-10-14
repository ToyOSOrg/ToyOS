//! QEMU's `iommu-testdev` (1b36:0005), whose one register read makes a DMA
//! write of a known word at a device address and reads it back at a physical
//! one: the only DMA a guest of this suite can aim at an address of its
//! choosing, for each backend's selftest.

use crate::drivers::pci::PciDevice;
use crate::mm::policy::MmioPolicy;
use crate::mm::Mmio;

/// `hw/misc/iommu-testdev.h`: Red Hat's vendor and its test device.
pub const VENDOR: u16 = 0x1b36;
pub const DEVICE: u16 = 0x0005;

/// Its BAR0: reading `TRIGGERING` runs an armed DMA; `GVA` is the device
/// address written, `GPA` the physical one read back, `LEN` the bytes,
/// `DBELL` bit 0 arms, and `RESULT` says how it went.
const TRIGGERING: u64 = 0x00;
const GVA_LO: u64 = 0x04;
const GVA_HI: u64 = 0x08;
const LEN: u64 = 0x0c;
const RESULT: u64 = 0x10;
const DBELL: u64 = 0x14;
const GPA_LO: u64 = 0x1c;
const GPA_HI: u64 = 0x20;
const BAR_BYTES: u64 = 0x1000;

/// `RESULT`: the word landed at `GPA`; the write was refused.
pub const LANDED: u32 = 0;
pub const REFUSED: u32 = 0xdead_0002;

/// `device`'s registers, its memory decode on. No Bus Master Enable: the
/// device writes through the unit's address space whatever its `COMMAND` says.
pub fn registers(device: &PciDevice) -> Mmio {
    let bar = device.memory_bar(0).expect("iommu selftest: an iommu-testdev's BAR0 is unassigned").address();
    let regs = crate::mm::paging::map_mmio(bar, BAR_BYTES, MmioPolicy::Uncacheable);
    device.enable_memory_space();
    regs
}

/// The device's write of four bytes at `at`, read back at `phys`: its `RESULT`.
pub fn write(regs: Mmio, at: u64, phys: u64) -> u32 {
    regs.write_u32(GVA_LO, at as u32);
    regs.write_u32(GVA_HI, (at >> 32) as u32);
    regs.write_u32(GPA_LO, phys as u32);
    regs.write_u32(GPA_HI, (phys >> 32) as u32);
    regs.write_u32(LEN, 4);
    regs.write_u32(DBELL, 1);
    regs.read_u32(TRIGGERING);
    regs.read_u32(RESULT)
}

pub fn verdict(result: u32, want: u32) -> &'static str {
    match (result == want, want) {
        (true, REFUSED) => "refused",
        (true, _) => "landed there",
        (false, _) => "FAIL",
    }
}
