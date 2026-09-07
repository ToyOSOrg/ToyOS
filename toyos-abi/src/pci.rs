//! What a claim on one PCI function hands the process that drives it.
//!
//! The kernel keeps config space, programs the interrupt vector into the
//! function's MSI-X table, and hands out every device address a descriptor may
//! carry ([`syscall::device_dma_alloc`]); the holder gets a register window,
//! buffers it can reach through both, and the interrupt as records on its own
//! claim handle. Nothing here is a physical address, and nothing here is
//! specific to what the function *is*.
//!
//! [`syscall::device_dma_alloc`]: crate::syscall::device_dma_alloc

/// The six BAR slots a Type 0 PCI header has (PCI 3.0 §6.1), and the bound
/// every index in this module is checked against.
pub const BARS: usize = 6;

/// The function the claim names, as its driver needs to see it before it has
/// mapped anything.
///
/// No addresses: a BAR's *size* is what a driver bounds its own accesses with,
/// and where the window sits is [`syscall::device_bar_map`]'s answer, so the
/// kernel stays free to move a BAR to a boundary it can map.
///
/// [`syscall::device_bar_map`]: crate::syscall::device_bar_map
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PciFunctionInfo {
    /// Byte size of each memory BAR; 0 where the function has none in that
    /// slot, or where it is one the kernel will not map — the BAR holding this
    /// function's MSI-X table or PBA reads 0 here.
    pub bar_bytes: [u64; BARS],
    pub vendor: u16,
    pub device: u16,
    /// Where firmware put it. Identity for a log line, never a name a claim is
    /// looked up by: nothing in this ABI takes a bus/device/function.
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    /// 1 when the kernel armed an interrupt for this function, 0 when it has
    /// none. A claim is never minted for a function whose interrupt could not
    /// be armed.
    pub irq: u8,
}

/// Every byte belongs to a field: this crosses the boundary through
/// `as_bytes`, so a gap would publish whatever the kernel stack held.
const _: () = {
    let named = BARS * 8 + 2 + 2 + 1 + 1 + 1 + 1;
    assert!(core::mem::size_of::<PciFunctionInfo>() == named);
};

impl PciFunctionInfo {
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `self` is a valid `&Self` (non-null, aligned, readable for
        // `size_of::<Self>()` bytes), and the const assert above proves the
        // `repr(C)` layout has no padding, so every byte the slice exposes is
        // an initialized field, not a gap.
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }
}

/// One DMA buffer, in the two address spaces it exists in.
///
/// Both, because neither can be derived from the other: `shm` maps where this
/// process's loads and stores reach the bytes, and `device_addr` is what a
/// descriptor must carry for the function to reach the same bytes through the
/// unit.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DmaGrant {
    /// The memory, as an object to map; the caller owns this handle.
    pub shm: crate::RawHandle,
    pub _pad: u32,
    /// Never a physical address once a unit translates for this function.
    pub device_addr: u64,
    /// The request rounded up to whole 2 MiB pages.
    pub bytes: u64,
}

const _: () = assert!(core::mem::size_of::<DmaGrant>() == 4 + 4 + 8 + 8);

/// The interrupts that landed since the last read of a claim.
///
/// One record and not a queue: the kernel accumulates, so a driver that slept
/// through several is told about all of them at once and there is no ring for a
/// slow reader to overflow. The count says how many messages arrived, never
/// what any of them meant.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DeviceIrqRecord {
    /// Never 0 in a record that was answered; an empty count is `WouldBlock`.
    pub count: u32,
    pub _pad: u32,
    /// When the most recent of them was taken, by the same clock `SYS_CLOCK`
    /// answers. The most recent and not the first, because a field the kernel
    /// overwrites per message is set for every count a reader can observe.
    pub timestamp_nanos: u64,
}

impl DeviceIrqRecord {
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

const _: () = assert!(DeviceIrqRecord::SIZE == 4 + 4 + 8);
