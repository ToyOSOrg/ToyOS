//! The wall clock at boot: firmware's `GetTime`, which the loader asked before
//! `ExitBootServices` and hands over in [`KernelArgs`], since the kernel calls
//! no UEFI service; on `virt` firmware reads it from the PL031, which only the
//! DSDT names.

use core::fmt;

use toyos_abi::boot::KernelArgs;
use toyos_wallclock::Civil;

/// Why this machine did not say what time it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcFault {
    /// The loader handed no reading; its own line says why.
    NotHanded,
}

impl fmt::Display for RtcFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotHanded => write!(f, "the loader handed no reading of firmware's GetTime"),
        }
    }
}

/// The instant firmware named, and the counter when it was true.
pub fn read(args: &KernelArgs) -> Result<(Civil, u64), RtcFault> {
    match args.wall_clock_known {
        0 => Err(RtcFault::NotHanded),
        1 => Ok((Civil::from_unix_secs(args.wall_clock_secs), args.wall_clock_counter)),
        other => panic!("the loader wrote {other} as wall_clock_known, which is 0 or 1"),
    }
}
