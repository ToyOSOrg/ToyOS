//! Whether a CPU loads at all, and the Intel file the kernel embeds.

#![allow(clippy::unwrap_used, clippy::panic, reason = "a test fails by panicking")]

use super::*;

#[test]
fn a_vendor_no_embedded_file_is_from_loads_nothing() {
    assert_eq!(Maker::of(Vendor::Intel), Ok(Maker::Intel));
    assert_eq!(Maker::of(Vendor::Amd), Ok(Maker::Amd));
    for vendor in [Vendor::Hygon, Vendor::Centaur, Vendor::Zhaoxin, Vendor::Unknown] {
        assert_eq!(Maker::of(vendor), Err(Unloaded::Vendor(vendor)));
    }
}

#[test]
fn a_guest_loads_nothing_whatever_revision_it_reports() {
    for found in [Revision(0xbe), Revision::DISABLED] {
        assert_eq!(may_load(1 << 31, found), Err(Unloaded::Hypervisor));
    }
}

/// Xen's reading of a revision of `~0`: loading is disabled, by a hypervisor
/// or a firmware toggle; an Intel CPU handed an update over it would refuse
/// it at the read-back.
#[test]
fn revision_0xffffffff_loads_nothing() {
    assert_eq!(may_load(0, Revision::DISABLED), Err(Unloaded::Disabled));
    assert_eq!(may_load(0, Revision(0xffff_fffe)), Ok(()));
}

/// The T14's i5-1135G7 below the update its firmware already runs.
#[test]
fn the_embedded_intel_file_hands_the_t14_its_update_16_byte_aligned() {
    let below = intel::Cpu {
        signature: Signature(0x0008_06c1),
        platform: intel::PlatformId::from_msr(7 << 50),
        revision: Revision(0xbd),
    };
    let Ok(Choice::Load(update)) = intel::select(&INTEL.0, &below) else { panic!("0xbe is newer than 0xbd") };
    assert_eq!(update.revision(), Revision(0xbe));
    assert_eq!(update.data().as_ptr().align_offset(16), 0, "SDM §12.11.6 takes the data 16-byte aligned");
}
