//! Where the platform's PCI root bridges decode memory, asked of the firmware
//! that assigned every BAR on the machine.
//!
//! `EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL` (UEFI 2.10 §14.2) is installed on one
//! handle per root bridge, and its `Configuration()` answers with the ACPI
//! resource descriptors describing that bridge's current windows — the same
//! data an operating system reads out of `_CRS`, from the same firmware, and
//! reachable here without an AML interpreter. It has to be asked here: the
//! protocol dies with boot services.
//!
//! **A refusal hands the kernel nothing rather than something partial**, for
//! the reason [`toyos_abi::boot::KernelArgs::root_bridge_windows`] states: a
//! list missing one window is worse than no list.

use core::ptr::read_volatile;

use toyos_abi::boot::RootBridgeWindow;
use toyos_acpi::{memory_windows, Phys, MAX_LIST_BYTES};
use crate::efi::{PciRootBridgeIo, Status, SystemTable};

const HEAD: &str = "Root bridge:";

/// The descriptor list firmware answered with.
///
/// Boot services identity-map physical memory, so the pointer is the address
/// this loader reads. The bound is the decoder's own, because `Configuration()`
/// answers with a pointer and no length: what says a byte belongs to the list is
/// how far the walk may go.
#[derive(Clone, Copy)]
struct List {
    at: u64,
}

impl Phys for List {
    fn readable(self, phys: u64, len: usize) -> bool {
        let Some(ceiling) = self.at.checked_add(MAX_LIST_BYTES as u64) else { return false };
        phys >= self.at && phys.checked_add(len as u64).is_some_and(|e| e <= ceiling)
    }

    fn byte(self, phys: u64) -> u8 {
        // SAFETY: `readable` bounded `phys` to the list, and every caller in
        // `toyos-acpi` asks it before asking this.
        unsafe { read_volatile(phys as *const u8) }
    }
}

/// Every memory window this machine's root bridges decode, written into `out`;
/// the count, or zero on a machine that would not say.
pub fn windows(system_table: &SystemTable, out: &mut [RootBridgeWindow]) -> usize {
    let bs = system_table.boot_services();
    let handles = match bs.handles::<PciRootBridgeIo>() {
        Ok(handles) => handles,
        Err(e) => {
            println!("{HEAD} no handle carries the PCI Root Bridge I/O protocol ({e}), so the kernel is handed no window");
            return 0;
        }
    };

    let mut found = 0usize;
    for (index, handle) in handles.iter().enumerate() {
        let bridge = match bs.get::<PciRootBridgeIo>(*handle) {
            Ok(bridge) => bridge,
            Err(e) => {
                println!("{HEAD} handle {index} would not open ({e}), so the kernel is handed no window");
                return 0;
            }
        };

        let answer = bridge.configuration();
        let Some(&resources) = answer.as_ref().ok().filter(|resources| !resources.is_null()) else {
            println!(
                "{HEAD} {index} (segment {}) answered {:?} to Configuration(), so the kernel is handed no window",
                bridge.segment_number,
                answer.err().unwrap_or(Status::SUCCESS),
            );
            return 0;
        };

        let list = List { at: resources as u64 };
        match memory_windows(list, list.at, &mut out[found..]) {
            Ok(count) => found += count,
            Err(why) => {
                println!("{HEAD} {index} (segment {}) {why}, so the kernel is handed no window", bridge.segment_number);
                return 0;
            }
        }
    }

    if found == 0 {
        println!("{HEAD} {} bridge(s) name no memory window, so the kernel is handed none", handles.len());
    }
    found
}
