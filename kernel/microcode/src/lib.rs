//! CPU microcode as Intel and AMD publish it, matched to one CPU, and the
//! files the kernel loads it from, pure.
//!
//! [`intel`] reads Intel's update files as SDM Vol. 3A §12.11 defines them,
//! [`amd`] AMD's containers as AMD's own parser reads them.
//! Each validates the whole file, refuses it by name and offset where any
//! part is malformed, and answers with the update a CPU loads, or why it loads
//! none. Only the documented headers are read: an update's body is its
//! maker's signed payload, handed on whole and never interpreted.
//!
//! Before any file is read, [`Maker::of`] says whose interface a CPU's
//! revision is read through and [`may_load`] whether it loads at all.
//!
//! [`INTEL`] and [`AMD`] are the files the kernel embeds, one per CPU ToyOS
//! has a bench for, each as its maker publishes it and pinned by its digest
//! in `NOTICE`. **On an AMD CPU whose own signature check can be made to take
//! a patch AMD did not sign (CVE-2024-36347, AMD-SB-7033), that pin is the
//! only check**: the files are inside the kernel image, so no other patch
//! reaches the loader, and a microcode source outside the image brings a
//! check of its own.
//!
//! Pure: no I/O, no allocation, no `unsafe`. The caller reads the CPU.
//!
//! A file is untrusted, so nothing here may panic on one.

#![no_std]
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]

use core::fmt;

use toyos_cpuvuln::Vendor;

pub mod amd;
pub mod intel;

#[cfg(test)]
mod tests;

/// A file the kernel loads from, starting 16-byte aligned: Intel's trigger
/// takes an update's data only at such an address (SDM §12.11.6), and every
/// update behind the first starts a multiple of 1024 bytes in, its data 48
/// bytes after that.
#[repr(C, align(16))]
pub struct Aligned<Bytes: ?Sized>(pub Bytes);

/// Intel's updates for the CPUs ToyOS has a bench for, end to end:
/// `intel-ucode/06-8c-01`, the T14's i5-1135G7.
pub static INTEL: &Aligned<[u8]> = &Aligned(*include_bytes!("../intel-ucode/06-8c-01"));

/// AMD's container for the CPUs ToyOS has a bench for:
/// `amd-ucode/microcode_amd_fam17h.bin`, the AMD bench's Ryzen 7 4800U among
/// them.
pub static AMD: &Aligned<[u8]> = &Aligned(*include_bytes!("../amd-ucode/microcode_amd_fam17h.bin"));

/// CPUID.01H:EAX, which both makers' updates name their CPU by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature(pub u32);

/// A microcode revision: Intel's update revision, AMD's patch level, each the
/// dword its maker reports it in MSR 8BH, compared unsigned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision(pub u32);

impl Revision {
    /// The highest revision, which no update loads over.
    pub const DISABLED: Self = Self(u32::MAX);

    /// Intel's: MSR 8BH read after writing it 0 and executing CPUID.01H, whose
    /// upper dword the revision is (SDM §12.11.7.1).
    pub const fn from_sign_id(msr: u64) -> Self {
        Self((msr >> 32) as u32)
    }

    /// AMD's: MSR 8BH's lower dword, the patch level (55922, MSR0000_008B).
    pub const fn from_patch_level(msr: u64) -> Self {
        Self(msr as u32)
    }
}

impl fmt::LowerHex for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

/// What a valid file holds for one CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice<Update> {
    /// The newest update naming the CPU, which it loads.
    Load(Update),
    /// The newest update naming the CPU is this revision, and the CPU runs
    /// one its maker's rule does not load it over.
    Current(Revision),
    /// No update in the file names the CPU.
    NoMatch,
}

/// A maker whose files the kernel embeds: whose file a CPU chooses from, and
/// whose half of MSR 8BH its revision is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Maker {
    Intel,
    Amd,
}

impl Maker {
    /// The maker a CPU of `vendor` loads from, or why it loads nothing.
    pub fn of(vendor: Vendor) -> Result<Self, Unloaded> {
        match vendor {
            Vendor::Intel => Ok(Self::Intel),
            Vendor::Amd => Ok(Self::Amd),
            Vendor::Hygon | Vendor::Centaur | Vendor::Zhaoxin | Vendor::Unknown => {
                Err(Unloaded::Vendor(vendor))
            }
        }
    }
}

/// Why a CPU loads nothing, said before any file is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unloaded {
    /// No embedded file is from this vendor, so nothing of its CPU is read.
    Vendor(Vendor),
    /// CPUID.01H:ECX[31], which a hypervisor sets for its guest (AMD's APM
    /// Vol. 3, 24594 rev 3.36, §E.3.2): the guest's microcode is the
    /// hypervisor's.
    Hypervisor,
    /// The CPU reports [`Revision::DISABLED`].
    Disabled,
}

impl fmt::Display for Unloaded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Vendor(_) => "no embedded file is from this vendor",
            Self::Hypervisor => "a hypervisor owns this CPU's microcode",
            Self::Disabled => "revision 0xffffffff, which no update loads over",
        })
    }
}

/// Whether a CPU whose maker [`Maker::of`] names may load, from its
/// CPUID.01H:ECX and the revision it reports. A guest loads nothing whatever
/// it reports.
pub fn may_load(cpuid_1_ecx: u32, found: Revision) -> Result<(), Unloaded> {
    if cpuid_1_ecx & 1 << 31 != 0 {
        return Err(Unloaded::Hypervisor);
    }
    if found == Revision::DISABLED {
        return Err(Unloaded::Disabled);
    }
    Ok(())
}

/// `bytes` as little-endian dwords; a `LEN` that is not `4 * N` does not build.
fn dwords<const LEN: usize, const N: usize>(bytes: &[u8; LEN]) -> [u32; N] {
    const { assert!(LEN == 4 * N) };
    let mut dwords = [0; N];
    for (dword, bytes) in dwords.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *dword = u32::from_le_bytes(*bytes);
    }
    dwords
}
