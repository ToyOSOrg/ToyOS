//! What a claim on the machine's ACPI fixed hardware hands the process that
//! serves its events.
//!
//! The kernel puts the machine in ACPI mode when it mints the claim and back
//! in the mode its firmware handed over when the claim goes; the holder gets
//! the register blocks the FADT and the ECDT name, as ports it may `in` and
//! `out`, and the SCI as records on its own claim handle.
//!
//! **The SCI is a level line, masked by the kernel each time it is taken.** The
//! holder clears the status bits behind it and then acknowledges the claim
//! ([`ACK`]); a line acknowledged with a status bit still set is taken again.
//!
//! **What the firmware's AML addresses outside those blocks the holder reaches
//! one access at a time, through the kernel** ([`Access`]): firmware-owned
//! memory and a port both ways, the memory its tables are in and a register
//! at an address firmware lists nowhere to read, a function's configuration space to read. The
//! kernel decides each
//! by what the address is and answers a refusal by name ([`Refused`]); nothing
//! is mapped and no port opened. The firmware's Global Lock is taken and given
//! back the same way ([`op::LOCK_TAKE`]), and goes back with the claim.

crate::user_safe! {
    /// A run of ports a register block occupies; `len` 0 is no block.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Block {
        pub port: u16,
        pub len: u16,
    }
}

impl Block {
    pub const NONE: Self = Self { port: 0, len: 0 };

    /// A status-and-enable block's enable half (ACPI 6.5 §4.8.1): the status
    /// registers are its first half, its enable registers the second.
    pub const fn enable(self) -> u16 {
        self.port + self.len / 2
    }
}

/// The FADT's power button is the fixed-hardware one, `PWRBTN_STS` and
/// `PWRBTN_EN` in the PM1 event block.
pub const FIXED_POWER_BUTTON: u16 = 1 << 0;

crate::user_safe! {
    /// The claim's description.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct AcpiInfo {
        /// Where the loader found the RSDP: the root of the tables the holder
        /// reads through [`Access`].
        pub rsdp: u64,
        /// The PM1a event block, its status half then its enable half.
        pub pm1_event: Block,
        /// The GPE0 block, its status half then its enable half.
        pub gpe0: Block,
        /// The embedded controller's command/status and data ports, as the ECDT
        /// names them; `len` 0 where the machine named none.
        pub ec_command: Block,
        pub ec_data: Block,
        /// The GPE the embedded controller raises, inside [`Self::gpe0`].
        pub ec_gpe: u16,
        pub flags: u16,
        pub reserved: u32,
    }
}

impl AcpiInfo {
    pub fn has_ec(&self) -> bool {
        self.ec_command.len != 0
    }
}

/// The word a holder writes to its claim to have the SCI unmasked.
pub const ACK: u32 = 1;

/// What [`crate::syscall::SYS_ACPI`] does on the claim.
pub mod op {
    /// One [`Access`](super::Access), in and out through the caller's struct.
    pub const ACCESS: u64 = 0;
    /// Try the firmware's Global Lock (ACPI 6.5 §5.2.10.1): answers
    /// [`TAKEN`], or [`PENDING`] where the firmware owns it, with the pending
    /// bit left set so the firmware raises `GBL_STS` when it lets go.
    pub const LOCK_TAKE: u64 = 1;
    /// Give the Global Lock back, signalling the firmware where it asked
    /// meanwhile.
    pub const LOCK_RELEASE: u64 = 2;

    pub const TAKEN: u64 = 0;
    pub const PENDING: u64 = 1;
}

/// The address space an [`Access`] names, by its ACPI number (ACPI 6.5 Table
/// 5.1).
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Space {
    SystemMemory = 0,
    SystemIo = 1,
    /// `address` is [`pci_address`]'s.
    PciConfig = 2,
}

impl Space {
    pub const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::SystemMemory),
            1 => Some(Self::SystemIo),
            2 => Some(Self::PciConfig),
            _ => None,
        }
    }
}

/// How many bytes one access moves.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Width {
    Byte = 1,
    Word = 2,
    DWord = 4,
    QWord = 8,
}

impl Width {
    pub const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Byte),
            2 => Some(Self::Word),
            4 => Some(Self::DWord),
            8 => Some(Self::QWord),
            _ => None,
        }
    }

    pub const fn bytes(self) -> u64 {
        self as u64
    }

    /// The widest value an access of this width carries.
    pub const fn max_value(self) -> u64 {
        match self {
            Self::QWord => u64::MAX,
            narrower => (1 << (8 * narrower as u64)) - 1,
        }
    }
}

/// A PCI_Config [`Access::address`]: the function, and the byte offset into
/// its 4096 bytes of configuration space.
pub const fn pci_address(segment: u16, bus: u8, device: u8, function: u8, offset: u16) -> u64 {
    (segment as u64) << 32 | (bus as u64) << 24 | (device as u64 & 0x1F) << 19 | (function as u64 & 7) << 16 | offset as u64
}

/// Why the kernel did not make an access. Each is one row of the policy, so a
/// holder's log tells them apart.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// Memory the kernel hands out as RAM: its own, and every process's.
    UsableMemory = 1,
    /// A write to memory a firmware keeps its tables in: ACPI reclaim, and
    /// runtime-services data.
    TableWrite = 2,
    /// Memory of a type the kernel passes no access to, or a write at an
    /// address firmware's map does not list; [`Access::memory_type`] says
    /// which.
    MemoryType = 3,
    /// Past the end of what the kernel maps.
    Unmapped = 4,
    /// Memory a device decodes: a window the kernel drives one through, a PCI
    /// function's memory BAR whoever drives it, or the local APIC's.
    DeviceMemory = 5,
    /// A write to the FACS, whose Global Lock is the kernel's to change.
    FacsWrite = 6,
    /// An access whose first and last byte the map types differently.
    Straddles = 7,
    /// A port the kernel declared and keeps to itself.
    KernelPort = 8,
    /// A write to a port the kernel declared and lets be read.
    ReadOnlyPort = 9,
    /// A port of a function another claim is for.
    ClaimedPort = 10,
    /// No port access is this wide, or it runs past port 0xFFFF.
    PortSpan = 11,
    /// A segment group or bus the kernel's configuration window does not hold.
    ConfigUnreachable = 12,
    /// A configuration access wider than a dword, across a dword boundary, or
    /// past the function's 4096 bytes.
    ConfigSpan = 13,
    /// A write to configuration space, by its address or through the ECAM
    /// window: the kernel makes none for the holder.
    ConfigWrite = 14,
    /// A read at an address firmware's map does not list, which the
    /// processor's range registers do not type uncacheable: no register.
    UnlistedCached = 15,
    /// A read at an address firmware's map does not list, on a machine where
    /// some CPU's range registers are on and are not the boot processor's:
    /// what types the address a register is not what every CPU reads it
    /// under.
    RangeRegistersDiffer = 16,
}

impl Refused {
    pub const fn from_raw(raw: u8) -> Option<Self> {
        Some(match raw {
            1 => Self::UsableMemory,
            2 => Self::TableWrite,
            3 => Self::MemoryType,
            4 => Self::Unmapped,
            5 => Self::DeviceMemory,
            6 => Self::FacsWrite,
            7 => Self::Straddles,
            8 => Self::KernelPort,
            9 => Self::ReadOnlyPort,
            10 => Self::ClaimedPort,
            11 => Self::PortSpan,
            12 => Self::ConfigUnreachable,
            13 => Self::ConfigSpan,
            14 => Self::ConfigWrite,
            15 => Self::UnlistedCached,
            16 => Self::RangeRegistersDiffer,
            _ => return None,
        })
    }
}

/// [`Access::memory_type`] for an address firmware's map does not list, and
/// for an access that is not to memory.
pub const UNLISTED: u8 = 0xFF;

crate::user_safe! {
    /// One access the holder asks the kernel to make, and its answer.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Access {
        pub address: u64,
        /// What to write, or what was read.
        pub value: u64,
        /// A [`Space`].
        pub space: u8,
        /// A [`Width`].
        pub width: u8,
        /// 0 to read, 1 to write.
        pub write: u8,
        /// Out: 0 where the access was made, else a [`Refused`].
        pub refused: u8,
        /// Out, for memory: the UEFI memory type firmware's map gives the first
        /// byte (`EFI_MEMORY_TYPE`), or [`UNLISTED`].
        pub memory_type: u8,
        pub reserved: [u8; 3],
    }
}

impl Access {
    pub const fn read(space: Space, address: u64, width: Width) -> Self {
        Self { address, value: 0, space: space as u8, width: width as u8, write: 0, refused: 0, memory_type: UNLISTED, reserved: [0; 3] }
    }

    pub const fn write(space: Space, address: u64, width: Width, value: u64) -> Self {
        Self { address, value, space: space as u8, width: width as u8, write: 1, refused: 0, memory_type: UNLISTED, reserved: [0; 3] }
    }
}
