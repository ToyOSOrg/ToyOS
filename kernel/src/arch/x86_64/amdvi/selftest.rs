//! `amdvi-selftest`: QEMU's two `iommu-testdev` asked three things of the
//! programmed unit, the first enumerated on a domain of its own and the
//! second left on the identity domain:
//!
//! 1. the first writes at an address its domain maps, and lands at the
//!    memory mapped there and nowhere else;
//! 2. the second writes at a physical address of memory, which the identity
//!    domain maps to itself, and lands there;
//! 3. the second writes at an address the identity domain does not map, and
//!    is refused and recorded, and the machine halts: the function is one
//!    this kernel drives.
//!
//! The record is what says the unit translated: a write the unit passed
//! untranslated to 2^46 reaches no memory on a guest of 40 physical bits,
//! and the device answers it refused all the same. The write of 3 is made with this CPU's interrupts masked, so its verdict
//! is on the console before its event is taken. Each step says what it saw
//! on its own line; the harness judges them, and the record by the handler's.

use crate::drivers::pci::PciDevice;
use crate::iommu::testdev::{registers, verdict, write, DEVICE, LANDED, REFUSED, VENDOR};
use crate::iommu::{OwnSpace, StreamId};
use crate::log;
use crate::mm::PAGE_2M;
use crate::time::{Duration, Tripwire};

/// How long the refused write's event is given to reach this CPU once it
/// unmasks.
const EVENT_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(1),
    "the unit writes the record and raises its interrupt while the refused write is still being made",
);

pub fn run(devices: &[PciDevice]) {
    let mut testdevs = devices.iter().filter(|d| d.is_id(VENDOR, DEVICE));
    let (Some(moved), Some(kept), None) = (testdevs.next(), testdevs.next(), testdevs.next()) else {
        panic!("amdvi-selftest: this machine has not two iommu-testdevs (1b36:0005)");
    };
    let (moved_regs, kept_regs) = (registers(moved), registers(kept));
    let page = crate::mm::pmm::alloc_page().expect("amdvi-selftest: no page to aim at");
    let phys = page.direct_map().phys();
    let (on_own, on_identity) =
        (StreamId::pci(moved.bus, moved.dev, moved.func), StreamId::pci(kept.bus, kept.dev, kept.func));

    // Before any declaration, so clear of no window: QEMU routes nothing on
    // its root bus peer to peer, so the device's write reaches the unit.
    let (space, at) =
        OwnSpace::create(PAGE_2M, &[], moved.bus, moved.dev, moved.func).expect("amdvi-selftest: a domain on the programmed unit");
    space.place(at, phys, PAGE_2M).expect("amdvi-selftest: the domain's own first leaf");
    space.attach(moved.bus, moved.dev, moved.func);
    let landed = write(moved_regs, at + 0x40, phys + 0x40);
    log!(
        "amdvi-selftest: {on_own}'s write at {:#x}, mapped to {:#x}, answered {landed:#x}: {}",
        at + 0x40,
        phys + 0x40,
        verdict(landed, LANDED)
    );

    let identity = write(kept_regs, phys + 0x80, phys + 0x80);
    log!(
        "amdvi-selftest: {on_identity}'s write at {:#x}, on the identity domain, answered {identity:#x}: {}",
        phys + 0x80,
        verdict(identity, LANDED)
    );

    let before = super::fault::recorded();
    let masked = crate::arch::IrqGuard::close();
    // Past the memory the identity domain maps; a domain's own addresses are
    // handed out lowest first, so they are no such address.
    let unmapped = crate::mm::pmm::top().next_multiple_of(PAGE_2M);
    let refused = write(kept_regs, unmapped, phys);
    log!(
        "amdvi-selftest: {on_identity}'s write at {unmapped:#x}, which the identity domain does not map, answered \
         {refused:#x}: {}",
        verdict(refused, REFUSED)
    );
    drop(masked);
    let deadline = crate::clock::nanos_since_boot() + EVENT_TIMEOUT.nanos();
    while super::fault::recorded() == before {
        assert!(
            crate::clock::nanos_since_boot() < deadline,
            "amdvi-selftest: FAIL: no record of {on_identity}'s refused write was read {EVENT_TIMEOUT} after it"
        );
        core::hint::spin_loop();
    }
    panic!("amdvi-selftest: FAIL: the handler read the record of a function this kernel drives and the machine went on");
}
