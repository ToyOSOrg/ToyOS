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
/// (§5.2.2, Table 106). Of it, a translation entry carries `SysMgt` alone:
/// the pass bits are the interrupt half's, which nothing here sets valid.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Setting(pub u8);

impl Setting {
    /// Bits 5:4, the `SysMgt` firmware declares the requester needs.
    pub const fn sys_mgt(self) -> u64 {
        (self.0 as u64 >> 4) & 0b11
    }
}

/// One entry's translation half, the 128 bits a translation change replaces
/// in one write (§2.2.2.2); the interrupt half above it stays zero, `IV`
/// clear, so the unit forwards a requester's interrupts unremapped (Table 9).
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
