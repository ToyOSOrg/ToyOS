//! Which slot a boot may take, which image an update may install, and every
//! reason one is refused, by the word the loader hands the kernel for it.
//!
//! **The anti-rollback floor** is the highest version a boot has proven
//! ([`crate::record::Ended::Proven`]). The loader refuses an image below it,
//! and raises it only for an image that handed the machine back on purpose —
//! never for one merely booted, because a new image that dies must leave the
//! old one bootable. The loader keeps the floor in a UEFI variable it creates
//! without runtime access, so no kernel it hands the machine to, however
//! compromised, can lower it; the disk holds nothing the floor rests on.
//!
//! **What that cannot defend**: firmware variables are the firmware's, so an
//! EFI program booted instead of this loader (anything, until Secure Boot is
//! on with the owner's key), the firmware's own reset of its variables, or a
//! hand clearing its NVRAM, lowers the floor to nothing. Only a monotonic
//! counter the platform refuses to lower — a TPM's NV counter — closes that.

use crate::slots::{Table, Which};

/// Why a slot was not booted, or an image not installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The table carries no such slot.
    Absent,
    /// Its image died on its last boot.
    Died,
    /// It carries no signed header.
    Unsigned,
    /// Its signed header is not one.
    Malformed,
    /// Its signature is not this machine's key's.
    Signature,
    /// Its version is below the floor a boot has proven.
    BelowFloor { version: u64, floor: u64 },
    /// An update older than what it would replace.
    Older { version: u64, than: u64 },
    /// A section's bytes are not the ones its header names.
    Hash(&'static str),
    /// A section could not be read.
    Unreadable(&'static str),
}

impl Refusal {
    /// The word the loader hands the kernel for this refusal: one token of the
    /// boot parameter, so no comma and no space.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Died => "died",
            Self::Unsigned => "unsigned",
            Self::Malformed => "malformed",
            Self::Signature => "signature",
            Self::BelowFloor { .. } | Self::Older { .. } => "version",
            Self::Hash(_) => "hash",
            Self::Unreadable(_) => "unreadable",
        }
    }
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Absent => write!(f, "the slot table carries no such slot"),
            Self::Died => write!(f, "its image died on its last boot"),
            Self::Unsigned => write!(f, "it carries no signed header, so it is unsigned"),
            Self::Malformed => write!(f, "its signed header is not one"),
            Self::Signature => write!(f, "its signature is not this machine's key's"),
            Self::BelowFloor { version, floor } => {
                write!(f, "its version {version} is below {floor}, the highest a boot has proven")
            }
            Self::Older { version, than } => write!(f, "its version {version} is older than {than}"),
            Self::Hash(section) => write!(f, "its {section} is not the bytes its signed header names"),
            Self::Unreadable(what) => write!(f, "its {what} could not be read"),
        }
    }
}

/// Whether a boot may take an image of `version` against the proven `floor`.
pub fn admits(floor: u64, version: u64) -> Result<(), Refusal> {
    if version < floor {
        return Err(Refusal::BelowFloor { version, floor });
    }
    Ok(())
}

/// The slots a boot tries, in order: the marked one, then the other where the
/// table carries it.
pub fn order(table: &Table) -> [Option<Which>; 2] {
    let other = table.marked.other();
    [Some(table.marked), table.slot(other).map(|_| other)]
}

/// The floor after a boot proved `proven`: it only rises.
pub fn raised(floor: u64, proven: u64) -> u64 {
    floor.max(proven)
}

/// Whether an update of `version` may replace the idle slot, on a machine
/// running `running` whose idle slot holds `idle`.
///
/// **Newer than what runs, and no older than what it overwrites.** The same
/// version as the running image is refused too: two images of one version
/// cannot be told apart by any floor. The idle slot's own version may be
/// written again, which is how a torn install is finished.
pub fn installable(version: u64, running: u64, idle: Option<u64>) -> Result<(), Refusal> {
    if version <= running {
        return Err(Refusal::Older { version, than: running });
    }
    match idle {
        Some(idle) if version < idle => Err(Refusal::Older { version, than: idle }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::Slot;

    #[test]
    fn the_floor_admits_its_own_version_and_above_and_only_rises() {
        assert_eq!(admits(10, 10), Ok(()));
        assert_eq!(admits(10, 11), Ok(()));
        assert_eq!(admits(10, 9), Err(Refusal::BelowFloor { version: 9, floor: 10 }));
        assert_eq!(raised(10, 9), 10);
        assert_eq!(raised(10, 12), 12);
    }

    #[test]
    fn an_update_must_be_newer_than_what_runs() {
        assert_eq!(installable(11, 10, None), Ok(()));
        assert_eq!(installable(10, 10, None), Err(Refusal::Older { version: 10, than: 10 }));
        assert_eq!(installable(9, 10, None), Err(Refusal::Older { version: 9, than: 10 }));
        assert_eq!(installable(11, 10, Some(12)), Err(Refusal::Older { version: 11, than: 12 }));
        assert_eq!(installable(12, 10, Some(12)), Ok(()));
    }

    #[test]
    fn a_boot_tries_the_marked_slot_first_and_the_other_only_if_there_is_one() {
        let slot = Some(Slot { boot: [1; 16], root: [2; 16], version: 1 });
        let both = Table { sequence: 1, marked: Which::B, slots: [slot, slot] };
        assert_eq!(order(&both), [Some(Which::B), Some(Which::A)]);
        let one = Table { sequence: 1, marked: Which::A, slots: [slot, None] };
        assert_eq!(order(&one), [Some(Which::A), None]);
    }

    /// Each word is one token of a comma-separated boot parameter.
    #[test]
    fn every_word_is_one_parameter_token() {
        for r in [
            Refusal::Absent,
            Refusal::Died,
            Refusal::Unsigned,
            Refusal::Malformed,
            Refusal::Signature,
            Refusal::BelowFloor { version: 1, floor: 2 },
            Refusal::Hash("kernel"),
            Refusal::Unreadable("root"),
        ] {
            assert!(!r.word().is_empty() && !r.word().contains([',', ' ', ':', '=']), "{r:?}");
        }
    }
}
