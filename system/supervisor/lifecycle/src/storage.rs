//! Which partitions a file-server role's block grant reaches.
//!
//! **LOG and BOOT are the loader's word**: the unique GUIDs it read off the
//! path firmware booted (`toyos_abi::inventory::Role`), so on a machine with
//! ToyOS on two disks each role is the volume of the disk it booted from, and
//! a GUID two disks carry is the block service's to refuse on both. **DATA is
//! its ToyOS type.** No role is reached by a place on a disk or by another
//! system's partition type, so a disk ToyOS shares with another system offers
//! that system's partitions to no role.

use toyos_abi::inventory::Role;
use toyos_blockring::wire::{Grant, Scope};

/// The block grant file-server `role` is started with, given the partition
/// the loader named for each role: `Err` for a log or boot role the loader
/// named none for.
///
/// # Panics
/// On SYSTEM, which ROOT's image serves and no grant reaches, and on a role
/// [`toyos_manifest::ROLES`] does not name, which the build refuses.
pub fn grant(role: &str, loaded: impl Fn(Role) -> Option<[u8; 16]>) -> Result<Grant, String> {
    let (which, writes) = match role {
        "data" => return Ok(Grant { scope: Scope::Kind(toyos_gpt::Guid::TOYOS_DATA.0), writes: true }),
        "log" => (Role::Log, true),
        "boot" => (Role::Boot, false),
        other => panic!("supervisor: `{other}` is no block role"),
    };
    let guid = loaded(which).ok_or_else(|| format!("the loader named no `{role}` partition"))?;
    Ok(Grant { scope: Scope::Unique(guid), writes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_gpt::Guid;

    /// `0FC63DAF-8483-4772-8E79-3D69D8477DE4`, Linux filesystem data: the
    /// type of a bench's `/boot` and its root.
    const LINUX: Guid = Guid::from_fields(0x0FC6_3DAF, 0x8483, 0x4772, [0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4]);

    struct Part {
        name: &'static str,
        unique: [u8; 16],
        kind: Guid,
    }

    const fn part(name: &'static str, n: u8, kind: Guid) -> Part {
        Part { name, unique: [n; 16], kind }
    }

    /// A bench's internal disk: the host's ESP, `/boot` and root, the owner's
    /// unformatted ToyOS DATA, and an install in the free area after it.
    const INTERNAL: [Part; 11] = [
        part("p1, the host's ESP", 1, Guid::EFI_SYSTEM),
        part("p2, the host's /boot", 2, LINUX),
        part("p3, the host's root", 3, LINUX),
        part("p4, ToyOS DATA", 4, Guid::TOYOS_DATA),
        part("p5, the installed ESP", 5, Guid::EFI_SYSTEM),
        part("p6, the installed slot table", 6, Guid::TOYOS_SLOTS),
        part("p7, the installed log", 7, Guid::MICROSOFT_BASIC),
        part("p8, the installed slot A", 8, Guid::TOYOS_BOOT),
        part("p9, the installed slot B", 9, Guid::TOYOS_BOOT),
        part("p10, the installed ROOT A", 10, Guid::TOYOS_ROOT),
        part("p11, the installed ROOT B", 11, Guid::TOYOS_ROOT),
    ];

    /// The stick the bench boots its images from, built at another time.
    const STICK: [Part; 5] = [
        part("the stick's ESP", 21, Guid::EFI_SYSTEM),
        part("the stick's slot table", 22, Guid::TOYOS_SLOTS),
        part("the stick's log", 23, Guid::MICROSOFT_BASIC),
        part("the stick's slot A", 24, Guid::TOYOS_BOOT),
        part("the stick's ROOT A", 25, Guid::TOYOS_ROOT),
    ];

    /// What each role's grant reaches across both disks, and whether it writes.
    fn reached(role: &str, boot: [u8; 16], log: [u8; 16]) -> (Vec<&'static str>, bool) {
        let loaded = |which| match which {
            Role::Boot => Some(boot),
            Role::Log => Some(log),
            _ => None,
        };
        let grant = grant(role, loaded).expect("the loader named it");
        let all = INTERNAL.iter().chain(&STICK);
        (all.filter(|p| grant.admits(p.unique, p.kind.0)).map(|p| p.name).collect(), grant.writes)
    }

    /// With ToyOS on the stick and on the internal disk, each role is the
    /// booted disk's own and DATA the one DATA partition, so the host's
    /// partitions are no role's on either boot.
    #[test]
    fn each_role_is_the_booted_disks_and_none_is_the_hosts() {
        let boots = [
            ("the stick", [21; 16], [23; 16], "the stick's ESP", "the stick's log"),
            ("the internal disk", [5; 16], [7; 16], "p5, the installed ESP", "p7, the installed log"),
        ];
        for (booted, boot, log, its_esp, its_log) in boots {
            assert_eq!(reached("boot", boot, log), (vec![its_esp], false), "booted off {booted}");
            assert_eq!(reached("log", boot, log), (vec![its_log], true), "booted off {booted}");
            assert_eq!(reached("data", boot, log), (vec!["p4, ToyOS DATA"], true), "booted off {booted}");
        }
    }

    #[test]
    fn a_role_the_loader_did_not_name_is_refused() {
        assert_eq!(grant("log", |_| None), Err("the loader named no `log` partition".to_string()));
        assert_eq!(grant("boot", |_| None), Err("the loader named no `boot` partition".to_string()));
    }
}
