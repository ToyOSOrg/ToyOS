//! The protocols this loader opens, each in its spec section's own field
//! order, and the console it writes through.

use core::ffi::c_void;
use core::mem::offset_of;
use core::ptr::{self, NonNull};

use alloc::vec;

use super::boot::{Exclusive, Protocol};
use super::{CStr16, Guid, Handle, Status};

type Unused = *const c_void;

/// `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL` (UEFI 2.10 §12.4.1), firmware's console:
/// reached through the system table's `ConOut`, never opened.
#[repr(C)]
pub(super) struct TextOutput {
    reset: Unused,
    pub(super) output_string: unsafe extern "efiapi" fn(this: *mut TextOutput, string: *const u16) -> Status,
    test_string: Unused,
    query_mode: Unused,
    set_mode: Unused,
    set_attribute: Unused,
    pub(super) clear_screen: unsafe extern "efiapi" fn(this: *mut TextOutput) -> Status,
    set_cursor_position: Unused,
    enable_cursor: Unused,
    mode: Unused,
}

const _: () = {
    assert!(size_of::<TextOutput>() == 80);
    assert!(offset_of!(TextOutput, output_string) == 8);
    assert!(offset_of!(TextOutput, clear_screen) == 48);
};

/// `EFI_LOADED_IMAGE_PROTOCOL` (UEFI 2.10 §9.1.1).
#[repr(C)]
pub struct LoadedImage {
    revision: u32,
    parent_handle: *mut c_void,
    system_table: Unused,
    device_handle: *mut c_void,
    file_path: Unused,
    reserved: Unused,
    load_options_size: u32,
    load_options: Unused,
    image_base: *const c_void,
    image_size: u64,
    image_code_type: u32,
    image_data_type: u32,
    unload: Unused,
}

const _: () = {
    assert!(size_of::<LoadedImage>() == 96);
    assert!(offset_of!(LoadedImage, device_handle) == 24);
    assert!(offset_of!(LoadedImage, load_options_size) == 48);
    assert!(offset_of!(LoadedImage, image_base) == 64);
    assert!(offset_of!(LoadedImage, image_size) == 72);
    assert!(offset_of!(LoadedImage, image_data_type) == 84);
};

// SAFETY: §9.1.1's layout, under its GUID.
unsafe impl Protocol for LoadedImage {
    const GUID: Guid = Guid::new(0x5b1b31a1, 0x9562, 0x11d2, [0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);
}
impl Exclusive for LoadedImage {}

impl LoadedImage {
    /// The handle of the device firmware loaded this image from, if it says.
    pub fn device(&self) -> Option<Handle> {
        Handle::from_ptr(self.device_handle)
    }

    /// Where firmware loaded the image, and its size in bytes.
    pub fn info(&self) -> (*const c_void, u64) {
        (self.image_base, self.image_size)
    }
}

/// `EFI_DEVICE_PATH_PROTOCOL` (UEFI 2.10 §10.2): the first node's header,
/// which the rest of the path follows.
#[repr(C)]
pub struct DevicePath {
    ty: u8,
    sub_type: u8,
    length: [u8; 2],
}

const _: () = assert!(size_of::<DevicePath>() == 4);

// SAFETY: §10.2's layout, under its GUID.
unsafe impl Protocol for DevicePath {
    const GUID: Guid = Guid::new(0x09576e91, 0x6d3f, 0x11d2, [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);
}
impl Exclusive for DevicePath {}

/// A device path node's header: type, subtype, and a length that counts it.
const NODE_HEADER: usize = 4;

/// End of Entire Device Path (§10.3.1).
const END_ENTIRE: (u8, u8) = (0x7f, 0xff);

impl DevicePath {
    /// Each node, whole, up to the End of Entire node.
    ///
    /// # Panics
    /// Where a node's length does not cover its own header: the walk could
    /// not step past it.
    pub fn nodes(&self) -> impl Iterator<Item = &[u8]> {
        let mut at = ptr::from_ref(self).cast::<u8>();
        core::iter::from_fn(move || {
            // SAFETY: firmware's path is a run of nodes ending in End of
            // Entire, and `at` is the start of one of them.
            let header = unsafe { core::slice::from_raw_parts(at, NODE_HEADER) };
            if (header[0], header[1]) == END_ENTIRE {
                return None;
            }
            let len = usize::from(u16::from_le_bytes([header[2], header[3]]));
            assert!(len >= NODE_HEADER, "firmware's device path has a {len}-byte node");
            // SAFETY: the node's own length, which counts its header.
            let node = unsafe { core::slice::from_raw_parts(at, len) };
            // SAFETY: the next node follows this one.
            at = unsafe { at.add(len) };
            Some(node)
        })
    }
}

/// The MEDIA/HARDDRIVE node (§10.3.5.1).
pub struct HardDrive {
    pub start: u64,
    pub size: u64,
    pub signature: [u8; 16],
    /// `MBRType`: 2 for a GPT partition.
    pub format: u8,
    /// `SignatureType`: 2 for a GUID signature.
    pub signature_type: u8,
}

impl HardDrive {
    pub const TYPE: (u8, u8) = (4, 1);
    /// `MBRType` of a GPT partition.
    pub const GPT: u8 = 2;
    /// `SignatureType` of a GUID signature.
    pub const GUID_SIGNATURE: u8 = 2;
    const LEN: usize = 42;

    /// `node`, where it is a HARDDRIVE node of the length §10.3.5.1 gives it.
    pub fn parse(node: &[u8]) -> Option<HardDrive> {
        if node.len() != Self::LEN || (node[0], node[1]) != Self::TYPE {
            return None;
        }
        let u64_at = |at: usize| u64::from_le_bytes(node[at..at + 8].try_into().expect("eight bytes"));
        Some(HardDrive {
            start: u64_at(8),
            size: u64_at(16),
            signature: node[24..40].try_into().expect("sixteen bytes"),
            format: node[40],
            signature_type: node[41],
        })
    }
}

/// `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL` (UEFI 2.10 §13.4.1).
#[repr(C)]
pub struct SimpleFileSystem {
    revision: u64,
    open_volume: unsafe extern "efiapi" fn(this: *mut SimpleFileSystem, root: *mut *mut FileProtocol) -> Status,
}

const _: () = assert!(size_of::<SimpleFileSystem>() == 16);

// SAFETY: §13.4.1's layout, under its GUID.
unsafe impl Protocol for SimpleFileSystem {
    const GUID: Guid = Guid::new(0x964e5b22, 0x6459, 0x11d2, [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);
}
impl Exclusive for SimpleFileSystem {}

impl super::Scoped<'_, SimpleFileSystem> {
    /// `OpenVolume` (§13.4.2): the volume's root directory.
    pub fn open_volume(&mut self) -> Result<File, Status> {
        let mut root = ptr::null_mut();
        // SAFETY: the open interface, and a live out parameter.
        unsafe { (self.open_volume)(self.this(), &mut root) }.ok()?;
        Ok(File(NonNull::new(root).expect("OpenVolume answered success with no root")))
    }
}

/// `EFI_FILE_PROTOCOL` (UEFI 2.10 §13.5.1), through `Flush`: the revision 2
/// functions after it are never called.
#[repr(C)]
pub struct FileProtocol {
    revision: u64,
    open: unsafe extern "efiapi" fn(
        this: *mut FileProtocol,
        new: *mut *mut FileProtocol,
        name: *const u16,
        mode: u64,
        attributes: u64,
    ) -> Status,
    close: unsafe extern "efiapi" fn(this: *mut FileProtocol) -> Status,
    delete: unsafe extern "efiapi" fn(this: *mut FileProtocol) -> Status,
    read: unsafe extern "efiapi" fn(this: *mut FileProtocol, size: *mut usize, buffer: *mut u8) -> Status,
    write: unsafe extern "efiapi" fn(this: *mut FileProtocol, size: *mut usize, buffer: *const u8) -> Status,
    get_position: Unused,
    set_position: unsafe extern "efiapi" fn(this: *mut FileProtocol, position: u64) -> Status,
    get_info: unsafe extern "efiapi" fn(
        this: *mut FileProtocol,
        kind: *const Guid,
        size: *mut usize,
        buffer: *mut u8,
    ) -> Status,
    set_info: Unused,
    flush: unsafe extern "efiapi" fn(this: *mut FileProtocol) -> Status,
}

const _: () = {
    assert!(size_of::<FileProtocol>() == 88);
    assert!(offset_of!(FileProtocol, open) == 8);
    assert!(offset_of!(FileProtocol, close) == 16);
    assert!(offset_of!(FileProtocol, delete) == 24);
    assert!(offset_of!(FileProtocol, read) == 32);
    assert!(offset_of!(FileProtocol, write) == 40);
    assert!(offset_of!(FileProtocol, set_position) == 56);
    assert!(offset_of!(FileProtocol, get_info) == 64);
    assert!(offset_of!(FileProtocol, flush) == 80);
};

/// `Open`'s modes (§13.5.2).
#[derive(Clone, Copy)]
pub enum Mode {
    Read,
    ReadWrite,
    CreateReadWrite,
}

impl Mode {
    fn bits(self) -> u64 {
        const READ: u64 = 0x1;
        const WRITE: u64 = 0x2;
        const CREATE: u64 = 0x8000_0000_0000_0000;
        match self {
            Mode::Read => READ,
            Mode::ReadWrite => READ | WRITE,
            Mode::CreateReadWrite => CREATE | READ | WRITE,
        }
    }
}

/// An open file or directory, closed when this drops.
pub struct File(NonNull<FileProtocol>);

/// `EFI_FILE_INFO` (§13.5.16), up to the name this loader never reads.
#[repr(C)]
struct RawFileInfo {
    size: u64,
    file_size: u64,
    physical_size: u64,
    create_time: super::Time,
    last_access_time: super::Time,
    modification_time: super::Time,
    attribute: u64,
}

const _: () = {
    assert!(size_of::<RawFileInfo>() == 80);
    assert!(offset_of!(RawFileInfo, file_size) == 8);
    assert!(offset_of!(RawFileInfo, attribute) == 72);
};

/// `EFI_FILE_INFO_ID` (§13.5.16).
const FILE_INFO: Guid = Guid::new(0x09576e92, 0x6d3f, 0x11d2, [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);

/// `EFI_FILE_DIRECTORY` (§13.5.16).
const DIRECTORY: u64 = 0x10;

/// What this loader reads of a file's `EFI_FILE_INFO`.
pub struct FileInfo {
    pub file_size: u64,
    pub directory: bool,
}

impl File {
    fn call<R>(&mut self, f: impl FnOnce(&FileProtocol, *mut FileProtocol) -> R) -> R {
        // SAFETY: an open handle's interface, live until it is closed.
        f(unsafe { self.0.as_ref() }, self.0.as_ptr())
    }

    /// `Open` (§13.5.2), relative to this directory.
    pub fn open(&mut self, name: &CStr16, mode: Mode) -> Result<File, Status> {
        let mut new = ptr::null_mut();
        // SAFETY: a NUL-terminated name and a live out parameter.
        self.call(|f, this| unsafe { (f.open)(this, &mut new, name.as_ptr(), mode.bits(), 0) }).ok()?;
        Ok(File(NonNull::new(new).expect("Open answered success with no handle")))
    }

    /// `Delete` (§13.5.4), which closes the handle whatever it answers.
    pub fn delete(mut self) -> Result<(), Status> {
        // SAFETY: the handle, which `Delete` closes, so `Drop` must not.
        let status = self.call(|f, this| unsafe { (f.delete)(this) });
        #[expect(clippy::disallowed_methods, reason = "`Delete` closed the handle `Drop` would close again")]
        core::mem::forget(self);
        status.ok()
    }

    /// `Read` (§13.5.5): the bytes read, at most `buffer`'s length.
    pub fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Status> {
        let mut size = buffer.len();
        // SAFETY: `buffer` is `size` writable bytes.
        self.call(|f, this| unsafe { (f.read)(this, &mut size, buffer.as_mut_ptr()) }).ok()?;
        assert!(size <= buffer.len(), "Read answered {size} bytes into {}", buffer.len());
        Ok(size)
    }

    /// `Write` (§13.5.6): on a refusal, the status and the bytes written
    /// before it.
    pub fn write(&mut self, buffer: &[u8]) -> Result<(), (Status, usize)> {
        let mut size = buffer.len();
        // SAFETY: `buffer` is `size` readable bytes.
        self.call(|f, this| unsafe { (f.write)(this, &mut size, buffer.as_ptr()) }).ok().map_err(|status| (status, size))
    }

    /// `SetPosition` (§13.5.8); `u64::MAX` is the end of the file.
    pub fn set_position(&mut self, position: u64) -> Result<(), Status> {
        // SAFETY: a plain call on the handle.
        self.call(|f, this| unsafe { (f.set_position)(this, position) }).ok()
    }

    /// `Flush` (§13.5.11).
    pub fn flush(&mut self) -> Result<(), Status> {
        // SAFETY: a plain call on the handle.
        self.call(|f, this| unsafe { (f.flush)(this) }).ok()
    }

    /// `GetInfo` (§13.5.12) of `EFI_FILE_INFO`, sized by asking first.
    pub fn info(&mut self) -> Result<FileInfo, Status> {
        let mut size = 0usize;
        // SAFETY: a zero size with no buffer asks for the size.
        let status = self.call(|f, this| unsafe { (f.get_info)(this, &FILE_INFO, &mut size, ptr::null_mut()) });
        if status != Status::BUFFER_TOO_SMALL {
            return Err(if status.is_success() { Status::BUFFER_TOO_SMALL } else { status });
        }
        assert!(size >= size_of::<RawFileInfo>(), "GetInfo wants {size} bytes for an EFI_FILE_INFO");
        let mut words = vec![0u64; size.div_ceil(size_of::<u64>())];
        // SAFETY: `words` is at least `size` writable bytes, aligned for the info.
        self.call(|f, this| unsafe { (f.get_info)(this, &FILE_INFO, &mut size, words.as_mut_ptr().cast()) }).ok()?;
        // SAFETY: firmware wrote an `EFI_FILE_INFO`, whose head is this.
        let info = unsafe { words.as_ptr().cast::<RawFileInfo>().read() };
        Ok(FileInfo { file_size: info.file_size, directory: info.attribute & DIRECTORY != 0 })
    }

    /// This handle, where it is a file and not a directory; `None` for a
    /// directory, and for a handle that would not say which it is.
    pub fn into_regular_file(mut self) -> Option<File> {
        match self.info() {
            Ok(FileInfo { directory: false, .. }) => Some(self),
            _ => None,
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        // SAFETY: the handle, closed once. `Close` always succeeds (§13.5.3).
        let _ = self.call(|f, this| unsafe { (f.close)(this) });
    }
}

/// `EFI_PARTITION_INFO_PROTOCOL` (UEFI 2.10 §13.18), packed as the spec
/// declares it; the record is MBR or GPT by `kind`, read as bytes.
#[repr(C, packed)]
pub struct PartitionInfo {
    revision: u32,
    kind: u32,
    system: u8,
    reserved: [u8; 7],
    record: [u8; 128],
}

const _: () = {
    assert!(size_of::<PartitionInfo>() == 144);
    assert!(offset_of!(PartitionInfo, kind) == 4);
    assert!(offset_of!(PartitionInfo, system) == 8);
    assert!(offset_of!(PartitionInfo, record) == 16);
};

// SAFETY: §13.18's layout, under its GUID.
unsafe impl Protocol for PartitionInfo {
    const GUID: Guid = Guid::new(0x8cf2f62c, 0xbc9b, 0x4821, [0x80, 0x8d, 0xec, 0x9e, 0xc4, 0x21, 0xa1, 0xa0]);
}

impl PartitionInfo {
    /// `EFI_PARTITION_INFO_PROTOCOL_REVISION`, which the spec spells `0x0001000`.
    const REVISION: u32 = 0x1000;
    /// `PARTITION_TYPE_GPT`.
    const GPT: u32 = 0x02;

    /// The unique partition GUID of the GPT entry this carries (UEFI 2.10
    /// §5.3.3, bytes 16 to 32 of the entry), or `None` for any other record.
    pub fn gpt_unique_guid(&self) -> Option<[u8; 16]> {
        let (revision, kind) = (self.revision, self.kind);
        if revision != Self::REVISION || kind != Self::GPT {
            return None;
        }
        let record = self.record;
        Some(record[16..32].try_into().expect("sixteen bytes"))
    }
}

/// `EFI_BLOCK_IO_PROTOCOL` (UEFI 2.10 §13.9.1).
#[repr(C)]
pub struct BlockIo {
    pub revision: u64,
    media: *const BlockIoMedia,
    reset: Unused,
    read_blocks: unsafe extern "efiapi" fn(
        this: *mut BlockIo,
        media_id: u32,
        lba: u64,
        size: usize,
        buffer: *mut u8,
    ) -> Status,
    write_blocks: Unused,
    flush_blocks: Unused,
}

const _: () = {
    assert!(size_of::<BlockIo>() == 48);
    assert!(offset_of!(BlockIo, media) == 8);
    assert!(offset_of!(BlockIo, read_blocks) == 24);
};

// SAFETY: §13.9.1's layout, under its GUID.
unsafe impl Protocol for BlockIo {
    const GUID: Guid = Guid::new(0x964e5b21, 0x6459, 0x11d2, [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);
}

/// `EFI_BLOCK_IO_MEDIA` (§13.9.1); `optimal_transfer_length_granularity` is
/// there only from `EFI_BLOCK_IO_PROTOCOL_REVISION3` on.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct BlockIoMedia {
    pub media_id: u32,
    removable_media: u8,
    media_present: u8,
    logical_partition: u8,
    read_only: u8,
    write_caching: u8,
    pub block_size: u32,
    pub io_align: u32,
    pub last_block: u64,
}

/// The revision 2 and 3 fields after `LastBlock`.
#[repr(C)]
struct BlockIoMediaRevision3 {
    head: BlockIoMedia,
    lowest_aligned_lba: u64,
    logical_blocks_per_physical_block: u32,
    optimal_transfer_length_granularity: u32,
}

const _: () = {
    assert!(size_of::<BlockIoMedia>() == 32);
    assert!(offset_of!(BlockIoMedia, media_present) == 5);
    assert!(offset_of!(BlockIoMedia, block_size) == 12);
    assert!(offset_of!(BlockIoMedia, io_align) == 16);
    assert!(offset_of!(BlockIoMedia, last_block) == 24);
    assert!(size_of::<BlockIoMediaRevision3>() == 48);
    assert!(offset_of!(BlockIoMediaRevision3, optimal_transfer_length_granularity) == 44);
};

impl BlockIoMedia {
    pub fn is_media_present(&self) -> bool {
        self.media_present != 0
    }
}

impl BlockIo {
    /// The first revision whose media carries `OptimalTransferLengthGranularity`.
    pub const REVISION3: u64 = 0x0002_001f;

    /// The media as firmware describes it now, through `LastBlock`.
    pub fn media(&self) -> BlockIoMedia {
        // SAFETY: every revision's media begins with this head.
        unsafe { self.media.read() }
    }

    /// `OptimalTransferLengthGranularity`, where the revision carries it.
    pub fn optimal_transfer_length_granularity(&self) -> Option<u32> {
        if self.revision < Self::REVISION3 {
            return None;
        }
        // SAFETY: a revision 3 media carries the field.
        Some(unsafe { (*self.media.cast::<BlockIoMediaRevision3>()).optimal_transfer_length_granularity })
    }
}

impl super::Scoped<'_, BlockIo> {
    /// `ReadBlocks` (§13.9.3): `buffer`'s length in blocks from `lba`.
    pub fn read_blocks(&self, media_id: u32, lba: u64, buffer: &mut [u8]) -> Result<(), Status> {
        // SAFETY: the open interface, and `buffer` is its length of writable bytes.
        unsafe { (self.read_blocks)(self.this(), media_id, lba, buffer.len(), buffer.as_mut_ptr()) }.ok()
    }
}

/// `EFI_GRAPHICS_OUTPUT_PROTOCOL` (UEFI 2.10 §12.9.2).
#[repr(C)]
pub struct Gop {
    query_mode: Unused,
    set_mode: Unused,
    blt: Unused,
    mode: *const GopMode,
}

/// `EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE` (§12.9.2).
#[repr(C)]
struct GopMode {
    max_mode: u32,
    mode: u32,
    info: *const GopModeInfo,
    size_of_info: usize,
    frame_buffer_base: u64,
    frame_buffer_size: usize,
}

/// `EFI_GRAPHICS_OUTPUT_MODE_INFORMATION` (§12.9.2).
#[repr(C)]
pub struct GopModeInfo {
    version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    pub pixel_format: PixelFormat,
    pixel_information: [u32; 4],
    pub pixels_per_scan_line: u32,
}

const _: () = {
    assert!(size_of::<Gop>() == 32);
    assert!(offset_of!(Gop, mode) == 24);
    assert!(size_of::<GopMode>() == 40);
    assert!(offset_of!(GopMode, info) == 8);
    assert!(offset_of!(GopMode, frame_buffer_base) == 24);
    assert!(offset_of!(GopMode, frame_buffer_size) == 32);
    assert!(size_of::<GopModeInfo>() == 36);
    assert!(offset_of!(GopModeInfo, pixel_format) == 12);
    assert!(offset_of!(GopModeInfo, pixels_per_scan_line) == 32);
};

/// `EFI_GRAPHICS_PIXEL_FORMAT` (§12.9.2), as the raw word firmware wrote.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct PixelFormat(pub u32);

impl PixelFormat {
    pub const RGB: PixelFormat = PixelFormat(0);
    pub const BGR: PixelFormat = PixelFormat(1);
    pub const BIT_MASK: PixelFormat = PixelFormat(2);
    pub const BLT_ONLY: PixelFormat = PixelFormat(3);
}

impl core::fmt::Debug for PixelFormat {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::RGB => f.write_str("PixelRedGreenBlueReserved8BitPerColor"),
            Self::BGR => f.write_str("PixelBlueGreenRedReserved8BitPerColor"),
            Self::BIT_MASK => f.write_str("PixelBitMask"),
            Self::BLT_ONLY => f.write_str("PixelBltOnly"),
            PixelFormat(other) => write!(f, "PixelFormat({other})"),
        }
    }
}

// SAFETY: §12.9.2's layout, under its GUID.
unsafe impl Protocol for Gop {
    const GUID: Guid = Guid::new(0x9042a9de, 0x23dc, 0x4a38, [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a]);
}

impl Gop {
    /// The mode firmware set.
    pub fn current_mode_info(&self) -> &GopModeInfo {
        // SAFETY: an open GOP names its mode and the mode its information.
        unsafe { &*(*self.mode).info }
    }

    /// The frame buffer's physical base and size in bytes.
    pub fn frame_buffer(&self) -> (u64, u64) {
        // SAFETY: an open GOP names its mode.
        let mode = unsafe { &*self.mode };
        (mode.frame_buffer_base, mode.frame_buffer_size as u64)
    }
}

/// `EFI_RNG_PROTOCOL` (UEFI 2.10 §37.5.1).
#[repr(C)]
pub struct Rng {
    get_info: Unused,
    get_rng: unsafe extern "efiapi" fn(this: *mut Rng, algorithm: *const Guid, size: usize, value: *mut u8) -> Status,
}

const _: () = assert!(size_of::<Rng>() == 16);

// SAFETY: §37.5.1's layout, under its GUID.
unsafe impl Protocol for Rng {
    const GUID: Guid = Guid::new(0x3152bca5, 0xeade, 0x433d, [0x86, 0x2e, 0xc0, 0x1c, 0xdc, 0x29, 0x1f, 0x44]);
}

impl super::Scoped<'_, Rng> {
    /// `GetRNG` (§37.5.3) with no algorithm named: firmware's default.
    pub fn get_rng(&self, into: &mut [u8]) -> Result<(), Status> {
        // SAFETY: the open interface, and `into` is its length of writable bytes.
        unsafe { (self.get_rng)(self.this(), ptr::null(), into.len(), into.as_mut_ptr()) }.ok()
    }
}

/// `EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL` (UEFI 2.10 §14.2.1).
#[repr(C)]
pub struct PciRootBridgeIo {
    parent_handle: Unused,
    poll_mem: Unused,
    poll_io: Unused,
    mem_read: Unused,
    mem_write: Unused,
    io_read: Unused,
    io_write: Unused,
    pci_read: Unused,
    pci_write: Unused,
    copy_mem: Unused,
    map: Unused,
    unmap: Unused,
    allocate_buffer: Unused,
    free_buffer: Unused,
    flush: Unused,
    get_attributes: Unused,
    set_attributes: Unused,
    configuration: unsafe extern "efiapi" fn(this: *mut PciRootBridgeIo, resources: *mut *const c_void) -> Status,
    pub segment_number: u32,
}

const _: () = {
    assert!(size_of::<PciRootBridgeIo>() == 18 * 8 + 8);
    assert!(offset_of!(PciRootBridgeIo, configuration) == 17 * 8);
    assert!(offset_of!(PciRootBridgeIo, segment_number) == 18 * 8);
};

// SAFETY: §14.2.1's layout, under its GUID.
unsafe impl Protocol for PciRootBridgeIo {
    const GUID: Guid = Guid::new(0x2f707ebb, 0x4a1a, 0x11d4, [0x9a, 0x38, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d]);
}

impl super::Scoped<'_, PciRootBridgeIo> {
    /// `Configuration()` (§14.2.13): the bridge's ACPI resource descriptors,
    /// in memory firmware owns.
    pub fn configuration(&self) -> Result<*const c_void, Status> {
        let mut resources = ptr::null();
        // SAFETY: the open interface, and a live out parameter.
        unsafe { (self.configuration)(self.this(), &mut resources) }.ok().map(|()| resources)
    }
}
