//! The anti-rollback floor as a firmware variable: whose it is, what it is
//! called, what a stored one is worth, and which others a loader deletes.
//!
//! **One floor per signing key, never one per machine.** A floor is a promise
//! about the images one key signs, so a loader holds images to the floor its
//! own key's images raised and no other: an image signed by a throwaway key
//! never reads or raises the owner's. The name carries the key's fingerprint
//! and the [`Scope`] the loader was built with:
//!
//! - [`Scope::Machine`], the owner's key: one floor for the machine, which
//!   every image the key signs is held to, whatever disk it arrives on.
//! - [`Scope::Image`], a throwaway key — every image built for QEMU, for CI
//!   and for the metal loop: the fingerprint is over the key *and the log
//!   partition's GUID*, which every image mints afresh, so each image starts
//!   at no floor and raises only its own. An image flashed after a newer one,
//!   or a bisect, boots; and a checkout's key living across its builds
//!   locks nothing out. What it gives up is exactly what a throwaway key has
//!   no use for: a running system that rewrites that GUID resets the floor.
//!
//! A loader deletes the floors it can tell are stale, so a machine's NVRAM
//! holds at most one of each scope: a [`Scope::Machine`] loader every other
//! floor, and a [`Scope::Image`] loader every other image's — never a
//! machine's, so no stick booted on the owner's machine lowers the owner's
//! floor.
//!
//! **A stored variable is the loader's only if it carries exactly the
//! attributes the loader writes** — non-volatile, boot-services-only — and
//! eight bytes. One carrying runtime access was made after a handoff, and
//! UEFI 2.10 §8.2 refuses a runtime `SetVariable` of a name that exists
//! without runtime access, so no floor this loader wrote stands behind it: it
//! is deleted and the floor is none — or refused, where the firmware will not
//! delete it, since every raise would fail against it. Anything else under
//! the name, and a
//! variable the firmware will not read, could only have been written before
//! a handoff — by this loader broken, or by whatever booted instead of it — and
//! is refused, never read as no floor.

use crate::Digest;

/// UEFI variable attributes (UEFI 2.10 §8.2, `EFI_VARIABLE_*`).
pub const NON_VOLATILE: u32 = 0x1;
pub const BOOTSERVICE_ACCESS: u32 = 0x2;
pub const RUNTIME_ACCESS: u32 = 0x4;

/// The attributes the loader writes the floor with, and the only ones it reads.
pub const ATTRIBUTES: u32 = NON_VOLATILE | BOOTSERVICE_ACCESS;

/// Whose images a loader's floor holds, fixed when the loader is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The owner's key: one floor for the machine.
    Machine,
    /// A throwaway key: one floor per image, reset by every image.
    Image,
}

impl Scope {
    /// The word the build hands the loader (`TOYOS_IMAGE_FLOOR`).
    pub const fn word(self) -> &'static str {
        match self {
            Self::Machine => "machine",
            Self::Image => "image",
        }
    }

    /// The scope `word` names; any other word is refused where the loader is
    /// compiled.
    pub const fn from_word(word: &str) -> Self {
        let w = word.as_bytes();
        if eq(w, b"machine") {
            Self::Machine
        } else if eq(w, b"image") {
            Self::Image
        } else {
            panic!("TOYOS_IMAGE_FLOOR is `machine` or `image`")
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::Machine => b'K',
            Self::Image => b'I',
        }
    }
}

const fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// What every floor variable's name begins with.
pub const PREFIX: &str = "ToyOSImageFloor";

/// The hex digits of the fingerprint a name carries.
const FINGERPRINT_HEX: usize = 16;

/// `ToyOSImageFloor-<tag><fingerprint>`.
pub const NAME_BYTES: usize = PREFIX.len() + 2 + FINGERPRINT_HEX;

/// A floor variable's name, in ASCII.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Name([u8; NAME_BYTES]);

impl Name {
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.0).expect("a floor's name is ASCII")
    }
}

/// The floor a loader of `scope` embedding `key` keeps, on the image whose log
/// partition is `log_guid`.
pub fn name(scope: Scope, key: &[u8; 32], log_guid: &[u8; 16]) -> Name {
    let fingerprint: Digest = match scope {
        Scope::Machine => crate::sha256(key),
        Scope::Image => {
            let mut both = [0u8; 48];
            both[..32].copy_from_slice(key);
            both[32..].copy_from_slice(log_guid);
            crate::sha256(&both)
        }
    };
    let mut out = [0u8; NAME_BYTES];
    out[..PREFIX.len()].copy_from_slice(PREFIX.as_bytes());
    out[PREFIX.len()] = b'-';
    out[PREFIX.len() + 1] = scope.tag();
    crate::hex(&fingerprint[..FINGERPRINT_HEX / 2], &mut out[PREFIX.len() + 2..]);
    Name(out)
}

/// Whether a loader keeping `own` deletes the variable `other` found under
/// the floor's vendor GUID.
pub fn stale(scope: Scope, own: &Name, other: &str) -> bool {
    if other == own.as_str() || !other.starts_with(PREFIX) {
        return false;
    }
    let machine = other.as_bytes().get(PREFIX.len()..PREFIX.len() + 2) == Some(&[b'-', Scope::Machine.tag()][..]);
    match scope {
        Scope::Machine => true,
        Scope::Image => !machine,
    }
}

/// `EFI_NOT_FOUND` (UEFI 2.10 Appendix D): the error bit and 14.
pub const NOT_FOUND: usize = (1 << (usize::BITS - 1)) | 14;

/// What the firmware answered for the floor's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored<'a> {
    Absent,
    Held { attributes: u32, value: &'a [u8] },
    /// The firmware would not read it, with this status code.
    Unreadable(usize),
}

impl<'a> Stored<'a> {
    /// What `GetVariable` answered: the value and its attributes, or its
    /// status. Only `EFI_NOT_FOUND` is no variable; every other failure is
    /// one the firmware would not read.
    pub fn answered(got: Result<(&'a [u8], u32), usize>) -> Self {
        match got {
            Ok((value, attributes)) => Self::Held { attributes, value },
            Err(NOT_FOUND) => Self::Absent,
            Err(status) => Self::Unreadable(status),
        }
    }
}

/// What a stored floor is worth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// The loader's floor: `0` where it has written none.
    Floor(u64),
    /// Made after a handoff: deleted, and the floor is none.
    RuntimeMade { attributes: u32, len: usize },
}

/// Why a stored floor is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The loader's attributes and not eight bytes.
    Size(usize),
    /// Attributes this loader never writes, without runtime access.
    Attributes(u32),
    Unreadable(usize),
    /// Made after a handoff, and the firmware would not delete it: no floor
    /// this loader writes can take its name.
    Undeletable { attributes: u32, status: usize },
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Size(n) => write!(f, "it holds {n} bytes where this loader writes 8"),
            Self::Attributes(a) => {
                write!(f, "it carries attributes {a:#x} where this loader writes {ATTRIBUTES:#x}")
            }
            Self::Unreadable(status) => write!(f, "the firmware would not read it (status {status:#x})"),
            Self::Undeletable { attributes, status } => write!(
                f,
                "it carries runtime access ({attributes:#x}), so it was made after a handoff, and the firmware \
                 would not delete it (status {status:#x}), so no floor this loader writes can replace it"
            ),
        }
    }
}

/// The floor once a variable made after a handoff ([`Read::RuntimeMade`]) has
/// been asked deleted, given the firmware's answer: none if it went, and
/// refused if it stayed, because every raise would fail against it.
pub fn deleted(attributes: u32, answer: Result<(), usize>) -> Result<u64, Refused> {
    match answer {
        Ok(()) => Ok(0),
        Err(status) => Err(Refused::Undeletable { attributes, status }),
    }
}

/// The floor a stored variable holds, or why it is refused.
pub fn judge(stored: Stored<'_>) -> Result<Read, Refused> {
    match stored {
        Stored::Absent => Ok(Read::Floor(0)),
        Stored::Held { attributes, value } if attributes & RUNTIME_ACCESS != 0 => {
            Ok(Read::RuntimeMade { attributes, len: value.len() })
        }
        Stored::Held { attributes, value } if attributes == ATTRIBUTES => match <[u8; 8]>::try_from(value) {
            Ok(bytes) => Ok(Read::Floor(u64::from_le_bytes(bytes))),
            Err(_) => Err(Refused::Size(value.len())),
        },
        Stored::Held { attributes, .. } => Err(Refused::Attributes(attributes)),
        Stored::Unreadable(status) => Err(Refused::Unreadable(status)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];
    const LOG: [u8; 16] = [3; 16];

    /// A key's floor is its own: another key, or for an image scope another
    /// image, is another name, and the scope is in the name.
    #[test]
    fn a_floor_is_named_for_its_key_and_scope() {
        let machine = name(Scope::Machine, &KEY, &LOG);
        assert!(machine.as_str().starts_with("ToyOSImageFloor-K"), "{}", machine.as_str());
        assert_eq!(machine.as_str().len(), NAME_BYTES);
        assert_eq!(machine, name(Scope::Machine, &KEY, &[9; 16]), "the owner's floor is the machine's, whatever disk");
        assert_ne!(machine, name(Scope::Machine, &[8; 32], &LOG));
        let image = name(Scope::Image, &KEY, &LOG);
        assert!(image.as_str().starts_with("ToyOSImageFloor-I"), "{}", image.as_str());
        assert_ne!(image, name(Scope::Image, &KEY, &[9; 16]), "every image resets its own");
        assert_eq!(Scope::from_word(Scope::Image.word()), Scope::Image);
        assert_eq!(Scope::from_word(Scope::Machine.word()), Scope::Machine);
    }

    /// **A throwaway key's loader never deletes the owner's floor**; the
    /// owner's deletes every other; nobody deletes its own or a stranger's
    /// variable.
    #[test]
    fn a_loader_deletes_the_stale_floors_of_its_scope_and_never_the_owners() {
        let image = name(Scope::Image, &KEY, &LOG);
        let machine = name(Scope::Machine, &KEY, &LOG);
        let other_image = name(Scope::Image, &KEY, &[9; 16]);
        assert!(!stale(Scope::Image, &image, image.as_str()));
        assert!(stale(Scope::Image, &image, other_image.as_str()));
        assert!(!stale(Scope::Image, &image, machine.as_str()), "the owner's floor");
        assert!(stale(Scope::Image, &image, "ToyOSImageFloor"), "the unscoped name no loader writes");
        assert!(!stale(Scope::Image, &image, "BootOrder"));
        assert!(!stale(Scope::Machine, &machine, machine.as_str()));
        assert!(stale(Scope::Machine, &machine, name(Scope::Machine, &[8; 32], &LOG).as_str()));
        assert!(stale(Scope::Machine, &machine, image.as_str()));
    }

    /// Eight bytes under the loader's attributes are the floor; runtime access
    /// is a variable made after a handoff; **anything else is refused, never
    /// read as no floor** — a wrong size, attributes the loader never writes,
    /// and a variable the firmware will not read.
    #[test]
    fn only_the_loaders_own_variable_is_a_floor_and_nothing_else_is_none() {
        assert_eq!(judge(Stored::Absent), Ok(Read::Floor(0)));
        let eight = 200u64.to_le_bytes();
        assert_eq!(judge(Stored::Held { attributes: ATTRIBUTES, value: &eight }), Ok(Read::Floor(200)));
        assert_eq!(judge(Stored::Held { attributes: ATTRIBUTES, value: &[0; 9] }), Err(Refused::Size(9)));
        assert_eq!(judge(Stored::Held { attributes: ATTRIBUTES, value: &[] }), Err(Refused::Size(0)));
        assert_eq!(judge(Stored::Held { attributes: BOOTSERVICE_ACCESS, value: &eight }), Err(Refused::Attributes(2)));
        let runtime = ATTRIBUTES | RUNTIME_ACCESS;
        assert_eq!(judge(Stored::Held { attributes: runtime, value: &[0; 9] }), Ok(Read::RuntimeMade { attributes: runtime, len: 9 }));
        assert_eq!(judge(Stored::Unreadable(7)), Err(Refused::Unreadable(7)));
    }

    /// **Only an absent variable is no floor**: every other failure the
    /// firmware answers a read with is `Unreadable`, which `judge` refuses.
    #[test]
    fn only_not_found_is_no_variable() {
        assert_eq!(Stored::answered(Err(NOT_FOUND)), Stored::Absent);
        let device_error = (1 << (usize::BITS - 1)) | 7;
        assert_eq!(Stored::answered(Err(device_error)), Stored::Unreadable(device_error));
        assert_eq!(judge(Stored::answered(Err(device_error))), Err(Refused::Unreadable(device_error)));
        let eight = 5u64.to_le_bytes();
        assert_eq!(Stored::answered(Ok((&eight, ATTRIBUTES))), Stored::Held { attributes: ATTRIBUTES, value: &eight });
    }

    /// **A variable made after a handoff that the firmware keeps is refused**,
    /// never read as no floor.
    #[test]
    fn a_runtime_variable_that_stays_is_refused() {
        let runtime = ATTRIBUTES | RUNTIME_ACCESS | 0x20;
        let security_violation = (1 << (usize::BITS - 1)) | 26;
        assert_eq!(deleted(runtime, Ok(())), Ok(0));
        assert_eq!(
            deleted(runtime, Err(security_violation)),
            Err(Refused::Undeletable { attributes: runtime, status: security_violation })
        );
    }
}
