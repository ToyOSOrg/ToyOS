//! `EFI_BOOT_SERVICES` (UEFI 2.10 §4.4) and what this loader calls of them:
//! pages and pool, the memory map, the watchdog, handles, protocols, and the
//! exit.
//!
//! **A protocol is opened two ways and no other.** EXCLUSIVE calls `Stop` on
//! every driver holding the protocol BY_DRIVER (UEFI 2.10 §7.3.9,
//! `OpenProtocol()`), and on `GraphicsOutput` that is the firmware's graphics
//! console, whose screen the loader's own lines are on. [`BootServices::get`]
//! opens GET_PROTOCOL, which stops nothing; [`BootServices::exclusive`] opens
//! only an [`Exclusive`] protocol, one no firmware console drives.

use core::ffi::c_void;
use core::mem::offset_of;
use core::ptr::{self, NonNull};

use super::{Guid, Handle, Status, TableHeader};

/// Every page UEFI counts (§7.2.1).
pub const PAGE_SIZE: usize = 4096;

/// `EfiLoaderData` (§7.2.1): what this loader allocates, and what the
/// firmware gives an application's data.
const LOADER_DATA: u32 = 2;

type Unused = *const c_void;

/// `EFI_BOOT_SERVICES` (§4.4), in the spec's own field order.
#[repr(C)]
pub struct BootServices {
    hdr: TableHeader,
    raise_tpl: Unused,
    restore_tpl: Unused,
    allocate_pages: unsafe extern "efiapi" fn(kind: u32, memory: u32, pages: usize, at: *mut u64) -> Status,
    free_pages: unsafe extern "efiapi" fn(at: u64, pages: usize) -> Status,
    get_memory_map: unsafe extern "efiapi" fn(
        size: *mut usize,
        map: *mut u64,
        key: *mut usize,
        descriptor_size: *mut usize,
        descriptor_version: *mut u32,
    ) -> Status,
    allocate_pool: unsafe extern "efiapi" fn(memory: u32, size: usize, at: *mut *mut u8) -> Status,
    free_pool: unsafe extern "efiapi" fn(at: *mut u8) -> Status,
    create_event: Unused,
    set_timer: Unused,
    wait_for_event: Unused,
    signal_event: Unused,
    close_event: Unused,
    check_event: Unused,
    install_protocol_interface: Unused,
    reinstall_protocol_interface: Unused,
    uninstall_protocol_interface: Unused,
    handle_protocol: Unused,
    reserved: Unused,
    register_protocol_notify: Unused,
    locate_handle: Unused,
    locate_device_path: Unused,
    install_configuration_table: Unused,
    load_image: Unused,
    start_image: Unused,
    exit: Unused,
    unload_image: Unused,
    exit_boot_services: unsafe extern "efiapi" fn(image: *mut c_void, key: usize) -> Status,
    get_next_monotonic_count: Unused,
    stall: Unused,
    set_watchdog_timer:
        unsafe extern "efiapi" fn(seconds: usize, code: u64, data_size: usize, data: *const u16) -> Status,
    connect_controller: Unused,
    disconnect_controller: Unused,
    open_protocol: unsafe extern "efiapi" fn(
        handle: *mut c_void,
        protocol: *const Guid,
        interface: *mut *mut c_void,
        agent: *mut c_void,
        controller: *mut c_void,
        attributes: u32,
    ) -> Status,
    close_protocol: unsafe extern "efiapi" fn(
        handle: *mut c_void,
        protocol: *const Guid,
        agent: *mut c_void,
        controller: *mut c_void,
    ) -> Status,
    open_protocol_information: Unused,
    protocols_per_handle: Unused,
    locate_handle_buffer: unsafe extern "efiapi" fn(
        search: u32,
        protocol: *const Guid,
        key: *const c_void,
        count: *mut usize,
        buffer: *mut *mut Handle,
    ) -> Status,
    locate_protocol: Unused,
    install_multiple_protocol_interfaces: Unused,
    uninstall_multiple_protocol_interfaces: Unused,
    calculate_crc32: Unused,
    copy_mem: Unused,
    set_mem: Unused,
    create_event_ex: Unused,
}

const _: () = {
    assert!(size_of::<BootServices>() == 24 + 44 * 8);
    assert!(offset_of!(BootServices, allocate_pages) == 40);
    assert!(offset_of!(BootServices, free_pages) == 48);
    assert!(offset_of!(BootServices, get_memory_map) == 56);
    assert!(offset_of!(BootServices, allocate_pool) == 64);
    assert!(offset_of!(BootServices, free_pool) == 72);
    assert!(offset_of!(BootServices, exit_boot_services) == 232);
    assert!(offset_of!(BootServices, set_watchdog_timer) == 256);
    assert!(offset_of!(BootServices, open_protocol) == 280);
    assert!(offset_of!(BootServices, close_protocol) == 288);
    assert!(offset_of!(BootServices, locate_handle_buffer) == 312);
};

/// `EFI_ALLOCATE_TYPE` (§7.2.1), as this loader asks.
pub enum AllocateType {
    AnyPages,
    Address(u64),
}

/// `OpenProtocol`'s two attributes this loader passes (§7.3.9).
const GET_PROTOCOL: u32 = 0x02;
const EXCLUSIVE: u32 = 0x20;

/// `ByProtocol` of `EFI_LOCATE_SEARCH_TYPE` (§7.3.15).
const BY_PROTOCOL: u32 = 2;

/// An interface firmware installs on a handle.
///
/// # Safety
/// `Self` is the layout of the interface installed under [`Protocol::GUID`].
pub unsafe trait Protocol {
    const GUID: Guid;
}

/// A protocol this loader may hold EXCLUSIVE: one no firmware console drives.
pub trait Exclusive: Protocol {}

/// A protocol open on a handle, closed when this drops.
pub struct Scoped<'a, P: Protocol> {
    bs: &'a BootServices,
    handle: Handle,
    interface: NonNull<P>,
}

impl<P: Protocol> core::ops::Deref for Scoped<'_, P> {
    type Target = P;
    fn deref(&self) -> &P {
        // SAFETY: firmware's interface, installed under `P::GUID` and held
        // open for as long as `self`.
        unsafe { self.interface.as_ref() }
    }
}

impl<P: Protocol> Scoped<'_, P> {
    /// The interface as firmware's functions take it, `This`.
    pub fn this(&self) -> *mut P {
        self.interface.as_ptr()
    }
}

impl<P: Protocol> Drop for Scoped<'_, P> {
    fn drop(&mut self) {
        // SAFETY: the open this undoes, under the same agent.
        let status = unsafe {
            (self.bs.close_protocol)(self.handle.as_ptr(), &P::GUID, super::image().as_ptr(), ptr::null_mut())
        };
        // Only a close that names another open fails, and this names its own.
        assert!(status.is_success(), "CloseProtocol({}) answered {status}", P::GUID);
    }
}

/// The handles `LocateHandleBuffer` answered, in its pool buffer.
pub struct Handles<'a> {
    bs: &'a BootServices,
    at: NonNull<Handle>,
    count: usize,
}

impl core::ops::Deref for Handles<'_> {
    type Target = [Handle];
    fn deref(&self) -> &[Handle] {
        // SAFETY: firmware answered `count` handles at `at`.
        unsafe { core::slice::from_raw_parts(self.at.as_ptr(), self.count) }
    }
}

impl Drop for Handles<'_> {
    fn drop(&mut self) {
        // SAFETY: the pool buffer `LocateHandleBuffer` allocated for this answer.
        let freed = unsafe { self.bs.free_pool(self.at.as_ptr().cast()) };
        assert!(freed.is_ok(), "firmware would not take back the handle buffer it gave");
    }
}

/// `EFI_MEMORY_DESCRIPTOR` (§7.2.3). Firmware's stride is its own
/// `DescriptorSize`, never this struct's size.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct MemoryDescriptor {
    pub ty: u32,
    pub phys_start: u64,
    pub virt_start: u64,
    pub page_count: u64,
    pub att: u64,
}

const _: () = {
    assert!(size_of::<MemoryDescriptor>() == 40);
    assert!(offset_of!(MemoryDescriptor, phys_start) == 8);
    assert!(offset_of!(MemoryDescriptor, page_count) == 24);
    assert!(offset_of!(MemoryDescriptor, att) == 32);
};

impl MemoryDescriptor {
    /// `EfiMemoryMappedIO` and `EfiMemoryMappedIOPortSpace` (§7.2.1).
    pub const MMIO: u32 = 11;
    pub const MMIO_PORT_SPACE: u32 = 12;
    /// `EFI_MEMORY_WB` (§7.2.3).
    pub const WRITE_BACK: u64 = 0x8;
}

/// What `GetMemoryMap` wrote: its key and its extent.
#[derive(Clone, Copy)]
pub(super) struct Filled {
    pub(super) key: usize,
    size: usize,
    entry_size: usize,
}

/// The memory map in the buffer it was taken into.
pub struct MemoryMap<'a> {
    words: &'a [u64],
    filled: Filled,
}

impl<'a> MemoryMap<'a> {
    pub(super) fn new(words: &'a [u64], filled: Filled) -> Self {
        MemoryMap { words, filled }
    }

    pub fn entries(&self) -> Entries<'_> {
        Entries { map: self, next: 0 }
    }
}

/// A [`MemoryMap`]'s descriptors, in firmware's order.
pub struct Entries<'a> {
    map: &'a MemoryMap<'a>,
    next: usize,
}

impl Iterator for Entries<'_> {
    type Item = MemoryDescriptor;

    fn next(&mut self) -> Option<MemoryDescriptor> {
        let Filled { size, entry_size, .. } = self.map.filled;
        if self.next >= size / entry_size {
            return None;
        }
        let at = self.next * entry_size;
        self.next += 1;
        // SAFETY: `fill_memory_map` checked the descriptors lie inside the
        // buffer and that each is at least a `MemoryDescriptor` long.
        Some(unsafe { self.map.words.as_ptr().cast::<u8>().add(at).cast::<MemoryDescriptor>().read_unaligned() })
    }
}

impl BootServices {
    /// `AllocatePages` (§7.2.1), as `EfiLoaderData`.
    pub fn allocate_pages(&self, kind: AllocateType, pages: usize) -> Result<u64, Status> {
        let (kind, mut at) = match kind {
            AllocateType::AnyPages => (0, 0),
            AllocateType::Address(at) => (2, at),
        };
        // SAFETY: the out parameter is a live `u64`.
        unsafe { (self.allocate_pages)(kind, LOADER_DATA, pages, &mut at) }.ok().map(|()| at)
    }

    /// `FreePages` (§7.2.2).
    ///
    /// # Safety
    /// `at` and `pages` are an allocation `allocate_pages` made, and nothing
    /// reads or writes it after.
    pub unsafe fn free_pages(&self, at: u64, pages: usize) -> Result<(), Status> {
        // SAFETY: the caller's contract.
        unsafe { (self.free_pages)(at, pages) }.ok()
    }

    /// `AllocatePool` (§7.2.4), as `EfiLoaderData`: 8-byte aligned.
    pub fn allocate_pool(&self, size: usize) -> Result<*mut u8, Status> {
        let mut at = ptr::null_mut();
        // SAFETY: the out parameter is a live pointer.
        unsafe { (self.allocate_pool)(LOADER_DATA, size, &mut at) }.ok().map(|()| at)
    }

    /// `FreePool` (§7.2.5).
    ///
    /// # Safety
    /// `at` is a pool allocation of firmware's, and nothing reads or writes it
    /// after.
    pub unsafe fn free_pool(&self, at: *mut u8) -> Result<(), Status> {
        // SAFETY: the caller's contract.
        unsafe { (self.free_pool)(at) }.ok()
    }

    /// The bytes the memory map takes now, and the size of one descriptor.
    pub fn memory_map_size(&self) -> (usize, usize) {
        let (mut size, mut key, mut entry_size, mut version) = (0, 0, 0, 0);
        // SAFETY: a zero size with no buffer asks only for the size (§7.2.3).
        let status =
            unsafe { (self.get_memory_map)(&mut size, ptr::null_mut(), &mut key, &mut entry_size, &mut version) };
        assert!(status == Status::BUFFER_TOO_SMALL, "GetMemoryMap with no buffer answered {status}");
        (size, entry_size)
    }

    /// The memory map, taken into `words`.
    pub fn memory_map<'b>(&self, words: &'b mut [u64]) -> Result<MemoryMap<'b>, Status> {
        let filled = self.fill_memory_map(words)?;
        Ok(MemoryMap::new(words, filled))
    }

    pub(super) fn fill_memory_map(&self, words: &mut [u64]) -> Result<Filled, Status> {
        let capacity = size_of_val(words);
        let (mut size, mut key, mut entry_size, mut version) = (capacity, 0, 0, 0);
        // SAFETY: `words` is `size` writable bytes, aligned for a descriptor.
        unsafe { (self.get_memory_map)(&mut size, words.as_mut_ptr(), &mut key, &mut entry_size, &mut version) }
            .ok()?;
        assert!(
            size <= capacity && entry_size >= size_of::<MemoryDescriptor>(),
            "GetMemoryMap answered {size} bytes of {entry_size}-byte descriptors into {capacity}"
        );
        Ok(Filled { key, size, entry_size })
    }

    /// `ExitBootServices` (§7.4.6).
    pub(super) fn exit(&self, image: Handle, key: usize) -> Status {
        // SAFETY: the call is the spec's; the caller stops using boot services
        // when it succeeds.
        unsafe { (self.exit_boot_services)(image.as_ptr(), key) }
    }

    /// This image's handle.
    pub fn image_handle(&self) -> Handle {
        super::image()
    }

    /// `SetWatchdogTimer` (§7.5.1) with no data.
    pub fn set_watchdog_timer(&self, seconds: usize, code: u64) -> Result<(), Status> {
        // SAFETY: no data, so no pointer is read.
        unsafe { (self.set_watchdog_timer)(seconds, code, 0, ptr::null()) }.ok()
    }

    /// Every handle carrying `P` (`LocateHandleBuffer`, §7.3.15).
    pub fn handles<P: Protocol>(&self) -> Result<Handles<'_>, Status> {
        let (mut count, mut at) = (0usize, ptr::null_mut());
        // SAFETY: both out parameters are live.
        unsafe { (self.locate_handle_buffer)(BY_PROTOCOL, &P::GUID, ptr::null(), &mut count, &mut at) }.ok()?;
        let at = NonNull::new(at).expect("LocateHandleBuffer answered success with no buffer");
        Ok(Handles { bs: self, at, count })
    }

    /// The first handle carrying `P`.
    pub fn handle_for<P: Protocol>(&self) -> Result<Handle, Status> {
        self.handles::<P>()?.first().copied().ok_or(Status::NOT_FOUND)
    }

    /// `P` on `handle`, GET_PROTOCOL: it stops no driver.
    ///
    /// Nothing between the open and the drop can uninstall the protocol: the
    /// loader is the one image running, it registers no event callback, and it
    /// calls no boot service that connects or disconnects a controller.
    pub fn get<P: Protocol>(&self, handle: Handle) -> Result<Scoped<'_, P>, Status> {
        self.open(handle, GET_PROTOCOL)
    }

    /// `P` on `handle`, EXCLUSIVE.
    pub fn exclusive<P: Exclusive>(&self, handle: Handle) -> Result<Scoped<'_, P>, Status> {
        self.open(handle, EXCLUSIVE)
    }

    fn open<P: Protocol>(&self, handle: Handle, attributes: u32) -> Result<Scoped<'_, P>, Status> {
        let mut interface = ptr::null_mut();
        // SAFETY: the out parameter is live, and the agent is this image.
        unsafe {
            (self.open_protocol)(
                handle.as_ptr(),
                &P::GUID,
                &mut interface,
                super::image().as_ptr(),
                ptr::null_mut(),
                attributes,
            )
        }
        .ok()?;
        let interface = NonNull::new(interface.cast::<P>())
            .unwrap_or_else(|| panic!("OpenProtocol({}) answered success with no interface", P::GUID));
        Ok(Scoped { bs: self, handle, interface })
    }
}

