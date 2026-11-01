//! Every partition a disk's table names, read once at the disk's bring-up,
//! with what the disk holds of the table: its size and each copy.
//!
//! Every partition lies outside every other and the table outside all of them,
//! so no session can change it: the table a driver read is the one it serves
//! for the disk's life. A partition whose range is not whole 4 KiB blocks, or
//! that its own table refuses, is offered and refused by name.

use toyos_blockring::BLOCK_BYTES;
use toyos_gpt::edit::Copies;

/// One partition of the table, in blocks of the disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    pub unique: [u8; 16],
    pub kind: [u8; 16],
    /// Where it starts and how long it is, or why it cannot be served.
    pub span: Result<(u64, u64), String>,
}

/// What a driver read of one disk at its bring-up.
pub struct Read {
    pub lba_bytes: u32,
    pub sectors: u64,
    /// LBA 0 and each copy of the table, each read alone; `None` for a disk
    /// with no room for a table or whose LBA 0 would not read.
    pub copies: Option<Box<Copies>>,
    /// Every partition the table names, or why the disk carries no table
    /// this reads.
    pub parts: Result<Vec<Partition>, String>,
}

/// What the table on `sectors` says: every partition it names, each checked
/// past `toyos_gpt::locate`'s range and overlap checks and held to whole
/// blocks, and both copies as they stand. Pure: [`say`] says it.
pub fn read(sectors: &mut dyn toyos_gpt::Sectors) -> Read {
    let (lba_bytes, count) = (sectors.lba_bytes(), sectors.lba_count());
    let copies = toyos_gpt::edit::read_copies(sectors).map(Box::new);
    Read { lba_bytes, sectors: count, copies, parts: partitions(sectors) }
}

fn partitions(sectors: &mut dyn toyos_gpt::Sectors) -> Result<Vec<Partition>, String> {
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
pub fn say(name: &str, read: &Read) -> Vec<Partition> {
    let parts = match &read.parts {
        Ok(parts) => parts.clone(),
        Err(why) => {
            println!("{name}: the disk carries no partition table this driver reads: {why}");
            return Vec::new();
        }
    };
    for part in &parts {
        let (guid, kind) = (crate::guid_text(part.unique), crate::guid_text(part.kind));
        match &part.span {
            Ok((first, blocks)) => println!("{name}: partition {guid} of type {kind} at block {first}, {blocks} blocks"),
            Err(why) => println!("{name}: partition {guid} of type {kind} is not served: {why}"),
        }
    }
    if let Some(copies) = &read.copies
        && copies.in_force().is_some()
        && !copies.agree()
    {
        println!("{name}: the table's two copies do not agree: primary {:?}, backup {:?}", state(&copies.primary), state(&copies.backup));
    }
    parts
}

/// One copy as a line says it.
pub fn state(copy: &Result<toyos_gpt::edit::Table, toyos_gpt::edit::CopyError>) -> String {
    match copy {
        Ok(_) => "valid".into(),
        Err(why) => format!("{why:?}"),
    }
}
