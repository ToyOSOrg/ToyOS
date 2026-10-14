//! Where on ROOT the microcode patch for a CPU is: [`Cpu::path`], under
//! [`DIR`], named as its vendor names a patch. The build writes one file per
//! name it splits out of a vendor's container; the loader looks up the one
//! its CPU's name gives, and hands that file to the kernel whole.
//!
//! A file is one patch in its vendor's own form: an Intel update, header,
//! data and any extended signature table (SDM Vol. 3A §12.11.1); an AMD
//! Microcode Patch Block, header and data.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

#[cfg(test)]
mod tests;

/// ROOT's directory of patches, `/system/share/microcode` once mounted.
pub const DIR: &str = "share/microcode";

/// A CPU as its vendor's patches name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cpu {
    /// CPUID.01H:EAX, and the platform `IA32_PLATFORM_ID` names: Intel
    /// releases an update for a family-model-stepping and a set of platforms.
    Intel { signature: u32, platform: PlatformId },
    /// CPUID Fn0000_0001 EAX: AMD releases a patch for a family, model and
    /// stepping.
    Amd { signature: u32 },
}

impl Cpu {
    /// Where on ROOT its patch is.
    pub fn path(self) -> impl fmt::Display {
        Path(self)
    }
}

/// `intel/06-8c-01/80` for the T14's i5-1135G7: the family, model and
/// stepping, then the processor flags as Intel's release notes write them,
/// the one bit of this CPU's platform. `amd/17-60-01` for the AMD bench's
/// Ryzen 7 4800U, as AMD lists its patches.
impl fmt::Display for Cpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Intel { signature, platform } => {
                let (family, model, stepping) = intel_fms(signature);
                write!(f, "intel/{family:02x}-{model:02x}-{stepping:02x}/{:02x}", platform.flag())
            }
            Self::Amd { signature } => {
                let (family, model, stepping) = amd_fms(signature);
                write!(f, "amd/{family:02x}-{model:02x}-{stepping:02x}")
            }
        }
    }
}

struct Path(Cpu);

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{DIR}/{}", self.0)
    }
}

/// Which bit of an update's processor flags names a CPU's platform (SDM Vol.
/// 3A §12.11.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlatformId(u8);

impl PlatformId {
    /// `IA32_PLATFORM_ID` (MSR 17H): bits 52:50 (Table 12-11).
    pub const fn from_msr(msr: u64) -> Self {
        Self((msr >> 50) as u8 & 7)
    }

    /// Every platform, each the one bit of a processor flags dword.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..8).map(Self)
    }

    /// Its bit in an update's processor flags.
    pub const fn flag(self) -> u32 {
        // `self.0` < 8: the field is private and every constructor masks it.
        1 << self.0
    }
}

impl fmt::Display for PlatformId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Fields of CPUID.01H:EAX: stepping 3:0, model 7:4, family 11:8, extended
/// model 19:16, extended family 27:20.
fn fields(signature: u32) -> (u32, u32, u32, u32, u32) {
    let at = |shift: u32, mask: u32| (signature >> shift) & mask;
    (at(8, 0xf), at(4, 0xf), at(0, 0xf), at(20, 0xff), at(16, 0xf))
}

/// Intel's Display Family and Display Model (SDM Vol. 2A, CPUID, Figure
/// 3-6): the extended family counts for family 0FH alone, the extended model
/// for families 06H and 0FH.
fn intel_fms(signature: u32) -> (u32, u32, u32) {
    let (family, model, stepping, ext_family, ext_model) = fields(signature);
    let display_family = if family == 0xf { family + ext_family } else { family };
    let display_model = if family == 0x6 || family == 0xf { ext_model << 4 | model } else { model };
    (display_family, display_model, stepping)
}

/// AMD's Family and Model (APM Vol. 3, 24594 rev 3.36, CPUID
/// Fn0000_0001_EAX): both extended fields count for base family 0FH alone.
fn amd_fms(signature: u32) -> (u32, u32, u32) {
    let (family, model, stepping, ext_family, ext_model) = fields(signature);
    match family {
        0xf => (family + ext_family, ext_model << 4 | model, stepping),
        _ => (family, model, stepping),
    }
}
