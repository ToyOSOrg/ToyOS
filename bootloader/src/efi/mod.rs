//! The UEFI this loader calls, and nothing it does not: UEFI 2.10's tables,
//! protocols, GUIDs and status codes, each laid out from the section cited at
//! it and held there by compile-time `size_of`/`offset_of!` asserts, so a field
//! added, dropped or resized fails the build of both targets.
//!
//! Firmware is the loader's host and is trusted to keep its own contracts; a
//! length it writes back about a buffer of this side's is still held to that
//! buffer. The safe wrappers each remove a hazard that is this side's: a
//! name handed over without its NUL, a protocol left open or opened in a way
//! that stops a driver, a boot service called once `ExitBootServices` has run.
//!
//! **Boot services end at [`SystemTable::exit_boot_services`]**, which consumes
//! the table; the console and the allocator, which reach firmware without it,
//! refuse from then on.

mod boot;
mod proto;
mod runtime;

use core::ffi::c_void;
use core::fmt;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use alloc::vec::Vec;

pub use boot::{AllocateType, BootServices, MemoryDescriptor, MemoryMap, Scoped, PAGE_SIZE};
pub use proto::{
    BlockIo, DevicePath, File, Gop, HardDrive, LoadedImage, Mode, PartitionInfo, PciRootBridgeIo,
    PixelFormat, Rng, SimpleFileSystem,
};
pub use runtime::{ResetType, RuntimeServices, Time, VariableAttributes, GLOBAL_VARIABLE};

/// `EFI_STATUS` (UEFI 2.10 Appendix D): success, a warning, or an error,
/// which carries the top bit.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
#[must_use]
pub struct Status(pub usize);

const ERROR: usize = 1 << (usize::BITS - 1);

/// Appendix D's codes by name: every refusal this loader writes carries one.
const NAMES: [(usize, &str); 41] = [
    (0, "SUCCESS"),
    (1, "WARN_UNKNOWN_GLYPH"),
    (2, "WARN_DELETE_FAILURE"),
    (3, "WARN_WRITE_FAILURE"),
    (4, "WARN_BUFFER_TOO_SMALL"),
    (5, "WARN_STALE_DATA"),
    (6, "WARN_FILE_SYSTEM"),
    (7, "WARN_RESET_REQUIRED"),
    (ERROR | 1, "LOAD_ERROR"),
    (ERROR | 2, "INVALID_PARAMETER"),
    (ERROR | 3, "UNSUPPORTED"),
    (ERROR | 4, "BAD_BUFFER_SIZE"),
    (ERROR | 5, "BUFFER_TOO_SMALL"),
    (ERROR | 6, "NOT_READY"),
    (ERROR | 7, "DEVICE_ERROR"),
    (ERROR | 8, "WRITE_PROTECTED"),
    (ERROR | 9, "OUT_OF_RESOURCES"),
    (ERROR | 10, "VOLUME_CORRUPTED"),
    (ERROR | 11, "VOLUME_FULL"),
    (ERROR | 12, "NO_MEDIA"),
    (ERROR | 13, "MEDIA_CHANGED"),
    (ERROR | 14, "NOT_FOUND"),
    (ERROR | 15, "ACCESS_DENIED"),
    (ERROR | 16, "NO_RESPONSE"),
    (ERROR | 17, "NO_MAPPING"),
    (ERROR | 18, "TIMEOUT"),
    (ERROR | 19, "NOT_STARTED"),
    (ERROR | 20, "ALREADY_STARTED"),
    (ERROR | 21, "ABORTED"),
    (ERROR | 22, "ICMP_ERROR"),
    (ERROR | 23, "TFTP_ERROR"),
    (ERROR | 24, "PROTOCOL_ERROR"),
    (ERROR | 25, "INCOMPATIBLE_VERSION"),
    (ERROR | 26, "SECURITY_VIOLATION"),
    (ERROR | 27, "CRC_ERROR"),
    (ERROR | 28, "END_OF_MEDIA"),
    (ERROR | 31, "END_OF_FILE"),
    (ERROR | 32, "INVALID_LANGUAGE"),
    (ERROR | 33, "COMPROMISED_DATA"),
    (ERROR | 34, "IP_ADDRESS_CONFLICT"),
    (ERROR | 35, "HTTP_ERROR"),
];

impl Status {
    pub const SUCCESS: Status = Status(0);
    pub const BUFFER_TOO_SMALL: Status = Status(ERROR | 5);
    pub const NOT_FOUND: Status = Status(ERROR | 14);
    pub const ABORTED: Status = Status(ERROR | 21);

    pub fn is_success(self) -> bool {
        self == Self::SUCCESS
    }

    /// `Ok` for `SUCCESS` alone: a warning is not what was asked for, and is
    /// refused like an error.
    pub fn ok(self) -> Result<(), Status> {
        if self.is_success() { Ok(()) } else { Err(self) }
    }
}

impl fmt::Debug for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match NAMES.iter().find(|(code, _)| *code == self.0) {
            Some((_, name)) => f.write_str(name),
            None => write!(f, "Status({:#x})", self.0),
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// `EFI_GUID` (UEFI 2.10 Appendix A): its three leading fields little-endian,
/// as it is held in memory and on a GPT disk.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C, align(4))]
pub struct Guid([u8; 16]);

const _: () = assert!(size_of::<Guid>() == 16);

impl Guid {
    /// The GUID the specification prints as `{a, b, c, {d...}}`.
    pub const fn new(a: u32, b: u16, c: u16, d: [u8; 8]) -> Self {
        let (a, b, c) = (a.to_le_bytes(), b.to_le_bytes(), c.to_le_bytes());
        Guid([a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]])
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            u16::from_le_bytes([b[4], b[5]]),
            u16::from_le_bytes([b[6], b[7]]),
            b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
        )
    }
}

/// `EFI_HANDLE` (UEFI 2.10 §2.3.1): opaque, and never null where firmware
/// hands one back.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Handle(NonNull<c_void>);

impl Handle {
    fn from_ptr(ptr: *mut c_void) -> Option<Handle> {
        NonNull::new(ptr).map(Handle)
    }

    fn as_ptr(self) -> *mut c_void {
        self.0.as_ptr()
    }
}

/// A NUL-terminated `CHAR16` string (UEFI 2.10 §2.3.1), the only form a name
/// crosses to firmware in: the type is what keeps firmware from reading past
/// one handed over without its terminator.
#[repr(transparent)]
pub struct CStr16([u16]);

impl CStr16 {
    /// `units` as a name, where its last unit is its only NUL.
    pub const fn from_units(units: &[u16]) -> Option<&CStr16> {
        let Some((last, body)) = units.split_last() else { return None };
        if *last != 0 {
            return None;
        }
        let mut i = 0;
        while i < body.len() {
            if body[i] == 0 {
                return None;
            }
            i += 1;
        }
        // SAFETY: `CStr16` is `repr(transparent)` over `[u16]`.
        Some(unsafe { &*(units as *const [u16] as *const CStr16) })
    }

    fn as_ptr(&self) -> *const u16 {
        self.0.as_ptr()
    }

    /// The units before the NUL.
    pub fn units(&self) -> &[u16] {
        &self.0[..self.0.len() - 1]
    }
}

impl fmt::Display for CStr16 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for unit in self.units() {
            fmt::Write::write_char(f, char::from_u32(u32::from(*unit)).unwrap_or(char::REPLACEMENT_CHARACTER))?;
        }
        Ok(())
    }
}

/// An owned [`CStr16`].
pub struct CString16(Vec<u16>);

impl CString16 {
    /// `text` as a name, or `None` where it holds a NUL or a character past
    /// UCS-2, which `CHAR16` cannot carry.
    pub fn new(text: &str) -> Option<CString16> {
        let mut units = Vec::with_capacity(text.len() + 1);
        for ch in text.chars() {
            let unit = u16::try_from(u32::from(ch)).ok().filter(|&u| u != 0)?;
            units.push(unit);
        }
        units.push(0);
        Some(CString16(units))
    }
}

impl core::ops::Deref for CString16 {
    type Target = CStr16;
    fn deref(&self) -> &CStr16 {
        CStr16::from_units(&self.0).expect("a CString16 ends in its only NUL")
    }
}

impl fmt::Display for CString16 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

/// `text`, ASCII, as `CHAR16` units with the NUL: [`cstr16!`]'s body.
pub const fn ucs2<const N: usize>(text: &str) -> [u16; N] {
    let bytes = text.as_bytes();
    assert!(bytes.len() + 1 == N, "the array is the text and its NUL");
    let mut units = [0u16; N];
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i] != 0 && bytes[i] < 0x80, "a literal name is ASCII with no NUL");
        units[i] = bytes[i] as u16;
        i += 1;
    }
    units
}

/// A literal name as a `&'static CStr16`, checked at compile time.
macro_rules! cstr16 {
    ($text:literal) => {{
        const UNITS: [u16; $text.len() + 1] = $crate::efi::ucs2($text);
        const NAME: &$crate::efi::CStr16 = match $crate::efi::CStr16::from_units(&UNITS) {
            Some(name) => name,
            None => panic!("a literal name is its text and one NUL"),
        };
        NAME
    }};
}
pub(crate) use cstr16;

/// `EFI_TABLE_HEADER` (UEFI 2.10 §4.2).
#[repr(C)]
struct TableHeader {
    signature: u64,
    revision: u32,
    header_size: u32,
    crc32: u32,
    reserved: u32,
}

const _: () = assert!(size_of::<TableHeader>() == 24);

/// `EFI_SYSTEM_TABLE` (UEFI 2.10 §4.3).
#[repr(C)]
struct RawSystemTable {
    hdr: TableHeader,
    firmware_vendor: *const u16,
    firmware_revision: u32,
    console_in_handle: *mut c_void,
    con_in: *mut c_void,
    console_out_handle: *mut c_void,
    con_out: *mut proto::TextOutput,
    standard_error_handle: *mut c_void,
    std_err: *mut c_void,
    runtime_services: *const RuntimeServices,
    boot_services: *const BootServices,
    number_of_table_entries: usize,
    configuration_table: *const ConfigurationTable,
}

const _: () = {
    assert!(size_of::<RawSystemTable>() == 120);
    assert!(core::mem::offset_of!(RawSystemTable, firmware_revision) == 32);
    assert!(core::mem::offset_of!(RawSystemTable, con_out) == 64);
    assert!(core::mem::offset_of!(RawSystemTable, runtime_services) == 88);
    assert!(core::mem::offset_of!(RawSystemTable, boot_services) == 96);
    assert!(core::mem::offset_of!(RawSystemTable, number_of_table_entries) == 104);
    assert!(core::mem::offset_of!(RawSystemTable, configuration_table) == 112);
};

/// `EFI_CONFIGURATION_TABLE` (UEFI 2.10 §4.6).
#[repr(C)]
pub struct ConfigurationTable {
    pub guid: Guid,
    pub address: *const c_void,
}

const _: () = assert!(size_of::<ConfigurationTable>() == 24);
const _: () = assert!(core::mem::offset_of!(ConfigurationTable, address) == 16);

/// `EFI_ACPI_20_TABLE_GUID` (UEFI 2.10 §4.6.1).
pub const ACPI2_GUID: Guid = Guid::new(0x8868e871, 0xe4f1, 0x11d3, [0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81]);

/// The system table firmware handed `efi_main`, while boot services live:
/// null before the entry and from [`SystemTable::exit_boot_services`] on. The
/// console and the allocator read it; nothing else does.
static SYSTEM: AtomicPtr<RawSystemTable> = AtomicPtr::new(ptr::null_mut());

/// Set before the first `ExitBootServices` call: from then on only the memory
/// allocation services may be called, even where the call fails (UEFI 2.10
/// §7.4.6), so firmware's console is not written again.
static EXITING: AtomicBool = AtomicBool::new(false);

/// This image's handle, the agent every protocol open names.
static IMAGE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

/// The system table while boot services live: made once, by the entry, and
/// consumed by [`SystemTable::exit_boot_services`].
pub struct SystemTable {
    raw: &'static RawSystemTable,
}

impl SystemTable {
    pub fn boot_services(&self) -> &BootServices {
        // SAFETY: firmware's table names its boot services for as long as they
        // live, which is as long as `self`.
        unsafe { &*self.raw.boot_services }
    }

    pub fn runtime_services(&self) -> &RuntimeServices {
        // SAFETY: as `boot_services`; runtime services outlive them.
        unsafe { &*self.raw.runtime_services }
    }

    pub fn config_table(&self) -> &[ConfigurationTable] {
        // SAFETY: firmware's table names this many entries at this address.
        unsafe { core::slice::from_raw_parts(self.raw.configuration_table, self.raw.number_of_table_entries) }
    }

    /// `ClearScreen` (UEFI 2.10 §12.4.8), which also homes the cursor.
    pub fn clear_screen(&self) -> Result<(), Status> {
        let out = self.raw.con_out;
        // SAFETY: `ConOut` is firmware's console for as long as boot services live.
        unsafe { ((*out).clear_screen)(out) }.ok()
    }

    /// `ExitBootServices` (UEFI 2.10 §7.4.6) on the map it is handed, taken
    /// into pool memory it never frees; tried twice, as a map key gone stale
    /// between the two calls is answered by asking again, and a machine that
    /// refuses twice is reset. The console and the allocator refuse from here.
    pub fn exit_boot_services(self) -> MemoryMap<'static> {
        let bs = self.boot_services();
        let reset = |status: Status| -> ! { self.runtime_services().reset(ResetType::COLD, status) };
        // Eight descriptors past the measured map, for the ones the pool
        // allocation below adds.
        let (map_size, entry_size) = bs.memory_map_size();
        let Some(bytes) = entry_size.checked_mul(8).and_then(|extra| map_size.checked_add(extra)) else {
            reset(Status::ABORTED)
        };
        let words = bytes.div_ceil(size_of::<u64>());
        let buffer = match bs.allocate_pool(words * size_of::<u64>()) {
            // SAFETY: the pool gives 8-byte-aligned memory (§7.2.4) of the
            // size asked, never freed: it is this map's for good.
            Ok(at) => unsafe { core::slice::from_raw_parts_mut(at.cast::<u64>(), words) },
            Err(status) => reset(status),
        };
        let image = image();
        let mut status = Status::ABORTED;
        for _ in 0..2 {
            let filled = match bs.fill_memory_map(buffer) {
                Ok(filled) => filled,
                Err(why) => {
                    status = why;
                    continue;
                }
            };
            EXITING.store(true, Ordering::Release);
            status = bs.exit(image, filled.key);
            if status.is_success() {
                SYSTEM.store(ptr::null_mut(), Ordering::Release);
                return MemoryMap::new(buffer, filled);
            }
        }
        reset(status)
    }
}

fn image() -> Handle {
    Handle::from_ptr(IMAGE.load(Ordering::Acquire)).expect("the entry stored this image's handle")
}

/// The system table while boot services live, for the two readers that run
/// without one in hand.
fn live() -> Option<&'static RawSystemTable> {
    // SAFETY: `SYSTEM` is null or the table firmware handed the entry, whose
    // boot services are live until `exit_boot_services` nulls it.
    unsafe { SYSTEM.load(Ordering::Acquire).as_ref() }
}

/// The system table while firmware's console may be written: until the first
/// `ExitBootServices` call.
fn console() -> Option<&'static RawSystemTable> {
    live().filter(|_| !EXITING.load(Ordering::Acquire))
}

/// The image's entry (UEFI 2.10 §4.1, `EFI_IMAGE_ENTRY_POINT`), under the name
/// the UEFI targets link as the PE entry point.
#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(image: *mut c_void, system_table: *mut RawSystemTable) -> Status {
    // SAFETY: firmware hands its system table and this image's handle, live
    // until `ExitBootServices`.
    let raw = unsafe { system_table.as_ref() }.expect("firmware hands the entry its system table");
    let image = Handle::from_ptr(image).expect("firmware hands the entry this image's handle");
    IMAGE.store(image.as_ptr(), Ordering::Release);
    SYSTEM.store(system_table, Ordering::Release);
    crate::main(image, SystemTable { raw })
}

/// One line to firmware's console, `\n` as `\r\n`, in `CHAR16` chunks; a
/// character past UCS-2 is written as U+FFFD.
///
/// # Panics
/// From the first `ExitBootServices` call, and where the console refuses the
/// line.
pub fn print(args: fmt::Arguments) {
    let raw = console().expect("the console is gone with boot services");
    let mut out = Console { out: raw.con_out, units: [0; Console::CHUNK + 1], len: 0 };
    let written = fmt::write(&mut out, args).and_then(|()| out.flush());
    assert!(written.is_ok(), "firmware's console refused a line");
}

struct Console {
    out: *mut proto::TextOutput,
    units: [u16; Console::CHUNK + 1],
    len: usize,
}

impl Console {
    const CHUNK: usize = 128;

    fn push(&mut self, unit: u16) -> fmt::Result {
        self.units[self.len] = unit;
        self.len += 1;
        if self.len == Self::CHUNK { self.flush() } else { Ok(()) }
    }

    fn flush(&mut self) -> fmt::Result {
        if self.len == 0 {
            return Ok(());
        }
        self.units[self.len] = 0;
        self.len = 0;
        // SAFETY: `ConOut` is live while `SYSTEM` is set, and `units` is
        // NUL-terminated at the count just written.
        let status = unsafe { ((*self.out).output_string)(self.out, self.units.as_ptr()) };
        status.ok().map_err(|_| fmt::Error)
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for ch in s.chars() {
            if ch == '\n' {
                self.push(u16::from(b'\r'))?;
            }
            self.push(u16::try_from(u32::from(ch)).unwrap_or(0xFFFD))?;
        }
        Ok(())
    }
}

/// Rust's heap, in `EfiLoaderData` pool memory (UEFI 2.10 §7.2.4: a pool
/// allocation is 8-byte aligned; an application's data is `EfiLoaderData`,
/// §7.4.1), and null once boot services are gone. A wider alignment is cut
/// out of a larger allocation, the allocation's own address kept in the word
/// before it.
struct Pool;

#[global_allocator]
static POOL: Pool = Pool;

/// What `AllocatePool` aligns to.
const POOL_ALIGN: usize = 8;

// SAFETY: every pointer handed out is pool memory of at least the layout's
// size at its alignment, and is given back only through `dealloc`.
unsafe impl core::alloc::GlobalAlloc for Pool {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let Some(raw) = live() else { return ptr::null_mut() };
        // SAFETY: live boot services.
        let bs = unsafe { &*raw.boot_services };
        if layout.align() <= POOL_ALIGN {
            return bs.allocate_pool(layout.size()).unwrap_or(ptr::null_mut());
        }
        let Some(size) = layout.size().checked_add(layout.align()) else { return ptr::null_mut() };
        let Ok(whole) = bs.allocate_pool(size) else { return ptr::null_mut() };
        // At least one word ahead of the aligned address, for the allocation's own.
        let offset = match whole.align_offset(layout.align()) {
            0 => layout.align(),
            n => n,
        };
        // SAFETY: `offset <= align`, inside the `size + align` bytes; the
        // word before the aligned address is inside them too, as
        // `offset >= POOL_ALIGN`, and aligned, as both addresses are.
        unsafe {
            let aligned = whole.add(offset);
            aligned.cast::<*mut u8>().sub(1).write(whole);
            aligned
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        let raw = live().expect("nothing is freed once boot services are gone");
        let whole = if layout.align() <= POOL_ALIGN {
            ptr
        } else {
            // SAFETY: `alloc` wrote the allocation's address in the word before.
            unsafe { ptr.cast::<*mut u8>().sub(1).read() }
        };
        // SAFETY: live boot services, and `whole` is a pool allocation of theirs.
        let freed = unsafe { (*raw.boot_services).free_pool(whole) };
        assert!(freed.is_ok(), "firmware would not take back pool memory it gave");
    }
}

/// Set by the first panic, so a panic while saying one goes straight to the
/// power-off.
static PANICKED: AtomicBool = AtomicBool::new(false);

/// Say the panic on firmware's console and power the machine off; from the
/// first `ExitBootServices` call the console is not written, and once boot
/// services are gone neither is there, and the CPU spins.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    if let Some(raw) = live() {
        if console().is_some() && !PANICKED.swap(true, Ordering::Relaxed) {
            print(format_args!("[PANIC]: {info}\n"));
        }
        // SAFETY: runtime services live as long as the table.
        unsafe { &*raw.runtime_services }.reset(ResetType::SHUTDOWN, Status::ABORTED)
    }
    loop {
        core::hint::spin_loop();
    }
}
