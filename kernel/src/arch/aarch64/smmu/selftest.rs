//! `smmu-selftest`: QEMU's `iommu-testdev` (1b36:0005), whose one register
//! read makes a DMA write of a known word at a device address and reads it
//! back at a physical one. Two of them, asked four things of the armed unit:
//!
//! 1. the last one writes on the entry its stream starts with, and is refused
//!    with nothing recorded;
//! 2. on a domain of its own, it writes at an address the domain maps, and
//!    lands at the memory mapped there and nowhere else;
//! 3. the other, which [`super::init`] gives no route, as it gives none to a
//!    function it never enumerated, writes three times under a StreamID
//!    inside the stream table, and is refused and counted three times, the
//!    third record unwritten, and the machine goes on;
//! 4. once the domain takes its address back, the first writes there again,
//!    and is refused and recorded, and the machine halts: the function is
//!    one this kernel drives.
//!
//! The writes of 3, and the write of 4, are each made with this CPU's
//! interrupts masked, so one drain reads all of 3's and 4's verdict is on
//! the console before its event is taken, on this CPU, which the event SPI is
//! routed to. Each step says what it saw on its own line; the harness judges
//! them, and the records by the handler's, whose count says the first step
//! recorded nothing.

use crate::drivers::pci::PciDevice;
use crate::iommu::testdev::{registers, verdict, write, DEVICE, LANDED, REFUSED, VENDOR};
use crate::iommu::OwnSpace;
use crate::log;
use crate::mm::PAGE_2M;
use crate::time::{Duration, Tripwire};

/// How long a refused write's events are given to reach this CPU once it
/// unmasks.
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

    // Before any declaration, so clear of no window: QEMU routes nothing on
    // its root bus peer to peer, so the device's write reaches the unit.
    let (space, at) =
        OwnSpace::create(PAGE_2M, &[], device.bus, device.dev, device.func).expect("smmu-selftest: a domain on the armed unit");
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
    let stray_wrote = [(); 3].map(|()| write(stray_regs, phys, phys));
    for (i, answer) in stray_wrote.into_iter().enumerate() {
        log!(
            "smmu-selftest: {unrouted}'s write {} at {phys:#x}, under no route, answered {answer:#x}: {}",
            i + 1,
            verdict(answer, REFUSED)
        );
    }
    drop(masked);
    events_reach_this_cpu(before + 3, "the unrouted function's three refused writes");
    log!("smmu-selftest: the unrouted function's three events were read, and the machine goes on");

    let masked = crate::arch::IrqGuard::close();
    let taken_back = write(regs, at + 0x40, phys + 0x40);
    log!(
        "smmu-selftest: {function}'s write at {:#x} again, which its domain no longer maps, answered {taken_back:#x}: {}",
        at + 0x40,
        verdict(taken_back, REFUSED)
    );
    drop(masked);
    events_reach_this_cpu(before + 4, "the kernel-driven function's refused write");
    panic!("smmu-selftest: FAIL: the handler read the event of a function this kernel drives and the machine went on");
}

/// Spin until the handler has read `want` events in all, which `what` made;
/// `FAIL` past [`EVENT_TIMEOUT`].
fn events_reach_this_cpu(want: u32, what: &str) {
    let deadline = crate::clock::nanos_since_boot() + EVENT_TIMEOUT.nanos();
    while super::fault::recorded() < want {
        assert!(
            crate::clock::nanos_since_boot() < deadline,
            "smmu-selftest: FAIL: the handler had read {} events, not {want}, {EVENT_TIMEOUT} after {what}",
            super::fault::recorded()
        );
        core::hint::spin_loop();
    }
}
