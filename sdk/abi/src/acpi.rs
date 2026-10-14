//! What a claim on the machine's ACPI fixed hardware hands the process that
//! serves its events.
//!
//! The kernel puts the machine in ACPI mode when it mints the claim and back
//! in the mode its firmware handed over when the claim goes; the holder gets
//! the register blocks the FADT names, as ports it may `in` and `out`, and
//! the SCI as records on its own claim handle.
//!
//! **The SCI is a level line, masked by the kernel each time it is taken.** The
//! holder clears the status bits behind it and then acknowledges the claim
//! ([`ACK`]); a line acknowledged with a status bit still set is taken again.
//!
//! **What the firmware's AML addresses outside those blocks the holder reaches
//! one access at a time, through the kernel** ([`Access`]): firmware-owned
//! memory, a register at an address firmware lists nowhere or types
//! memory-mapped I/O, a port and a function's configuration space both ways,
//! and the memory its tables are in to read. The kernel decides each by what
//! the address is and answers a refusal by name ([`Refused`]); nothing is
//! mapped and no port opened. The firmware's Global Lock is taken and given
//! back the same way ([`op::LOCK_TAKE`]), and goes back with the claim.
//!
//! **A byte written to the FADT's `SMI_CMD` is a call into the firmware, which
//! the kernel makes on the boot processor** (ACPI 6.5 Table 5.9) and returns
//! from once the firmware's handler has. It makes none of a value the FADT
//! itself gives a meaning ([`Refused::KernelCommand`]), and no more than a
//! fixed few in a second ([`Refused::CommandRate`]).
//!
//! **The power-off is the kernel's, with the sleep type the holder supplies**
//! ([`op::S5`]): what `\_S5` evaluates to is in the firmware's AML, and
//! until a holder has supplied it [`crate::syscall::SYS_SHUTDOWN`] is refused
//! [`crate::syscall::SyscallError::NotSupported`].
//!
//! **The PCI root bridges are the holder's to declare, once** ([`op::DECLARE`],
//! [`Apertures`]): each root's memory windows from its `_CRS`, and what its
//! `_OSC` granted, are AML. Until a declaration is held the kernel hands over
//! no PCI function, and a claim made meanwhile waits for it; a declaration
//! the kernel refuses, or a claim that ends without one, leaves every PCI
//! claim refused for the boot.
//!
//! **A hardware-reduced machine** ([`HARDWARE_REDUCED`]; ACPI 6.5 §4.1) has
//! no fixed hardware: its claim names no block and raises no SCI, and its
//! holder reaches the tables, firmware's memory and configuration space
//! through [`Access`] alone.

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

/// The FADT's `HW_REDUCED_ACPI` (ACPI 6.5 Table 5.10): the claim's two
/// blocks are [`Block::NONE`], and no SCI is raised on it.
pub const HARDWARE_REDUCED: u16 = 1 << 1;

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
        pub flags: u16,
        pub reserved: [u16; 3],
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
    /// Supply the `SLP_TYPa` the machine's `\_S5` evaluates to (ACPI 6.5
    /// §7.4.2), in the call's third word: what the kernel writes to PM1a
    /// control to power the machine off. Once under a claim, and kept until
    /// the next claim's holder supplies its own.
    pub const S5: u64 = 3;
    /// Declare the PCI root bridges, their memory and their `_OSC`: an
    /// [`Apertures`](super::Apertures), in through the caller's struct. Once
    /// a boot, from the claim's holder.
    pub const DECLARE: u64 = 4;

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
    /// Memory of a type the kernel passes no access to; [`Access::memory_type`]
    /// says which.
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
    /// A register's access, at an address firmware's map does not list or in
    /// memory it types `EfiMemoryMappedIO`, which the processor's range
    /// registers do not type uncacheable, or below 1 MiB, where the fixed
    /// range registers the kernel does not read decide: no register.
    RegisterCached = 15,
    /// A register's access, as [`Self::RegisterCached`] names one, on a
    /// machine where some CPU's range registers are on and are not the boot
    /// processor's: what types the address a register is not what every CPU
    /// accesses it under.
    RangeRegistersDiffer = 16,
    /// A byte for `SMI_CMD` that the FADT gives a meaning (ACPI 6.5 Table
    /// 5.9: `ACPI_ENABLE`, `ACPI_DISABLE`, `S4BIOS_REQ`, `PSTATE_CNT`,
    /// `CST_CNT`, and `RESET_VALUE` where the reset register is `SMI_CMD`):
    /// the kernel's to write, or nobody's.
    KernelCommand = 17,
    /// A write that reaches `SMI_CMD` and is not one byte to it.
    CommandSpan = 18,
    /// A byte for `SMI_CMD` past the most the kernel writes for the holder in
    /// a second.
    CommandRate = 19,
    /// A port the kernel keeps from AML though it declared none of it:
    /// `toyos_userbound::firmware::PROTECTED`.
    ProtectedPort = 20,
    /// A configuration write to a function a kernel driver drives or a claim
    /// holds, or to an interrupt controller or an IOMMU.
    HeldFunction = 21,
    /// A configuration write to a register that places what the kernel
    /// enumerated or declared: any function's header, a host bridge's
    /// address map, or the decode of a port block the kernel declared.
    AddressMap = 22,
    /// A memory write to a register the kernel keeps for itself: the
    /// package's power limit, or what places the IOMMU or redirects
    /// interrupts.
    KernelRegister = 23,
    /// A register's access on a machine whose kernel maps no register for
    /// AML: an AArch64 kernel's direct map holds memory alone, and no range
    /// register types an address a register there.
    NoRegisters = 24,
    /// A port access on a machine with no port space.
    NoPorts = 25,
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
            15 => Self::RegisterCached,
            16 => Self::RangeRegistersDiffer,
            17 => Self::KernelCommand,
            18 => Self::CommandSpan,
            19 => Self::CommandRate,
            20 => Self::ProtectedPort,
            21 => Self::HeldFunction,
            22 => Self::AddressMap,
            23 => Self::KernelRegister,
            24 => Self::NoRegisters,
            25 => Self::NoPorts,
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

crate::user_safe! {
    /// One memory window a PCI root bridge decodes, in the addresses a CPU
    /// issues.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct RootBridgeWindow {
        pub base: u64,
        pub length: u64,
    }
}

impl RootBridgeWindow {
    /// The first address past the window, saturating: a firmware-named length
    /// is untrusted input and an extent that would wrap is one this cannot
    /// contain anything past.
    pub fn end(&self) -> u64 {
        self.base.saturating_add(self.length)
    }

    /// Whether every address of the `length` bytes at `base` is one this window
    /// decodes.
    ///
    /// The whole extent and not its first address: a BAR that starts inside a
    /// window and runs past it decodes addresses the bridge does not. An extent
    /// of no length carries no address for a window to decode and is inside
    /// none of them.
    pub fn holds(&self, base: u64, length: u64) -> bool {
        length != 0
            && base >= self.base
            && base.checked_add(length).is_some_and(|end| end <= self.end())
    }
}

/// The most root bridges one declaration carries.
pub const MAX_BRIDGES: usize = 16;
/// The most memory windows one root bridge's record carries.
pub const MAX_BRIDGE_WINDOWS: usize = 8;

/// The PCI host bridge `_OSC` Control field's bits ToyOS asks for (PCI
/// Firmware Specification 3.3, §4.5.1, Table 4-5): native PCI Express
/// hot-plug, and the PCI Express capability structure.
pub const NATIVE_HOT_PLUG: u32 = 1 << 0;
pub const CAPABILITY_STRUCTURE: u32 = 1 << 4;

/// What a root bridge's `_OSC` said.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Osc {
    /// The root has no `_OSC`.
    Absent,
    /// Its evaluation, or its answer, was refused: nothing is granted.
    Refused,
    /// It was negotiated, and granted these Control bits.
    Granted(u32),
}

/// [`HostBridge::osc`]'s words.
const OSC_ABSENT: u32 = 0;
const OSC_REFUSED: u32 = 1;
const OSC_ANSWERED: u32 = 2;

crate::user_safe! {
    /// One PCI root bridge: its segment, the buses below it, the memory
    /// windows its `_CRS` produces, and what its `_OSC` granted.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct HostBridge {
        pub segment: u16,
        pub first_bus: u8,
        pub last_bus: u8,
        pub osc: u32,
        pub granted: u32,
        pub window_count: u32,
        pub windows: [RootBridgeWindow; MAX_BRIDGE_WINDOWS],
    }
}

impl HostBridge {
    /// One root's record, or `None` past [`MAX_BRIDGE_WINDOWS`] or over a bus
    /// range that ends before it starts.
    pub fn new(segment: u16, buses: (u8, u8), windows: &[RootBridgeWindow], osc: Osc) -> Option<Self> {
        let (osc, granted) = match osc {
            Osc::Absent => (OSC_ABSENT, 0),
            Osc::Refused => (OSC_REFUSED, 0),
            Osc::Granted(granted) => (OSC_ANSWERED, granted),
        };
        let mut bridge = Self {
            segment,
            first_bus: buses.0,
            last_bus: buses.1,
            osc,
            granted,
            window_count: u32::try_from(windows.len()).ok()?,
            windows: [RootBridgeWindow::default(); MAX_BRIDGE_WINDOWS],
        };
        bridge.windows.get_mut(..windows.len())?.copy_from_slice(windows);
        (buses.0 <= buses.1).then_some(bridge)
    }

    /// Its windows, or `None` for a count past the array.
    pub fn windows(&self) -> Option<&[RootBridgeWindow]> {
        self.windows.get(..usize::try_from(self.window_count).ok()?)
    }

    /// What its `_OSC` said, or `None` for a word no [`Osc`] is, or a grant
    /// beside no answer.
    pub fn osc(&self) -> Option<Osc> {
        match (self.osc, self.granted) {
            (OSC_ABSENT, 0) => Some(Osc::Absent),
            (OSC_REFUSED, 0) => Some(Osc::Refused),
            (OSC_ANSWERED, granted) => Some(Osc::Granted(granted)),
            _ => None,
        }
    }
}

crate::user_safe! {
    /// What [`op::DECLARE`] carries: the first `count` of `bridges`.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Apertures {
        pub count: u64,
        pub bridges: [HostBridge; MAX_BRIDGES],
    }
}

impl Apertures {
    /// `bridges` as a declaration, or `None` past [`MAX_BRIDGES`].
    pub fn new(bridges: &[HostBridge]) -> Option<Self> {
        let empty = HostBridge::new(0, (0, 0), &[], Osc::Absent).expect("an empty record");
        let mut declared = Self { count: bridges.len() as u64, bridges: [empty; MAX_BRIDGES] };
        declared.bridges.get_mut(..bridges.len())?.copy_from_slice(bridges);
        Some(declared)
    }

    /// The bridges declared, or `None` for a count past the array.
    pub fn bridges(&self) -> Option<&[HostBridge]> {
        self.bridges.get(..usize::try_from(self.count).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_holds_an_extent_and_not_merely_its_first_address() {
        let window = RootBridgeWindow { base: 0xa080_0000, length: 0x1f80_0000 };
        assert_eq!(window.end(), 0xc000_0000);

        assert!(window.holds(0xbcf0_0000, 0x2_0000));
        assert!(window.holds(0xa080_0000, 0x2_0000));
        assert!(window.holds(0xa080_0000, 0x1f80_0000));
        assert!(window.holds(0xbfff_f000, 0x1000));
        assert!(!window.holds(0xa07f_f000, 0x2_0000));
        assert!(!window.holds(0x9920_0000, 0x1000));
        assert!(!window.holds(0xbfff_f000, 0x1001));
        assert!(!window.holds(0xbfff_0000, 0x1001_0000));
        assert!(!window.holds(0xc000_0000, 0));
        assert!(!window.holds(0xa080_0000, 0));

        let whole = RootBridgeWindow { base: 1, length: u64::MAX };
        assert_eq!(whole.end(), u64::MAX);
        assert!(!whole.holds(2, u64::MAX));
    }

    /// A record that fits is carried whole and read back as itself; one past
    /// its array is none, and a count or an `_OSC` word past what the ABI
    /// names reads back as none.
    #[test]
    fn a_declaration_carries_its_bridges_and_nothing_past_them() {
        let w = RootBridgeWindow { base: 0x1000_0000, length: 0x2eff_0000 };
        let root = HostBridge::new(0, (0, 0xff), &[w], Osc::Granted(NATIVE_HOT_PLUG)).expect("one window fits");
        assert_eq!(root.windows(), Some(&[w][..]));
        assert_eq!(root.osc(), Some(Osc::Granted(NATIVE_HOT_PLUG)));
        assert_eq!(HostBridge::new(0, (0, 0xff), &[w; MAX_BRIDGE_WINDOWS + 1], Osc::Absent), None);
        assert_eq!(HostBridge::new(0, (2, 1), &[w], Osc::Absent), None);
        assert_eq!(HostBridge { window_count: MAX_BRIDGE_WINDOWS as u32 + 1, ..root }.windows(), None);
        assert_eq!(HostBridge { osc: 3, ..root }.osc(), None);
        assert_eq!(HostBridge { osc: OSC_ABSENT, granted: 1, ..root }.osc(), None);

        let declared = Apertures::new(&[root]).expect("one bridge fits");
        assert_eq!(declared.bridges(), Some(&[root][..]));
        assert_eq!(Apertures::new(&[root; MAX_BRIDGES + 1]), None);
        assert_eq!(Apertures { count: MAX_BRIDGES as u64 + 1, ..declared }.bridges(), None);
    }
}
