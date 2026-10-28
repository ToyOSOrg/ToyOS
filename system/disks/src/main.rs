//! `/system/bin/disks`: the machine's disks, through the block service.
//!
//! - `disks` or `disks list`: every disk the block service serves, its size,
//!   each copy of its table, and every partition of the copy in force.
//! - `disks designate data`: judge the machine's one DATA partition for
//!   designation and write nothing; `--commit` writes the stamp, which the
//!   DATA file server formats at its next start.
//!
//! **It holds no authority of its own.** Each command needs the
//! authorization its row is minted under its name
//! (`toyos_manifest::authorize`): `tables` to list, `designate:data` to
//! designate. A run without one is refused, whoever runs it and from where;
//! a login grants none. Exit status 0 where it was done, 1 where it was
//! refused or failed, 2 for a command line it does not take.

use toyos_blockring::wire::{Designated, DiskHead, SlotLine};
use toyos_gpt::Guid;
use toyos_manifest::authorize::Authorization;

const USAGE: &str = "usage: disks [list | designate data [--commit]]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = match args[..] {
        [] | ["list"] => list(),
        ["designate", "data"] => designate(false),
        ["designate", "data", "--commit"] => designate(true),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

/// The refusal of a run whose row was not minted `authorization`.
fn unauthorized(authorization: Authorization) -> i32 {
    refuse(&format!(
        "disks: this run holds no `{}` authorization, which only the signed image grants a row; nothing was done",
        authorization.word()
    ))
}

fn list() -> i32 {
    let Some(names) = toyos::endow::namespace() else { return unauthorized(Authorization::Tables) };
    let service = Authorization::Tables.name();
    let listed = match diskserver::list(names, service) {
        Ok(listed) => listed,
        Err(diskserver::Error::NotHeld) => return unauthorized(Authorization::Tables),
        Err(why) => return refuse(&format!("disks: the block service would not list its partitions: {why:?}")),
    };
    let mut disks: Vec<u32> = listed.iter().map(|l| l.disk).collect();
    disks.sort_unstable();
    disks.dedup();
    if disks.is_empty() {
        println!("no disk is served");
    }
    let mut code = 0;
    for disk in disks {
        match diskserver::tables::describe(names, service, disk) {
            Ok((head, slots)) => print!("{}", describe(disk, &head, &slots)),
            Err(why) => {
                println!("disk {disk}: not described: {why:?}");
                code = 1;
            }
        }
    }
    code
}

fn designate(commit: bool) -> i32 {
    let Some(names) = toyos::endow::namespace() else { return unauthorized(Authorization::DesignateData) };
    match diskserver::tables::designate(names, Authorization::DesignateData.name(), commit) {
        Err(diskserver::Error::NotHeld) => unauthorized(Authorization::DesignateData),
        Ok(done) => {
            println!("{}", designated(&done));
            i32::from(!matches!(done, Designated::Written | Designated::WouldWrite | Designated::Already))
        }
        Err(why) => refuse(&format!("disks: the DATA partition was not designated: {why:?}")),
    }
}

fn refuse(why: &str) -> i32 {
    eprintln!("{why}");
    1
}

/// What a designation came to, as a line.
fn designated(done: &Designated) -> String {
    match done {
        Designated::Written => "the DATA partition is designated: the stamp is on the disk and read back; DATA is formatted at its next start".into(),
        Designated::WouldWrite => "the DATA partition holds nothing recognised and would be designated; nothing was written (--commit writes)".into(),
        Designated::Already => "the DATA partition is designated already; nothing was written".into(),
        Designated::Volume => "the DATA partition holds a ToyOS volume; nothing was written".into(),
        Designated::Foreign(what) => format!("the DATA partition carries {what}; nothing was written"),
    }
}

/// The name a partition type has here, or its GUID.
fn kind(guid: [u8; 16]) -> String {
    let named = [
        (Guid::EFI_SYSTEM, "EFI system"),
        (Guid::MICROSOFT_BASIC, "basic data"),
        (Guid::TOYOS_DATA, "ToyOS DATA"),
        (Guid::TOYOS_ROOT, "ToyOS ROOT"),
        (Guid::TOYOS_BOOT, "ToyOS slot"),
        (Guid::TOYOS_SLOTS, "ToyOS slot table"),
        (Guid::TOYOS_SCRATCH, "ToyOS scratch"),
    ];
    named.iter().find(|(g, _)| g.0 == guid).map_or_else(|| Guid(guid).to_string(), |(_, name)| (*name).to_string())
}

/// `bytes` in the largest binary unit that leaves at least 10 of it.
fn size(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut at = 0;
    let mut v = bytes;
    while v >= 1024 * 10 && at + 1 < units.len() {
        v /= 1024;
        at += 1;
    }
    format!("{v} {}", units[at])
}

/// One disk as `disks list` prints it.
fn describe(disk: u32, head: &DiskHead, slots: &[SlotLine]) -> String {
    let bytes = head.sectors * u64::from(head.lba_bytes);
    let copies = if head.agree {
        "both copies agree".to_string()
    } else {
        format!("primary {}, backup {}", head.primary, head.backup)
    };
    let mut out = format!(
        "disk {disk}: {} ({} sectors of {} bytes), a GPT, {copies}\n",
        size(bytes),
        head.sectors,
        head.lba_bytes
    );
    for s in slots {
        let name = String::from_utf16_lossy(s.name.split(|u| *u == 0).next().unwrap_or_default());
        let sectors = s.last.saturating_sub(s.first).saturating_add(1);
        let state = match (s.served, s.held) {
            (true, true) => "in use",
            (true, false) => "free",
            (false, _) => "not served",
        };
        out.push_str(&format!(
            "  {:>3}  {:<16} {:>10}  {}  {:?}  {state}\n",
            s.index + 1,
            kind(s.kind),
            size(sectors * u64::from(head.lba_bytes)),
            Guid(s.unique),
            name,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disk prints its size, its table's state and each partition by its
    /// one-based number, type, size, unique GUID, name and use; copies that
    /// disagree say what each is.
    #[test]
    fn a_disk_is_described_one_line_a_partition() {
        let mut name = [0u16; 36];
        for (u, c) in name.iter_mut().zip("ToyOS data".encode_utf16()) {
            *u = c;
        }
        let head = DiskHead {
            lba_bytes: 512,
            sectors: 500_118_192,
            agree: true,
            primary: "valid".into(),
            backup: "valid".into(),
        };
        let data = SlotLine {
            index: 3,
            kind: Guid::TOYOS_DATA.0,
            unique: [0x11; 16],
            first: 2048,
            last: 2048 + (170u64 << 21) - 1,
            attributes: 0,
            name,
            held: false,
            served: true,
        };
        let text = describe(1, &head, &[data]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "disk 1: 238 GiB (500118192 sectors of 512 bytes), a GPT, both copies agree");
        assert_eq!(
            lines[1].split_whitespace().collect::<Vec<_>>(),
            ["4", "ToyOS", "DATA", "170", "GiB", "11111111-1111-1111-1111-111111111111", "\"ToyOS", "data\"", "free"]
        );
        assert_eq!(lines.len(), 2);
        let torn = DiskHead { agree: false, backup: "Gpt(HeaderCrc { stored: 1, computed: 2 })".into(), ..head };
        assert!(describe(0, &torn, &[]).contains("primary valid, backup Gpt(HeaderCrc"), "{}", describe(0, &torn, &[]));
        assert_eq!(size(4096), "4096 B");
        assert_eq!(size(10 << 30), "10 GiB");
    }

    #[test]
    fn a_designation_is_said_as_what_it_came_to() {
        assert!(designated(&Designated::Written).contains("read back"));
        assert!(designated(&Designated::Foreign("an NTFS file system".into())).contains("carries an NTFS file system"));
    }
}
