//! `smmu-selftest`: QEMU's `iommu-testdev` (1b36:0005), whose one register
//! read makes a DMA write of a known word at a device address and reads it
//! back at a physical one. Two of them, asked four things of the armed unit:
//!
//! 1. the last one writes on the entry its stream starts with, and is refused
//!    with nothing recorded;
//! 2. on a domain of its own, it writes at an address the domain maps, and
//!    lands at the memory mapped there and nowhere else;
//! 3. once the domain takes that address back, it writes there again, and is
//!    refused and recorded;
//! 4. the other, which [`super::init`] gives no route, as it gives none to a
//!    function it never enumerated, writes under a StreamID inside the stream
//!    table, and is refused and recorded.
//!
//! The last two are made with this CPU's interrupts masked, so one drain reads
//! both: the event interrupt reaches the handler, which names each and, their
//! functions being no process's, halts the machine. Each step says what it
//! saw on its own line; the harness judges them, and the records by the
//! handler's, whose count says the first step recorded nothing.

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

/// How long the two events are given to reach this CPU once it unmasks.
const EVENT_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(1),
    "the unit records each event and pulses its interrupt while the refused write is still being made",
);

/// A test device below another: [`super::init`] routes nothing for it, and
/// the stream table, sized to the last one's StreamID, holds its entry.
pub(super) fn stands_unrouted(device: &PciDevice, devices: &[PciDevice]) -> bool {
    let at = |d: &PciDevice| (d.bus, d.dev, d.func);
    device.is_id(VENDOR, DEVICE) && devices.iter().any(|d| d.is_id(VENDOR, DEVICE) && at(d) > at(device))
}

/// `device`'s registers, its memory decode on. No Bus Master Enable: the
/// device writes through the unit's address space whatever its `COMMAND` says.
fn registers(device: &PciDevice) -> Mmio {
    let bar = device.memory_bar(0).expect("smmu-selftest: an iommu-testdev's BAR0 is unassigned").address();
    let regs = crate::mm::paging::map_mmio(bar, BAR_BYTES, MmioPolicy::Uncacheable);
    device.enable_memory_space();
    regs
}

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

fn verdict(result: u32, want: u32) -> &'static str {
    match (result == want, want) {
        (true, REFUSED) => "refused",
        (true, _) => "landed there",
        (false, _) => "FAIL",
    }
}

pub(super) fn run(devices: &[PciDevice]) {
    let mut testdevs = devices.iter().filter(|d| d.is_id(VENDOR, DEVICE));
    let (Some(stray), Some(device), None) = (testdevs.next(), testdevs.next(), testdevs.next()) else {
        panic!("smmu-selftest: this machine has not two iommu-testdevs (1b36:0005)");
    };
    let (regs, stray_regs) = (registers(device), registers(stray));
    let page = crate::mm::pmm::alloc_page().expect("smmu-selftest: no page to aim at");
    let phys = page.direct_map().phys();
    let function = crate::iommu::StreamId::pci(device.bus, device.dev, device.func);
    let unrouted = crate::iommu::StreamId::pci(stray.bus, stray.dev, stray.func);

    let aborted = write(regs, phys, phys);
    log!(
        "smmu-selftest: {function}'s write at {phys:#x}, on the entry its stream starts with, answered {aborted:#x}: {}",
        verdict(aborted, REFUSED)
    );

    let (space, at) = OwnSpace::create(PAGE_2M).expect("smmu-selftest: a domain on the armed unit");
    space.place(at, phys, PAGE_2M).expect("smmu-selftest: the domain's own first leaf");
    space.attach(device.bus, device.dev, device.func);
    let landed = write(regs, at + 0x40, phys + 0x40);
    log!(
        "smmu-selftest: {function}'s write at {:#x}, mapped to {:#x}, answered {landed:#x}: {}",
        at + 0x40,
        phys + 0x40,
        verdict(landed, LANDED)
    );
    space.unmap(at, PAGE_2M).expect("smmu-selftest: the leaf it placed");

    let before = super::fault::recorded();
    let masked = crate::arch::IrqGuard::close();
    let taken_back = write(regs, at + 0x40, phys + 0x40);
    let stray_wrote = write(stray_regs, phys, phys);
    log!(
        "smmu-selftest: {function}'s write at {:#x} again, which its domain no longer maps, answered {taken_back:#x}: {}",
        at + 0x40,
        verdict(taken_back, REFUSED)
    );
    log!(
        "smmu-selftest: {unrouted}'s write at {phys:#x}, under no route, answered {stray_wrote:#x}: {}",
        verdict(stray_wrote, REFUSED)
    );
    drop(masked);
    let deadline = crate::clock::nanos_since_boot() + EVENT_TIMEOUT.nanos();
    while super::fault::recorded() < before + 2 {
        assert!(
            crate::clock::nanos_since_boot() < deadline,
            "smmu-selftest: FAIL: {} of the two refused writes' events reached this CPU within {EVENT_TIMEOUT}",
            super::fault::recorded() - before
        );
        core::hint::spin_loop();
    }
    panic!("smmu-selftest: FAIL: the handler read the events of functions no process drives and the machine went on");
}
