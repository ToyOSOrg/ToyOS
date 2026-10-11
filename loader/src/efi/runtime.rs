//! `EFI_RUNTIME_SERVICES` (UEFI 2.10 §4.5) and what this loader calls of
//! them: the time, variables, and the reset.

use core::ffi::c_void;
use core::mem::{offset_of, MaybeUninit};
use core::ptr;

use alloc::vec;
use alloc::vec::Vec;

use super::{CStr16, CString16, Guid, Status, TableHeader};

type Unused = *const c_void;

/// `EFI_RUNTIME_SERVICES` (§4.5), in the spec's own field order.
#[repr(C)]
pub struct RuntimeServices {
    hdr: TableHeader,
    get_time: unsafe extern "efiapi" fn(time: *mut Time, capabilities: *mut c_void) -> Status,
    set_time: Unused,
    get_wakeup_time: Unused,
    set_wakeup_time: Unused,
    set_virtual_address_map: Unused,
    convert_pointer: Unused,
    get_variable: unsafe extern "efiapi" fn(
        name: *const u16,
        vendor: *const Guid,
        attributes: *mut u32,
        size: *mut usize,
        data: *mut u8,
    ) -> Status,
    get_next_variable_name: unsafe extern "efiapi" fn(size: *mut usize, name: *mut u16, vendor: *mut Guid) -> Status,
    set_variable: unsafe extern "efiapi" fn(
        name: *const u16,
        vendor: *const Guid,
        attributes: u32,
        size: usize,
        data: *const u8,
    ) -> Status,
    get_next_high_monotonic_count: Unused,
    reset_system: unsafe extern "efiapi" fn(kind: ResetType, status: Status, size: usize, data: *const c_void) -> !,
    update_capsule: Unused,
    query_capsule_capabilities: Unused,
    query_variable_info: Unused,
}

const _: () = {
    assert!(size_of::<RuntimeServices>() == 24 + 14 * 8);
    assert!(offset_of!(RuntimeServices, get_time) == 24);
    assert!(offset_of!(RuntimeServices, get_variable) == 72);
    assert!(offset_of!(RuntimeServices, get_next_variable_name) == 80);
    assert!(offset_of!(RuntimeServices, set_variable) == 88);
    assert!(offset_of!(RuntimeServices, reset_system) == 104);
};

/// `EFI_TIME` (§8.3).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Time {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pad1: u8,
    pub nanosecond: u32,
    pub time_zone: i16,
    pub daylight: u8,
    pad2: u8,
}

const _: () = {
    assert!(size_of::<Time>() == 16);
    assert!(offset_of!(Time, nanosecond) == 8);
    assert!(offset_of!(Time, time_zone) == 12);
    assert!(offset_of!(Time, daylight) == 14);
};

/// `EFI_RESET_TYPE` (§8.5.1).
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct ResetType(u32);

impl ResetType {
    pub const COLD: ResetType = ResetType(0);
    pub const WARM: ResetType = ResetType(1);
    pub const SHUTDOWN: ResetType = ResetType(2);
}

/// A variable's attributes (§8.2.1).
pub struct VariableAttributes;

impl VariableAttributes {
    pub const NON_VOLATILE: u32 = 0x1;
    pub const BOOTSERVICE_ACCESS: u32 = 0x2;
    pub const RUNTIME_ACCESS: u32 = 0x4;
}

/// `EFI_GLOBAL_VARIABLE` (§3.3), the vendor of `Boot####` and `BootNext`.
pub const GLOBAL_VARIABLE: Guid =
    Guid::new(0x8be4df61, 0x93ca, 0x11d2, [0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c]);

impl RuntimeServices {
    /// `GetTime` (§8.3.1), without the clock's capabilities.
    pub fn get_time(&self) -> Result<Time, Status> {
        let mut time = MaybeUninit::<Time>::uninit();
        // SAFETY: firmware writes the whole `EFI_TIME` on success, and a null
        // capabilities pointer is allowed.
        unsafe { (self.get_time)(time.as_mut_ptr(), ptr::null_mut()) }.ok()?;
        // SAFETY: written on success.
        Ok(unsafe { time.assume_init() })
    }

    /// `GetVariable` (§8.2.1): the value and its attributes.
    pub fn get_variable(&self, name: &CStr16, vendor: &Guid) -> Result<(Vec<u8>, u32), Status> {
        let (mut attributes, mut size) = (0u32, 0usize);
        // SAFETY: a zero size with no buffer asks for the size.
        let status = unsafe { (self.get_variable)(name.as_ptr(), vendor, &mut attributes, &mut size, ptr::null_mut()) };
        if status.is_success() {
            return Ok((Vec::new(), attributes));
        }
        if status != Status::BUFFER_TOO_SMALL {
            return Err(status);
        }
        let mut data = vec![0u8; size];
        // SAFETY: `data` is `size` writable bytes.
        unsafe { (self.get_variable)(name.as_ptr(), vendor, &mut attributes, &mut size, data.as_mut_ptr()) }.ok()?;
        data.truncate(size);
        Ok((data, attributes))
    }

    /// Every variable's name and vendor (`GetNextVariableName`, §8.2.2).
    pub fn variable_keys(&self) -> Result<Vec<(CString16, Guid)>, Status> {
        let mut keys = Vec::new();
        // Starts as the empty name, which asks for the first variable; each
        // call reads the last name back out of it.
        let mut name = vec![0u16; 32];
        let mut vendor = Guid([0; 16]);
        loop {
            let mut size = size_of_val(&name[..]);
            // SAFETY: `name` is `size` writable bytes holding a NUL-terminated name.
            let status = unsafe { (self.get_next_variable_name)(&mut size, name.as_mut_ptr(), &mut vendor) };
            match status {
                Status::SUCCESS => {
                    // A name firmware wrote with no NUL inside the buffer ends the walk.
                    let Some(end) = name.iter().position(|unit| *unit == 0) else { return Err(Status::ABORTED) };
                    let mut units = name[..end].to_vec();
                    units.push(0);
                    keys.push((CString16(units), vendor));
                }
                Status::BUFFER_TOO_SMALL => name.resize(size.div_ceil(2), 0),
                Status::NOT_FOUND => return Ok(keys),
                other => return Err(other),
            }
        }
    }

    /// `SetVariable` (§8.2.3).
    pub fn set_variable(&self, name: &CStr16, vendor: &Guid, attributes: u32, data: &[u8]) -> Result<(), Status> {
        // SAFETY: `data` is `data.len()` readable bytes.
        unsafe { (self.set_variable)(name.as_ptr(), vendor, attributes, data.len(), data.as_ptr()) }.ok()
    }

    /// `SetVariable` with no attributes and no data, which deletes (§8.2.3).
    pub fn delete_variable(&self, name: &CStr16, vendor: &Guid) -> Result<(), Status> {
        self.set_variable(name, vendor, 0, &[])
    }

    /// `ResetSystem` (§8.5.1), with no data.
    pub fn reset(&self, kind: ResetType, status: Status) -> ! {
        // SAFETY: no data, so no pointer is read.
        unsafe { (self.reset_system)(kind, status, 0, ptr::null()) }
    }
}
