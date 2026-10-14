//! What one CPU does with the microcode patch the loader handed it: whether
//! it loads at all, whether it is handed the patch, and where its trigger
//! reads it. The patch is one file off ROOT, one update in its vendor's own
//! form (`toyos-microcode`), validated by the build that split it out; only
//! the revision its header names is read here, and the CPU judges the rest.

#![forbid(unsafe_code)]

use core::fmt;

use crate::cpu::{Vendor, CPUID_1_ECX_HYPERVISOR};

/// A vendor ToyOS loads microcode for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loader {
    Intel,
    Amd,
}

impl Loader {
    pub const fn of(vendor: Vendor) -> Option<Self> {
        match vendor {
            Vendor::Intel => Some(Self::Intel),
            Vendor::Amd => Some(Self::Amd),
            Vendor::Other(_) => None,
        }
    }

    /// The header the CPU reads before the patch's data: an Intel update's
    /// (SDM Vol. 3A Table 12-7), whose data the trigger is written the
    /// address of (§12.11.6); an AMD MPB's (57896 §2.1.3.2), whose own first
    /// byte MSRC001_0020 is written (§2.1.13.3).
    const fn header(self) -> usize {
        match self {
            Self::Intel => 48,
            Self::Amd => 32,
        }
    }

    /// Where in a patch the trigger is written the address of.
    const fn trigger(self) -> usize {
        match self {
            Self::Intel => self.header(),
            Self::Amd => 0,
        }
    }
}

/// An Intel update revision or an AMD patch level, the dword MSR 8BH reports
/// it in, compared unsigned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision(pub u32);

impl Revision {
    /// The highest Intel revision, which no update loads over.
    pub const DISABLED: Self = Self(u32::MAX);

    /// Intel's MSR 8BH read after writing it 0 and executing CPUID.01H, whose
    /// upper dword the revision is (SDM Vol. 3A §12.11.7.1).
    pub const fn from_sign_id(msr: u64) -> Self {
        Self((msr >> 32) as u32)
    }

    /// AMD's MSR0000_008B, whose bits 31:0 are the patch level and 63:32
    /// reserved (55922 §2.1.14.1).
    pub const fn from_patch_level(msr: u64) -> Self {
        Self(msr as u32)
    }
}

impl fmt::LowerHex for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

/// Why a CPU loads nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unloaded {
    /// CPUID.01H:ECX[31], which a hypervisor sets for its guest (APM Vol. 3,
    /// 24594 rev 3.36, §E.3.2): the guest's microcode is the hypervisor's.
    Hypervisor,
    /// An Intel CPU reports [`Revision::DISABLED`].
    Disabled,
    /// The loader handed no patch: ROOT holds none for this CPU.
    NoPatch,
    /// The patch the loader handed ends inside its header.
    Short { len: usize, header: usize },
}

impl fmt::Display for Unloaded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hypervisor => f.write_str("a hypervisor owns this CPU's microcode"),
            Self::Disabled => f.write_str("revision 0xffffffff, which no update loads over"),
            Self::NoPatch => f.write_str("the loader handed no patch"),
            Self::Short { len, header } => {
                write!(f, "the patch the loader handed is {len} bytes, no more than its {header}-byte header")
            }
        }
    }
}

/// Whether a CPU may load, from its CPUID.01H:ECX and the revision it
/// reports, before any patch is read. A guest loads nothing whatever it
/// reports.
pub fn may_load(loader: Loader, cpuid_1_ecx: u32, found: Revision) -> Result<(), Unloaded> {
    if cpuid_1_ecx & CPUID_1_ECX_HYPERVISOR != 0 {
        return Err(Unloaded::Hypervisor);
    }
    if loader == Loader::Intel && found == Revision::DISABLED {
        return Err(Unloaded::Disabled);
    }
    Ok(())
}

/// The patch the loader handed, read for one vendor's CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patch<'a> {
    revision: Revision,
    /// From where the trigger is written the address of to the patch's end.
    trigger: &'a [u8],
}

impl<'a> Patch<'a> {
    /// `bytes` as `loader`'s CPU reads it: a header, with the revision in
    /// its second dword for both vendors, and something after it.
    pub fn read(loader: Loader, bytes: &'a [u8]) -> Result<Self, Unloaded> {
        let header = loader.header();
        let short = Unloaded::Short { len: bytes.len(), header };
        if bytes.len() <= header {
            return Err(short);
        }
        let revision = bytes.get(4..8).and_then(|word| word.try_into().ok()).ok_or(short)?;
        let trigger = bytes.get(loader.trigger()..).ok_or(short)?;
        Ok(Self { revision: Revision(u32::from_le_bytes(revision)), trigger })
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// The bytes whose first one's address the trigger is written.
    pub fn trigger(&self) -> &'a [u8] {
        self.trigger
    }

    /// Whether a CPU that runs `found` is handed this patch. Intel's only
    /// when it is newer: the CPU "may or may not reload" an equal one (SDM
    /// Vol. 3A Example 12-10). AMD's at the level the CPU runs too: the PPRs
    /// ask a patch "loaded on both threads on each core" unless its Load
    /// Control says otherwise (57896 §2.1.3.2), and the level is per core
    /// (55922 §2.1.14.1), so a core's second thread reads the level its first
    /// loaded before its own write.
    pub fn handed(&self, loader: Loader, found: Revision) -> bool {
        match loader {
            Loader::Intel => self.revision > found,
            Loader::Amd => self.revision >= found,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A patch of `len` bytes whose header names `revision`.
    fn patch(len: usize, revision: u32) -> std::vec::Vec<u8> {
        let mut bytes = std::vec![0xa5; len];
        bytes[4..8].copy_from_slice(&revision.to_le_bytes());
        bytes
    }

    #[test]
    fn intels_trigger_reads_the_data_after_its_header_and_amds_the_whole_block() {
        let bytes = patch(112_640, 0xbe);
        let intel = Patch::read(Loader::Intel, &bytes).unwrap();
        assert_eq!(intel.revision(), Revision(0xbe));
        assert_eq!(intel.trigger().as_ptr(), bytes[48..].as_ptr());
        assert_eq!(intel.trigger().len(), bytes.len() - 48);
        let bytes = patch(3200, 0x0860_010d);
        let amd = Patch::read(Loader::Amd, &bytes).unwrap();
        assert_eq!(amd.revision(), Revision(0x0860_010d));
        assert_eq!(amd.trigger(), &bytes[..]);
    }

    #[test]
    fn a_patch_that_ends_inside_its_header_is_refused_by_its_length() {
        for (loader, header) in [(Loader::Intel, 48), (Loader::Amd, 32)] {
            for len in [0, 4, 7, header] {
                let bytes = std::vec![0; len];
                assert_eq!(Patch::read(loader, &bytes), Err(Unloaded::Short { len, header }), "{loader:?} {len}");
            }
            assert!(Patch::read(loader, &patch(header + 1, 1)).is_ok());
        }
    }

    #[test]
    fn intel_hands_a_cpu_only_a_newer_update() {
        let bytes = patch(64, 0xbe);
        let update = Patch::read(Loader::Intel, &bytes).unwrap();
        assert!(update.handed(Loader::Intel, Revision(0xbd)));
        assert!(!update.handed(Loader::Intel, Revision(0xbe)));
        assert!(!update.handed(Loader::Intel, Revision(0xbf)));
    }

    #[test]
    fn amd_hands_a_core_its_level_again_and_never_an_older_one() {
        let bytes = patch(64, 0x0860_010d);
        let block = Patch::read(Loader::Amd, &bytes).unwrap();
        assert!(block.handed(Loader::Amd, Revision(0x0860_0106)));
        assert!(block.handed(Loader::Amd, Revision(0x0860_010d)));
        assert!(!block.handed(Loader::Amd, Revision(0x0860_010e)));
        assert!(!block.handed(Loader::Amd, Revision(u32::MAX)));
    }

    #[test]
    fn a_guest_loads_nothing_whatever_it_reports() {
        for loader in [Loader::Intel, Loader::Amd] {
            for found in [Revision(0xbe), Revision::DISABLED] {
                assert_eq!(may_load(loader, CPUID_1_ECX_HYPERVISOR, found), Err(Unloaded::Hypervisor));
            }
        }
    }

    #[test]
    fn revision_0xffffffff_is_disabled_on_intel_alone() {
        assert_eq!(may_load(Loader::Intel, 0, Revision::DISABLED), Err(Unloaded::Disabled));
        assert_eq!(may_load(Loader::Intel, 0, Revision(0xffff_fffe)), Ok(()));
        assert_eq!(may_load(Loader::Amd, 0, Revision::DISABLED), Ok(()));
    }

    #[test]
    fn intel_and_amd_have_a_loader_and_no_other_vendor() {
        assert_eq!(Loader::of(Vendor::Intel), Some(Loader::Intel));
        assert_eq!(Loader::of(Vendor::Amd), Some(Loader::Amd));
        assert_eq!(Loader::of(Vendor::Other(*b"HygonGenuine")), None);
    }

    #[test]
    fn the_msrs_are_read_where_each_vendor_puts_the_revision() {
        assert_eq!(Revision::from_sign_id(0x0000_00be_0000_0000), Revision(0xbe));
        assert_eq!(Revision::from_patch_level(0xffff_ffff_0860_010d), Revision(0x0860_010d));
    }
}
