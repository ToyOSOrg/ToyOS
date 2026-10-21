//! The boot's audit of the kernel's own translation tables: no leaf is both
//! writable and executable, and none is executable outside the kernel's text.
//!
//! A leaf is judged by its own bits. A table entry above it can only narrow
//! what the leaf grants, and no narrowing is credited, so the audit can be
//! stricter than the hardware and never looser.

#![forbid(unsafe_code)]

use core::ops::Range;

/// One entry as the audit reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    /// Translates nothing.
    Absent,
    /// Names the next table down, at this physical address.
    Table(u64),
    /// Maps all of its span.
    Leaf { writable: bool, executable: bool },
}

/// A four-level format of 512 eight-byte entries per table: the root is level
/// 0 and translates bits 47:39, level 3 translates bits 20:12.
pub trait Format {
    /// `entry`, found at `level`.
    fn read(level: usize, entry: u64) -> Entry;
    /// The virtual address whose bits 47:0 are `bits`.
    fn address(bits: u64) -> u64;
}

/// Four-level paging, Intel SDM Vol. 3A §4.5, Tables 4-15 to 4-20, with
/// `IA32_EFER.NXE` set: a supervisor write needs `R/W` (bit 1), and bit 63
/// (`XD`) forbids a fetch.
pub struct X86_64;

impl Format for X86_64 {
    fn read(level: usize, entry: u64) -> Entry {
        const PRESENT: u64 = 1 << 0;
        const WRITABLE: u64 = 1 << 1;
        /// A PDPTE's 1 GiB page or a PDE's 2 MiB one; reserved in a PML4E.
        const PAGE_SIZE: u64 = 1 << 7;
        const EXECUTE_DISABLE: u64 = 1 << 63;
        /// Bits 51:12, the widest `MAXPHYADDR` §4.1.4 allows.
        const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
        if entry & PRESENT == 0 {
            Entry::Absent
        } else if level == 3 || (level > 0 && entry & PAGE_SIZE != 0) {
            Entry::Leaf { writable: entry & WRITABLE != 0, executable: entry & EXECUTE_DISABLE == 0 }
        } else {
            Entry::Table(entry & ADDRESS)
        }
    }

    /// Canonical: bits 63:48 copy bit 47 (§3.3.7.1).
    fn address(bits: u64) -> u64 {
        (((bits << 16) as i64) >> 16) as u64
    }
}

/// VMSAv8-64 stage 1, 4 KiB granule, under `TTBR1_EL1` with `T1SZ` = 16 and
/// Direct permissions, Arm ARM DDI 0487 M.d D8.3 (Tables D8-48 and D8-52):
/// bit 0 marks a valid descriptor, and bit 1 a table at levels 0 to 2 and a
/// page at level 3, where clear is reserved and treated as invalid; `AP[2]`
/// (bit 7) makes it read-only and `PXN` (bit 53) forbids an EL1 fetch.
pub struct Aarch64;

impl Format for Aarch64 {
    fn read(level: usize, entry: u64) -> Entry {
        const VALID: u64 = 1 << 0;
        const TABLE_OR_PAGE: u64 = 1 << 1;
        const READ_ONLY: u64 = 1 << 7;
        const PXN: u64 = 1 << 53;
        /// Bits 47:12 of a 4 KiB granule's next-level table address.
        const ADDRESS: u64 = 0x0000_FFFF_FFFF_F000;
        match (entry & VALID != 0, entry & TABLE_OR_PAGE != 0, level) {
            (false, _, _) | (true, false, 3) => Entry::Absent,
            (true, true, 0..=2) => Entry::Table(entry & ADDRESS),
            (true, _, _) => Entry::Leaf { writable: entry & READ_ONLY == 0, executable: entry & PXN == 0 },
        }
    }

    /// `TTBR1_EL1` translates the top 2^(64 - T1SZ) bytes (D8.2.4).
    fn address(bits: u64) -> u64 {
        0xFFFF_0000_0000_0000 | bits
    }
}

/// The leaves one rule caught: how many, and the lowest address among them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Finding {
    pub leaves: u64,
    pub first: u64,
}

/// What the walk of one root found.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Verdict {
    /// Every leaf it read.
    pub leaves: u64,
    pub writable_executable: Option<Finding>,
    /// An executable leaf not wholly inside the text.
    pub executable_outside_text: Option<Finding>,
}

impl Verdict {
    pub fn holds(&self) -> bool {
        self.writable_executable.is_none() && self.executable_outside_text.is_none()
    }
}

/// Walk every table under `root`, reading a table at a physical address
/// through `table`, and judge each leaf against `text`, the virtual addresses
/// that may be executable.
pub fn audit<'t, F: Format>(root: &'t [u64; 512], text: Range<u64>, table: &impl Fn(u64) -> &'t [u64; 512]) -> Verdict {
    let mut verdict = Verdict::default();
    descend::<F>(root, 0, 0, &text, table, &mut verdict);
    verdict
}

fn descend<'t, F: Format>(
    entries: &'t [u64; 512],
    level: usize,
    base: u64,
    text: &Range<u64>,
    table: &impl Fn(u64) -> &'t [u64; 512],
    verdict: &mut Verdict,
) {
    let shift = 39 - 9 * level as u32;
    for (index, &entry) in entries.iter().enumerate() {
        let bits = base | (index as u64) << shift;
        match F::read(level, entry) {
            Entry::Absent => {}
            Entry::Table(phys) => descend::<F>(table(phys), level + 1, bits, text, table, verdict),
            Entry::Leaf { writable, executable } => {
                verdict.leaves += 1;
                let start = F::address(bits);
                let end = start + (1 << shift);
                if executable && writable {
                    note(&mut verdict.writable_executable, start);
                }
                if executable && !(text.start <= start && end <= text.end) {
                    note(&mut verdict.executable_outside_text, start);
                }
            }
        }
    }
}

/// Entries are walked in ascending address order, so the first noted is the lowest.
fn note(finding: &mut Option<Finding>, at: u64) {
    match finding {
        Some(found) => found.leaves += 1,
        None => *finding = Some(Finding { leaves: 1, first: at }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    /// Tables at physical addresses `0x1000 * (i + 1)`, the root first.
    struct Tables(Vec<Box<[u64; 512]>>);

    impl Tables {
        fn new(count: usize) -> Self {
            Self((0..count).map(|_| Box::new([0; 512])).collect())
        }

        fn phys(index: usize) -> u64 {
            0x1000 * (index as u64 + 1)
        }

        fn audit<F: Format>(&self, text: Range<u64>) -> Verdict {
            let table = |phys: u64| -> &[u64; 512] { &self.0[(phys / 0x1000 - 1) as usize] };
            audit::<F>(&self.0[0], text, &table)
        }
    }

    mod x86_64 {
        use super::*;

        const P: u64 = 1;
        const RW: u64 = 1 << 1;
        const PS: u64 = 1 << 7;
        const XD: u64 = 1 << 63;
        const HIGH: u64 = 0xFFFF_8000_0000_0000;

        /// The kernel half's first slot down to one directory and one table of 4 KiB pages:
        /// the shape the kernel's direct map takes around its image.
        fn direct_map() -> Tables {
            let mut t = Tables::new(4);
            t.0[0][256] = Tables::phys(1) | P | RW;
            t.0[1][0] = Tables::phys(2) | P | RW;
            t.0[2][0] = Tables::phys(3) | P | RW;
            for i in 1..512 {
                t.0[2][i] = (i as u64) << 21 | P | RW | PS | XD;
            }
            t
        }

        #[test]
        fn text_read_only_and_executable_and_the_rest_neither_holds() {
            let mut t = direct_map();
            t.0[3][0] = P | RW | XD;
            t.0[3][1] = 0x1000 | P;
            t.0[3][2] = 0x2000 | P | XD;
            t.0[3][3] = 0x3000 | P | RW | XD;
            let verdict = t.audit::<X86_64>(HIGH + 0x1000..HIGH + 0x2000);
            assert_eq!(verdict, Verdict { leaves: 4 + 511, writable_executable: None, executable_outside_text: None });
            assert!(verdict.holds());
        }

        #[test]
        fn a_writable_leaf_without_xd_is_caught_at_its_address() {
            let mut t = direct_map();
            t.0[3][1] = 0x1000 | P;
            t.0[2][7] = 7 << 21 | P | RW | PS;
            t.0[2][9] = 9 << 21 | P | RW | PS;
            let verdict = t.audit::<X86_64>(HIGH + 0x1000..HIGH + 0x2000);
            let both = Some(Finding { leaves: 2, first: HIGH + (7 << 21) });
            assert_eq!(verdict.writable_executable, both);
            assert_eq!(verdict.executable_outside_text, both);
            assert!(!verdict.holds());
        }

        #[test]
        fn a_read_only_leaf_without_xd_outside_the_text_is_caught() {
            let mut t = direct_map();
            t.0[3][1] = 0x1000 | P;
            t.0[3][2] = 0x2000 | P;
            let verdict = t.audit::<X86_64>(HIGH + 0x1000..HIGH + 0x2000);
            assert_eq!(verdict.writable_executable, None);
            assert_eq!(verdict.executable_outside_text, Some(Finding { leaves: 1, first: HIGH + 0x2000 }));
        }

        /// A 2 MiB leaf that holds the text but reaches past it is executable outside it.
        #[test]
        fn a_large_leaf_straddling_the_text_is_outside_it() {
            let mut t = direct_map();
            t.0[2][0] = P | PS;
            let verdict = t.audit::<X86_64>(HIGH + 0x1000..HIGH + 0x2000);
            assert_eq!(verdict.executable_outside_text, Some(Finding { leaves: 1, first: HIGH }));
            assert_eq!(verdict.writable_executable, None);
        }

        /// `PS` makes a PDPTE a 1 GiB leaf, and in a PML4E it is not a size at all.
        #[test]
        fn page_size_ends_the_walk_below_the_root_only() {
            let mut t = Tables::new(3);
            t.0[0][256] = Tables::phys(1) | P | RW | PS;
            t.0[1][3] = 3 << 30 | P | RW | PS;
            let verdict = t.audit::<X86_64>(0..0);
            assert_eq!(verdict.leaves, 1);
            assert_eq!(verdict.writable_executable, Some(Finding { leaves: 1, first: HIGH + (3 << 30) }));
        }

        /// The user half's addresses stay canonical, and a not-present entry's other bits are nothing.
        #[test]
        fn absent_entries_are_skipped_and_addresses_are_canonical() {
            let mut t = Tables::new(3);
            t.0[0][0] = Tables::phys(1) | P | RW;
            t.0[0][255] = Tables::phys(2) | P | RW;
            t.0[1][0] = RW | PS;
            t.0[2][511] = 511 << 30 | P | RW | PS;
            let verdict = t.audit::<X86_64>(0..0);
            assert_eq!(verdict.leaves, 1);
            assert_eq!(verdict.writable_executable, Some(Finding { leaves: 1, first: 0x0000_7FFF_C000_0000 }));
        }
    }

    mod aarch64 {
        use super::*;

        const VALID: u64 = 1;
        const TABLE: u64 = 1 << 1;
        const RO: u64 = 1 << 7;
        const PXN: u64 = 1 << 53;
        const UXN: u64 = 1 << 54;
        const HIGH: u64 = 0xFFFF_8000_0000_0000;

        fn direct_map() -> Tables {
            let mut t = Tables::new(4);
            t.0[0][256] = Tables::phys(1) | VALID | TABLE;
            t.0[1][0] = Tables::phys(2) | VALID | TABLE;
            t.0[2][0] = Tables::phys(3) | VALID | TABLE;
            t.0[2][1] = 1 << 21 | VALID | PXN | UXN;
            t
        }

        #[test]
        fn text_read_only_and_executable_and_the_rest_neither_holds() {
            let mut t = direct_map();
            t.0[3][0] = VALID | TABLE | PXN | UXN;
            t.0[3][1] = 0x1000 | VALID | TABLE | RO | UXN;
            t.0[3][2] = 0x2000 | VALID | TABLE | RO | PXN | UXN;
            let verdict = t.audit::<Aarch64>(HIGH + 0x1000..HIGH + 0x2000);
            assert_eq!(verdict, Verdict { leaves: 4, writable_executable: None, executable_outside_text: None });
        }

        /// `UXN` is EL0's: a block with `PXN` clear is executable at EL1 whatever it says.
        #[test]
        fn a_writable_block_without_pxn_is_caught() {
            let mut t = direct_map();
            t.0[2][1] = 1 << 21 | VALID | UXN;
            let verdict = t.audit::<Aarch64>(HIGH + 0x1000..HIGH + 0x2000);
            assert_eq!(verdict.writable_executable, Some(Finding { leaves: 1, first: HIGH + (1 << 21) }));
        }

        /// A level 1 block is a leaf; bit 1 clear at level 3 is no page at all.
        #[test]
        fn blocks_end_the_walk_and_level_3_needs_bit_1() {
            let mut t = direct_map();
            t.0[1][1] = 1 << 30 | VALID;
            t.0[3][5] = 0x5000 | VALID;
            let verdict = t.audit::<Aarch64>(0..0);
            assert_eq!(verdict.leaves, 2);
            assert_eq!(verdict.writable_executable, Some(Finding { leaves: 1, first: HIGH + (1 << 30) }));
        }

        /// Every address under `TTBR1_EL1` has its top sixteen bits set, the root's first slot included.
        #[test]
        fn addresses_are_ttbr1s() {
            let mut t = Tables::new(1);
            t.0[0][0] = VALID;
            let verdict = t.audit::<Aarch64>(0..0);
            assert_eq!(verdict.executable_outside_text, Some(Finding { leaves: 1, first: 0xFFFF_0000_0000_0000 }));
        }
    }
}
