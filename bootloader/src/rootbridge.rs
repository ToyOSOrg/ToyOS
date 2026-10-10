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

use core::ffi::c_void;
use core::ptr::read_volatile;

use toyos_abi::boot::RootBridgeWindow;
use toyos_acpi::{memory_windows, Phys, MAX_LIST_BYTES};
use uefi::prelude::*;
use uefi::proto::unsafe_protocol;

const HEAD: &str = "Root bridge:";

/// UEFI 2.10 §14.2.2, in the spec's own field order.
#[repr(C)]
#[unsafe_protocol("2f707ebb-4a1a-11d4-9a38-0090273fc14d")]
pub(crate) struct PciRootBridgeIo {
    parent_handle: *mut c_void,
    poll_mem: *mut c_void,
    poll_io: *mut c_void,
    mem_read: *mut c_void,
    mem_write: *mut c_void,
    io_read: *mut c_void,
    io_write: *mut c_void,
    pci_read: unsafe extern "efiapi" fn(
        this: *const PciRootBridgeIo,
        width: u32,
        address: u64,
        count: usize,
        buffer: *mut c_void,
    ) -> Status,
    pci_write: *mut c_void,
    copy_mem: *mut c_void,
    map: *mut c_void,
    unmap: *mut c_void,
    allocate_buffer: *mut c_void,
    free_buffer: *mut c_void,
    flush: *mut c_void,
    get_attributes: *mut c_void,
    set_attributes: *mut c_void,
    configuration:
        unsafe extern "efiapi" fn(this: *const PciRootBridgeIo, resources: *mut *const c_void)
            -> Status,
    pub(crate) segment_number: u32,
}

/// `EfiPciWidthUint32` (UEFI 2.10 §14.2.2).
const WIDTH_U32: u32 = 2;

impl PciRootBridgeIo {
    /// One configuration dword of `bus:device.function` at `offset`, read by
    /// the bridge's own `Pci.Read`: firmware's access, through no mapping of
    /// this loader's. `EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL_PCI_ADDRESS` carries an
    /// offset past the first 256 bytes in `ExtendedRegister`, with `Register`
    /// zero.
    pub(crate) fn config_u32(&self, bus: u8, device: u8, function: u8, offset: u16) -> Result<u32, Status> {
        let place = (u64::from(bus) << 24) | (u64::from(device & 0x1f) << 16) | (u64::from(function & 7) << 8);
        let offset = u64::from(offset & 0xffc);
        let address = if offset < 0x100 { place | offset } else { place | (offset << 32) };
        let mut value = 0u32;
        // SAFETY: `self` is the protocol firmware installed, the call is the
        // spec's, and the buffer is one `u32` for a count of one at that width.
        let status = unsafe { (self.pci_read)(self, WIDTH_U32, address, 1, (&mut value as *mut u32).cast()) };
        if status.is_success() { Ok(value) } else { Err(status) }
    }
}

/// A field added or dropped above moves `configuration`, and the symptom is
/// firmware calling something else.
const _: () = assert!(core::mem::size_of::<PciRootBridgeIo>() == 18 * 8 + 8);

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
pub fn windows(system_table: &SystemTable<Boot>, out: &mut [RootBridgeWindow]) -> usize {
    let bs = system_table.boot_services();
    let handles = match bs.find_handles::<PciRootBridgeIo>() {
        Ok(handles) => handles,
        Err(e) => {
            println!("{HEAD} no handle carries the PCI Root Bridge I/O protocol ({e}), so the kernel is handed no window");
            return 0;
        }
    };

    let mut found = 0usize;
    for (index, handle) in handles.iter().enumerate() {
        let bridge = match crate::protocol::get::<PciRootBridgeIo>(bs, *handle) {
            Ok(bridge) => bridge,
            Err(e) => {
                println!("{HEAD} handle {index} would not open ({e}), so the kernel is handed no window");
                return 0;
            }
        };

        let mut resources: *const c_void = core::ptr::null();
        let this: *const PciRootBridgeIo = &*bridge;
        // SAFETY: `this` is the protocol firmware installed on this handle and
        // the call is the spec's — one out parameter, which firmware fills with
        // a pointer it owns and this loader only reads.
        let status = unsafe { (bridge.configuration)(this, &mut resources) };
        if !status.is_success() || resources.is_null() {
            println!(
                "{HEAD} {index} (segment {}) answered {status:?} to Configuration(), so the kernel is handed no window",
                bridge.segment_number
            );
            return 0;
        }

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
