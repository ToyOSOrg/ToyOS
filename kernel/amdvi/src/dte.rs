//! The device table: one 256-bit entry per requester id (§2.2.2, Figure 7,
//! Table 7), indexed by the id.

use toyos_phys::Phys;

/// Every requester id a segment has; the table holds an entry for each.
pub const DEVICE_IDS: usize = 1 << 16;
/// Bytes per entry.
pub const ENTRY_BYTES: usize = 32;

const V: u64 = 1 << 0;
const TV: u64 = 1 << 1;
const MODE_SHIFT: u32 = 9;
const IR: u64 = 1 << 61;
const IW: u64 = 1 << 62;
/// `SysMgt`, bits 105:104: bits 41:40 of the second doubleword.
const SYS_MGT_SHIFT: u32 = 40;

/// The host table depth every translating entry names: `Mode` 100b, a 48-bit
/// input.
pub const MODE: u8 = 4;

/// What an IVHD device entry's DTE-setting byte declares for its requesters
/// (§5.2.2, Table 106): the `SysMgt` its translation half carries, and the
/// pass bits its interrupt half does.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Setting(pub u8);

impl Setting {
    /// What a function a process drives is given whatever firmware declared:
    /// no `SysMgt`, and no NMI, INIT, ExtInt or LINT forwarded unremapped
    /// (Table 10), so its device reaches no CPU but by its own vectors.
    pub const CLAIMED: Self = Self(0);

    /// Bits 5:4, the `SysMgt` firmware declares the requester needs.
    pub const fn sys_mgt(self) -> u64 {
        (self.0 as u64 >> 4) & 0b11
    }

    /// The pass bits, at their places in the interrupt half's doubleword
    /// (Table 106 bits 0, 1, 2, 6, 7 to Table 7 bits 184, 185, 186, 190, 191).
    const fn passes(self) -> u64 {
        let byte = self.0 as u64;
        (byte & 0b111) << 56 | (byte >> 6 & 0b11) << 62
    }
}

/// `IV`, bit 128: the interrupt half is valid.
const IV: u64 = 1 << 0;
const INT_TAB_LEN_SHIFT: u32 = 1;
/// `IntCtl`, bits 189:188.
const INT_CTL_SHIFT: u32 = 60;
const INT_CTL_REMAPPED: u64 = 0b10;

/// Entries in a requester's remapping table: 256, `IntTabLen` 1000b, one
/// per vector, 4 KiB of 128-bit entries.
pub const INTERRUPT_ENTRIES: usize = 256;

/// One entry's interrupt half, bits 191:128, which an interrupt change
/// replaces in one 64-bit write (§2.2.2.2).
pub const fn interrupts_blocked(setting: Setting) -> u64 {
    // `IntCtl` 00b: every fixed and arbitrated message target-aborted (Table 9).
    IV | setting.passes()
}

/// The interrupt half remapping every fixed and arbitrated message through
/// the 256-entry table at `table` (Table 7: root pointer 179:134, `IntTabLen`
/// 132:129).
pub const fn interrupts_remapped(table: Phys<12>, setting: Setting) -> u64 {
    IV | 0b1000 << INT_TAB_LEN_SHIFT | table.get() | INT_CTL_REMAPPED << INT_CTL_SHIFT | setting.passes()
}

/// One entry's translation half, the 128 bits a translation change replaces
/// in one write (§2.2.2.2); the interrupt half above it is replaced on its
/// own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Dte([u64; 2]);

impl Dte {
    /// `V`, `TV`, `Mode` 000b and `IR`/`IW` clear: every read and write the
    /// requester makes is target-aborted (Table 7 bits 11:9, 61, 62). Tagged
    /// with `domain`, which Table 8 requires valid wherever `V` is set.
    pub const fn blocked(domain: u16, setting: Setting) -> Self {
        Self([V | TV, domain as u64 | setting.sys_mgt() << SYS_MGT_SHIFT])
    }

    /// Translating through the four-level table at `root`, tagged `domain`:
    /// the requester reaches what that table maps and nothing else.
    pub const fn translated(root: Phys<12>, domain: u16, setting: Setting) -> Self {
        Self([
            V | TV | (MODE as u64) << MODE_SHIFT | root.get() | IR | IW,
            domain as u64 | setting.sys_mgt() << SYS_MGT_SHIFT,
        ])
    }

    pub const fn words(self) -> [u64; 2] {
        self.0
    }

    /// The domain an entry read back out of the table is tagged with.
    pub const fn domain_of(words: [u64; 2]) -> u16 {
        words[1] as u16
    }
}
