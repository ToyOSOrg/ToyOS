//! What a claim on one PCI function hands the process that drives it.
//!
//! The kernel keeps config space, programs the interrupt vector into the
//! function's MSI-X table, and hands out every device address a descriptor may
//! carry ([`syscall::device_dma_alloc`], and [`syscall::device_dma_map`] for a
//! region the holder lends the function); the holder gets a register window,
//! buffers it can reach through both, and the interrupt as records on its own
//! claim handle. Nothing here is a physical address, and nothing here is
//! specific to what the function *is*.
//!
//! **The function masters the bus from [`syscall::device_dma_start`] and not
//! from the hand-over**, so a driver stops and resets it through its window,
//! states its address width ([`syscall::device_dma_limit`]) and takes its
//! grants before the device can do anything at all, an interrupt included.
//!
//! [`syscall::device_dma_alloc`]: crate::syscall::device_dma_alloc
//! [`syscall::device_dma_map`]: crate::syscall::device_dma_map
//! [`syscall::device_dma_start`]: crate::syscall::device_dma_start
//! [`syscall::device_dma_limit`]: crate::syscall::device_dma_limit

/// The six BAR slots a Type 0 PCI header has (PCI 3.0 §6.1), and the bound
/// every index in this module is checked against.
pub const BARS: usize = 6;

crate::user_safe! {
    /// The function the claim names, as its driver needs to see it before it has
    /// mapped anything.
    ///
    /// No addresses: a BAR's *size* is what a driver bounds its own accesses with,
    /// and where the window sits is [`syscall::device_bar_map`]'s answer.
    ///
    /// [`syscall::device_bar_map`]: crate::syscall::device_bar_map
    #[derive(Clone, Copy)]
    pub struct PciFunctionInfo {
        /// Byte size of each memory BAR; 0 where the function has none in that
        /// slot, or where it is one the kernel will not map.
        pub bar_bytes: [u64; BARS],
        /// Where firmware put it. Identity for a log line, never a name a claim is
        /// looked up by: nothing in this ABI takes a bus/device/function.
        pub bus: u8,
        pub dev: u8,
        pub func: u8,
        /// Written, never read: the tail `repr(C)` would otherwise leave as a gap.
        pub _pad: [u8; 5],
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

/// A window into memory firmware set up for a function, granted only by the
/// signed image's manifest (`firmware = [...]`).
#[repr(u64)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FirmwareWindow {
    /// The IGD OpRegion of an Intel graphics function, at its ASLS register.
    IgdOpRegion = 1,
}

impl FirmwareWindow {
    pub const fn from_raw(raw: u64) -> Option<Self> {
        match raw {
            1 => Some(Self::IgdOpRegion),
            _ => None,
        }
    }
}

/// The firmware windows a PCI function claim is granted at its mint, one bit
/// per [`FirmwareWindow`], carried in the mint's second selector word.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FirmwareWindows(u64);

impl FirmwareWindows {
    pub const NONE: Self = Self(0);
    const KNOWN: u64 = 1 << FirmwareWindow::IgdOpRegion as u64;

    pub fn of(windows: &[FirmwareWindow]) -> Self {
        Self(windows.iter().fold(0, |mask, &window| mask | 1 << window as u64))
    }

    /// The mask off the wire, or `None` naming a window there is none of.
    pub const fn from_mask(mask: u64) -> Option<Self> {
        if mask & !Self::KNOWN != 0 { None } else { Some(Self(mask)) }
    }

    pub const fn mask(self) -> u64 {
        self.0
    }

    pub fn contains(self, window: FirmwareWindow) -> bool {
        self.0 & 1 << window as u64 != 0
    }

    pub fn iter(self) -> impl Iterator<Item = FirmwareWindow> {
        [FirmwareWindow::IgdOpRegion].into_iter().filter(move |&window| self.contains(window))
    }
}

/// A firmware window as [`crate::syscall::device_firmware_window`] answers
/// it: whole pages in `shm`, and the window `offset` bytes into them.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WindowGrant {
    /// The pages, as an object to map; the caller owns this handle.
    pub shm: crate::RawHandle,
    /// Where the kernel refused the window, a [`WindowRefused`]; 0
    /// otherwise.
    pub refused: u32,
    pub offset: u64,
    /// The window's length; its pages run to `offset + len`, rounded up.
    pub len: u64,
}

const _: () = assert!(core::mem::size_of::<WindowGrant>() == 4 + 4 + 8 + 8);

/// Why the kernel refused a firmware window firmware set up.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowRefused {
    /// The header does not carry the window's signature.
    Signature = 1,
    /// The size its header gives is none the window can be.
    Size = 2,
    /// A page of it is not memory firmware keeps for itself.
    NotFirmware = 3,
    /// A page of it is memory the kernel hands out.
    Usable = 4,
    /// A page of it is one a device decodes.
    Device = 5,
}

impl WindowRefused {
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            1 => Some(Self::Signature),
            2 => Some(Self::Size),
            3 => Some(Self::NotFirmware),
            4 => Some(Self::Usable),
            5 => Some(Self::Device),
            _ => None,
        }
    }
}

/// Where a function reaches a region its holder mapped into its address space
/// ([`crate::syscall::device_dma_map`]).
///
/// No handle beside it: the region is the caller's already, and this is only
/// the second of the two addresses it now has.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaMapping {
    /// Never a physical address: a function with no unit translating for it is
    /// never handed to a process at all.
    pub device_addr: u64,
    /// The region's whole length, which is whole 2 MiB pages.
    pub bytes: u64,
}

const _: () = assert!(core::mem::size_of::<DmaMapping>() == 8 + 8);

/// The interrupts that landed since the last read of a claim.
///
/// One record and not a queue: the kernel accumulates, so a driver that slept
/// through several is told about all of them at once and there is no ring for a
/// slow reader to overflow. The count says how many messages arrived, never
/// what any of them meant: that is in the device's own rings.
///
/// **The two times are when messages landed, which a driver reading its rings
/// later cannot recover**: a driver clocked by its device — soundserver's DLL —
/// is clocked by these and not by when it woke. Both are nanoseconds since
/// boot on the clock page's clock (`crate::clock`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DeviceIrqRecord {
    /// Never 0 in a record that was answered; an empty count is `WouldBlock`.
    pub count: u32,
    pub _pad: u32,
    /// When the first message `count` holds landed.
    pub first_nanos: u64,
    /// When the newest landed: never older than the newest message `count`
    /// holds, and newer only by one that lands as the read is made, which the
    /// next read counts.
    pub last_nanos: u64,
}

impl DeviceIrqRecord {
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

const _: () = assert!(DeviceIrqRecord::SIZE == 4 + 4 + 8 + 8);

/// How many bytes of regions one claim may have lent its function at once
/// ([`crate::syscall::device_dma_map`]), apart from its own grants: a window
/// of the function's address space, so a lend costs addresses and no memory.
pub const LEND_WINDOW: u64 = 256 * 1024 * 1024;
