//! Every partition a disk's table names, read once at the disk's bring-up.
//!
//! Every partition lies outside every other and the table outside all of them,
//! so no session can change it: the table a driver read is the one it serves
//! for the disk's life. A partition whose range is not whole 4 KiB blocks, or
//! that its own table refuses, is offered and refused by name.

use toyos_blockring::BLOCK_BYTES;

/// One partition of the table, in blocks of the disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    pub unique: [u8; 16],
    pub kind: [u8; 16],
    /// Where it starts and how long it is, or why it cannot be served.
    pub span: Result<(u64, u64), String>,
}

/// Every partition the table on `sectors` names, each checked past
/// `toyos_gpt::locate`'s range and overlap checks and held to whole blocks;
/// or why the disk carries no table this reads. Pure: [`say`] says it.
pub fn read(sectors: &mut dyn toyos_gpt::Sectors) -> Result<Vec<Partition>, String> {
    let per = BLOCK_BYTES as u64 / u64::from(sectors.lba_bytes());
    let mut found = [None; 128];
    if let Err(why) = toyos_gpt::list(sectors, &mut found) {
        return Err(format!("{why:?}"));
    }
    let mut parts = Vec::new();
    for entry in found.iter().flatten() {
        let listed = match entry {
            Ok(listed) => listed,
            Err(unplaced) => {
                let span = Err(format!("its blocks are no partition on this disk: {unplaced:?}"));
                parts.push(Partition { unique: unplaced.unique_guid.0, kind: unplaced.type_guid.0, span });
                continue;
            }
        };
        let span = match toyos_gpt::locate(sectors, listed.unique_guid()) {
            Err(why) => Err(format!("its own table refuses it: {why:?}")),
            Ok(located) => {
                let p = located.partition();
                let (first, count) = (p.first_lba(), p.lba_count().get());
                if first % per != 0 || count % per != 0 {
                    Err(format!("LBA {first}+{count} is not whole 4 KiB blocks of {}-byte sectors", sectors.lba_bytes()))
                } else {
                    Ok((first / per, count / per))
                }
            }
        };
        parts.push(Partition { unique: listed.unique_guid().0, kind: listed.type_guid().0, span });
    }
    Ok(parts)
}

/// Say what [`read`] found, as `name`, and answer the partitions: none for a
/// disk with no table.
pub fn say(name: &str, read: Result<Vec<Partition>, String>) -> Vec<Partition> {
    let parts = match read {
        Ok(parts) => parts,
        Err(why) => {
            println!("{name}: the disk carries no partition table this driver reads: {why}");
            return Vec::new();
        }
    };
    for part in &parts {
        let guid = crate::guid_text(part.unique);
        match &part.span {
            Ok((first, blocks)) => println!("{name}: partition {guid} at block {first}, {blocks} blocks"),
            Err(why) => println!("{name}: partition {guid} is not served: {why}"),
        }
    }
    parts
}
