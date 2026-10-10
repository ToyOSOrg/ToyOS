//! `smmu-selftest`: QEMU's `iommu-testdev` (1b36:0005), whose one register
//! read makes a DMA write of a known word at a device address and reads it
//! back at a physical one, asked three things of the armed unit:
//!
//! 1. its write, on the entry every stream starts with, is refused;
//! 2. on a domain of its own, a write at an address the domain maps lands at
//!    the memory mapped there and nowhere else;
//! 3. a write at an address the domain does not map is refused and recorded:
//!    the event interrupt reaches the handler, which names it and, the
//!    function being no process's, halts the machine.
//!
//! Each says what it saw on its own line; the harness judges them, and the
//! third by the handler's.

use crate::drivers::pci::PciDevice;
use crate::iommu::OwnSpace;
use crate::log;
use crate::mm::policy::MmioPolicy;
use crate::mm::{Mmio, PAGE_2M};
use crate::time::{Duration, Tripwire};

/// `hw/misc/iommu-testdev.h`: Red Hat's vendor and its test device.
const VENDOR: u16 = 0x1b36;
const DEVICE: u16 = 0x0005;

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
const LANDED: u32 = 0;
const REFUSED: u32 = 0xdead_0002;

/// How long the event is given to reach this CPU once the write is refused.
const EVENT_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(1),
    "the unit records the event and pulses its interrupt while the refused write is still being made",
);

/// The device's write of four bytes at `at`, read back at `phys`: its `RESULT`.
fn write(regs: Mmio, at: u64, phys: u64) -> u32 {
    regs.write_u32(GVA_LO, at as u32);
    regs.write_u32(GVA_HI, (at >> 32) as u32);
    regs.write_u32(GPA_LO, phys as u32);
    regs.write_u32(GPA_HI, (phys >> 32) as u32);
    regs.write_u32(LEN, 4);
    regs.write_u32(DBELL, 1);
    regs.read_u32(TRIGGERING);
    regs.read_u32(RESULT)
}

pub(super) fn run(devices: &[PciDevice]) {
    let device = devices
        .iter()
        .find(|d| d.is_id(VENDOR, DEVICE))
        .expect("smmu-selftest: no iommu-testdev (1b36:0005) on this machine");
    let bar = device.memory_bar(0).expect("smmu-selftest: the iommu-testdev's BAR0 is unassigned").address();
    let regs = crate::mm::paging::map_mmio(bar, BAR_BYTES, MmioPolicy::Uncacheable);
    // No Bus Master Enable: the device writes through the unit's address
    // space whatever its `COMMAND` says.
    device.enable_memory_space();
    let page = crate::mm::pmm::alloc_page().expect("smmu-selftest: no page to aim at");
    let phys = page.direct_map().phys();
    let function = crate::iommu::StreamId::pci(device.bus, device.dev, device.func);

    let aborted = write(regs, phys, phys);
    log!(
        "smmu-selftest: {function}'s write at {phys:#x}, on the entry every stream starts with, answered {aborted:#x}: {}",
        if aborted == REFUSED { "refused" } else { "FAIL" }
    );

    let (space, at) = OwnSpace::create(PAGE_2M).expect("smmu-selftest: a domain on the armed unit");
    space.place(at, phys, PAGE_2M).expect("smmu-selftest: the domain's own first leaf");
    space.attach(device.bus, device.dev, device.func);
    let landed = write(regs, at + 0x40, phys + 0x40);
    log!(
        "smmu-selftest: {function}'s write at {:#x}, mapped to {:#x}, answered {landed:#x}: {}",
        at + 0x40,
        phys + 0x40,
        if landed == LANDED { "landed there" } else { "FAIL" }
    );

    let unmapped = at + PAGE_2M + 0x40;
    log!("smmu-selftest: {function} writes at {unmapped:#x}, which its domain does not map");
    let before = super::fault::recorded();
    let refused = write(regs, unmapped, phys + 0x40);
    let deadline = crate::clock::nanos_since_boot() + EVENT_TIMEOUT.nanos();
    while super::fault::recorded() == before {
        assert!(
            crate::clock::nanos_since_boot() < deadline,
            "smmu-selftest: FAIL: the write at {unmapped:#x} answered {refused:#x}, and no event reached this CPU within {EVENT_TIMEOUT}"
        );
        core::hint::spin_loop();
    }
    panic!("smmu-selftest: FAIL: the handler read the event of a function no process drives and the machine went on");
}
