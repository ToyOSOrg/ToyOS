//! A bench's internal NVMe as an internal install sees it: the bench's own
//! partitions 1 to 4, which nothing here writes, and the seven after them
//! ([`Owned`]) that an install creates once and writes every time.
//!
//! **A disk is recognised before anything is written to it.** Partitions 1–4
//! are the bench's layout — ESP first, ToyOS DATA fourth — and 5–11 are either
//! all absent, or all present under their own types and names. Anything else is
//! refused by what it is, and nothing past partition 11 is read.
//!
//! What the disk says is read through `lsblk --pairs` ([`QUERY`] and
//! [`HOLDERS`]): the kernel's numbers out of `sysfs` and the table's types,
//! GUIDs and names out of udev's record of it.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use toyos_build::disk::Node;
use toyos_build::image::LBA;
use toyos_gpt::Guid;

/// The unit `sysfs` and `lsblk`'s `START` count in, whatever the disk's own
/// logical block.
const SECTOR: u64 = 512;

const MIB: u64 = 1 << 20;

/// The bench's own partitions: they come before every [`Owned`] one.
const BENCH: u32 = 4;

/// The fourth partition's name, which the owner gave the bench's DATA.
const DATA_LABEL: &str = "ToyOS data";

/// The partitions an internal install owns, in table order from partition 5,
/// laid out as the image lays out its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owned {
    Esp,
    Slots,
    Log,
    BootA,
    RootA,
    BootB,
    RootB,
}

impl Owned {
    pub const ALL: [Self; 7] =
        [Self::Esp, Self::Slots, Self::Log, Self::BootA, Self::RootA, Self::BootB, Self::RootB];

    pub fn index(self) -> u32 {
        BENCH + 1 + Self::ALL.iter().position(|o| *o == self).expect("every partition is listed") as u32
    }

    pub fn kind(self) -> Guid {
        match self {
            Self::Esp => Guid::EFI_SYSTEM,
            Self::Slots => Guid::TOYOS_SLOTS,
            Self::Log => Guid::MICROSOFT_BASIC,
            Self::BootA | Self::BootB => Guid::TOYOS_BOOT,
            Self::RootA | Self::RootB => Guid::TOYOS_ROOT,
        }
    }

    /// The partition's name in the table, which is what says the tool made
    /// it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Esp => "ToyOS-internal-ESP",
            Self::Slots => "ToyOS-internal-slots",
            Self::Log => "ToyOS-internal-log",
            Self::BootA => "ToyOS-internal-slot-A",
            Self::RootA => "ToyOS-internal-root-A",
            Self::BootB => "ToyOS-internal-slot-B",
            Self::RootB => "ToyOS-internal-root-B",
        }
    }

    /// Its size, with room to grow past what an image carries today: a slot's
    /// volume holds three boot programs a few MiB each twice over, and a ROOT
    /// the whole of `/system`.
    pub fn mib(self) -> u64 {
        match self {
            Self::Esp => 512,
            Self::Slots => 16,
            Self::Log => 1024,
            Self::BootA | Self::BootB => 1024,
            Self::RootA | Self::RootB => 8192,
        }
    }

    /// Whether the image's own contents name this partition by its unique
    /// GUID — the ESP's file names the log, the slot table each slot's two —
    /// so the partition has to carry the GUID the image gave it. The ESP and
    /// the slot table are found by the firmware's entry and by type, and keep
    /// the GUIDs they were created with, so the boot entry outlives an install.
    pub fn named_by_image(self) -> bool {
        !matches!(self, Self::Esp | Self::Slots)
    }

    pub fn node(self, disk: &Node) -> String {
        disk.partition(self.index())
    }
}

/// What every [`Owned`] partition needs, with a MiB between each for the
/// alignment the partitioner gives every start.
pub fn needed_sectors() -> u64 {
    Owned::ALL.iter().map(|o| (o.mib() + 1) * MIB / SECTOR).sum()
}

/// The question [`Disk::parse`] reads the answer of.
pub fn query(disk: &Node) -> String {
    format!(
        "lsblk --bytes --noheadings --paths --pairs --output \
         NAME,TYPE,RM,PARTN,START,SIZE,PARTTYPE,PARTUUID,PARTLABEL,MOUNTPOINTS {}",
        disk.whole()
    )
}

/// Every block device's partition GUID, for [`collision`].
pub const HOLDERS: &str = "lsblk --noheadings --paths --pairs --output NAME,PARTUUID";

/// One line of `lsblk --pairs`: `KEY="value"` separated by spaces, each value
/// with `"`, `\`, `` ` ``, `$` and every unprintable byte written `\xHH`.
pub(crate) fn pairs(line: &str) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    let mut rest = line.trim();
    while !rest.is_empty() {
        let (key, after) = rest.split_once("=\"").ok_or_else(|| format!("{line:?} is no lsblk pair line"))?;
        let (value, after) = after.split_once('"').ok_or_else(|| format!("{line:?} leaves a value open"))?;
        out.insert(key.to_string(), unescape(value).ok_or_else(|| format!("{line:?} escapes badly"))?);
        rest = after.trim_start();
    }
    Ok(out)
}

fn unescape(value: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut rest = value.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        if b == b'\\' {
            let hex = tail.strip_prefix(b"x")?.get(..2)?;
            bytes.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            rest = &tail[3..];
        } else {
            bytes.push(b);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}

/// A GUID as `lsblk` writes one, lower-case, in [`Guid::parse`]'s spelling.
pub(crate) fn guid(text: &str) -> Option<Guid> {
    Guid::parse(&text.to_ascii_uppercase())
}

/// One partition of the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub index: u32,
    pub node: String,
    pub start: u64,
    pub sectors: u64,
    pub kind: Option<Guid>,
    pub unique: Option<Guid>,
    pub label: String,
    pub mounted: bool,
}

impl Part {
    fn end(&self) -> u64 {
        self.start + self.sectors
    }

    /// The partition as a refusal names it: its node, type and name, and not
    /// its GUID, which on a bench partition is the machine's own identifier.
    fn said(&self) -> String {
        let kind = self.kind.map_or_else(|| "no type".to_string(), |k| k.to_string());
        format!("{} (type {kind}, named {:?})", self.node, self.label)
    }
}

/// The internal disk, as [`query`] answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    pub sectors: u64,
    pub removable: bool,
    pub parts: Vec<Part>,
}

impl Disk {
    pub fn parse(text: &str, node: &Node) -> Result<Self, String> {
        let mut disk = None;
        let mut parts = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let row = pairs(line)?;
            let field = |key: &str| row.get(key).map(String::as_str).unwrap_or_default();
            let number = |key: &str| {
                field(key).parse::<u64>().map_err(|_| format!("{} reads {key}={:?}", field("NAME"), field(key)))
            };
            match field("TYPE") {
                "disk" if field("NAME") == node.whole() => {
                    disk = Some((number("SIZE")? / SECTOR, field("RM") == "1"));
                }
                "part" => {
                    let index = u32::try_from(number("PARTN")?).map_err(|_| "a partition number".to_string())?;
                    if field("NAME") != node.partition(index) {
                        return Err(format!("{} answers as partition {index} of {}", field("NAME"), node.whole()));
                    }
                    parts.push(Part {
                        index,
                        node: field("NAME").to_string(),
                        start: number("START")?,
                        sectors: number("SIZE")? / SECTOR,
                        kind: guid(field("PARTTYPE")),
                        unique: guid(field("PARTUUID")),
                        label: field("PARTLABEL").to_string(),
                        mounted: !field("MOUNTPOINTS").trim().is_empty(),
                    });
                }
                // A holder stacked on a partition, such as a mapped volume,
                // is the partition's business and not the table's.
                _ => {}
            }
        }
        let (sectors, removable) = disk.ok_or_else(|| format!("lsblk did not list {}", node.whole()))?;
        parts.sort_by_key(|p| p.index);
        Ok(Self { sectors, removable, parts })
    }

    fn part(&self, index: u32) -> Option<&Part> {
        self.parts.iter().find(|p| p.index == index)
    }
}

/// What the install area of a recognised disk holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Area {
    /// Partitions 5–11 are not there, and the free space after partition 4
    /// is where they go.
    Absent,
    /// Every [`Owned`] partition, in [`Owned::ALL`]'s order.
    Present(Vec<Part>),
}

/// Whether `disk` is a bench's internal disk this tool may install on, and
/// what its install area holds — or why it is no layout this tool knows.
pub fn recognise(disk: &Disk) -> Result<Area, String> {
    if disk.removable {
        return Err("the disk is removable, and an internal install is for the bench's own disk".to_string());
    }
    let bench: Vec<&Part> = (1..=BENCH)
        .map(|i| disk.part(i).ok_or_else(|| format!("the disk has no partition {i}, so it is no bench's layout")))
        .collect::<Result<_, _>>()?;
    if bench[0].kind != Some(Guid::EFI_SYSTEM) {
        return Err(format!("partition 1 is {}, where a bench's ESP is", bench[0].said()));
    }
    let data = bench[3];
    if data.kind != Some(Guid::TOYOS_DATA) || data.label != DATA_LABEL {
        return Err(format!(
            "partition 4 is {}, where a bench carries ToyOS DATA named {DATA_LABEL:?}",
            data.said()
        ));
    }
    if bench.windows(2).any(|w| w[1].start < w[0].end()) {
        return Err("partitions 1 to 4 do not lie in their own order".to_string());
    }
    let owned: Vec<Option<&Part>> = Owned::ALL.iter().map(|o| disk.part(o.index())).collect();
    if owned.iter().all(Option::is_none) {
        if let Some(beyond) = disk.parts.iter().find(|p| p.index > BENCH) {
            return Err(format!(
                "{} is there and the install area is not: a partition after the bench's own that \
                 this tool did not make",
                beyond.said()
            ));
        }
        let gap = |from: u64, to: u64| to.saturating_sub(from);
        let last = disk.sectors.saturating_sub(MIB / SECTOR);
        let after = gap(data.end(), last);
        let largest_before = bench.windows(2).map(|w| gap(w[0].end(), w[1].start)).chain([bench[0].start]).max().unwrap_or(0);
        // The partitioner places each new partition at the start of the
        // largest free block as it stands, so the block after DATA has to
        // stay the largest until the last partition is carved from it.
        if after < needed_sectors() || after - needed_sectors() <= largest_before {
            return Err(format!(
                "the free space after partition 4 is {} MiB, and the install area needs {} MiB of \
                 it with the rest still larger than any other free block of the disk ({} MiB)",
                after * SECTOR / MIB,
                needed_sectors() * SECTOR / MIB,
                largest_before * SECTOR / MIB
            ));
        }
        return Ok(Area::Absent);
    }
    let mut present = Vec::new();
    let mut end = data.end();
    for (o, part) in Owned::ALL.iter().zip(&owned) {
        let part = part.ok_or_else(|| {
            format!("partition {} is missing from an install area that is otherwise there", o.index())
        })?;
        if part.kind != Some(o.kind()) || part.label != o.label() {
            return Err(format!(
                "{} is no partition this tool made: it would be type {} named {:?}",
                part.said(),
                o.kind(),
                o.label()
            ));
        }
        if part.start < end || part.unique.is_none() {
            return Err(format!("{} does not lie after the partition before it, or states no GUID", part.said()));
        }
        end = part.end();
        present.push(part.clone());
    }
    Ok(Area::Present(present))
}

/// The partition holding any of `guids` that the write about to happen does
/// not replace, out of [`HOLDERS`]' answer: a GUID on two disks is one the
/// machine's disk service serves on neither.
pub fn collision(holders: &str, guids: &[Guid], replaced: impl Fn(&str) -> bool) -> Result<(), String> {
    for line in holders.lines().filter(|l| !l.trim().is_empty()) {
        let row = pairs(line)?;
        let (Some(name), Some(held)) = (row.get("NAME"), row.get("PARTUUID").and_then(|g| guid(g))) else {
            continue;
        };
        if guids.contains(&held) && !replaced(name) {
            return Err(format!(
                "{name} already carries partition {held}, which this image gives a partition too; a \
                 machine with one GUID on two disks serves neither"
            ));
        }
    }
    Ok(())
}

/// One partition of the image: its bytes' extent in the file and its GUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub at: u64,
    pub len: u64,
    pub unique: Guid,
}

/// An image admitted for an internal install: each [`Owned`] partition's piece
/// of it, slot B's absent where the image was built with one slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub path: PathBuf,
    pub pieces: [Option<Piece>; 7],
}

impl Install {
    pub fn piece(&self, owned: Owned) -> Option<Piece> {
        self.pieces[(owned.index() - BENCH - 1) as usize]
    }

    /// The GUID each partition the image names carries once installed, in
    /// [`Owned::ALL`]'s order: the image's own, or for slot B's pair where the
    /// image has none, `fresh`, so no GUID an earlier image gave them stays.
    pub fn guids(&self, mut fresh: impl FnMut() -> Guid) -> Vec<(Owned, Guid)> {
        Owned::ALL
            .iter()
            .filter(|o| o.named_by_image())
            .map(|o| (*o, self.piece(*o).map_or_else(&mut fresh, |p| p.unique)))
            .collect()
    }
}

/// The image's partitions, each matched to the [`Owned`] partition it goes to:
/// one ESP, one slot table, one log, and the two partitions of every slot the
/// image's slot table names. Anything else in the image, or a piece larger
/// than its partition, is refused.
pub fn admit(path: &Path) -> Result<Install, String> {
    let (mut file, _) = toyos_build::disk::whole_disk(path).map_err(|e| e.to_string())?;
    let table = toyos_build::image::slot_table_of(&mut file)?;
    let mut listed = [None; 16];
    let scan = toyos_gpt::list(&mut toyos_build::image::FileSectors(&mut file), &mut listed)
        .map_err(|e| format!("the image carries no readable partition table: {e:?}"))?;
    if scan.matched as usize > listed.len() {
        return Err(format!("the image carries {} partitions", scan.matched));
    }
    let slot = |i: usize| table.slots[i].as_ref().map(|s| (Guid(s.boot), Guid(s.root)));
    let (a, b) = (slot(0).ok_or("the image's slot table names no slot A")?, slot(1));
    let mut pieces = [None; 7];
    for entry in listed.iter().flatten() {
        let part = entry.map_err(|unplaced| format!("an entry of the image is no partition: {unplaced:?}"))?;
        let (kind, unique) = (part.type_guid(), part.unique_guid());
        let owned = match kind {
            k if k == Guid::EFI_SYSTEM => Owned::Esp,
            k if k == Guid::TOYOS_SLOTS => Owned::Slots,
            k if k == Guid::MICROSOFT_BASIC => Owned::Log,
            _ if unique == a.0 => Owned::BootA,
            _ if unique == a.1 => Owned::RootA,
            _ if Some(unique) == b.map(|b| b.0) => Owned::BootB,
            _ if Some(unique) == b.map(|b| b.1) => Owned::RootB,
            _ => return Err(format!("partition {unique} of type {kind} is none the slot table names")),
        };
        if owned.kind() != kind {
            return Err(format!("partition {unique} is of type {kind}, where {owned:?} is {}", owned.kind()));
        }
        let at = part.first_lba() * u64::from(LBA);
        let len = part.lba_count().get() * u64::from(LBA);
        if len > owned.mib() * MIB {
            return Err(format!("the image's {owned:?} is {len} bytes, past its {} MiB partition", owned.mib()));
        }
        let slot = &mut pieces[(owned.index() - BENCH - 1) as usize];
        if slot.replace(Piece { at, len, unique }).is_some() {
            return Err(format!("the image carries two partitions for {owned:?}"));
        }
    }
    for owned in [Owned::Esp, Owned::Slots, Owned::Log, Owned::BootA, Owned::RootA] {
        if pieces[(owned.index() - BENCH - 1) as usize].is_none() {
            return Err(format!("the image carries no partition for {owned:?}"));
        }
    }
    if b.is_some() && pieces[5..].iter().any(Option::is_none) {
        return Err("the image's slot table names a slot B the image carries no partitions for".to_string());
    }
    Ok(Install { path: path.to_path_buf(), pieces })
}

/// `len` bytes of `path` from `at`, to be streamed into a partition.
pub fn read_piece(path: &Path, piece: Piece) -> std::io::Result<impl Read> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(piece.at))?;
    Ok(file.take(piece.len))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISK: Node = Node::Nvme { controller: b'0', namespace: b'1' };

    /// 238.5 GiB, the T14 bench's disk.
    const SECTORS: u64 = 500_118_192;

    /// Partition `n`'s line, as `lsblk --pairs` writes it.
    fn line(n: u32, start: u64, sectors: u64, parttype: &str, label: &str, mounts: &str) -> String {
        format!(
            "NAME=\"{}\" TYPE=\"part\" RM=\"0\" PARTN=\"{n}\" START=\"{start}\" SIZE=\"{}\" \
             PARTTYPE=\"{parttype}\" PARTUUID=\"{}\" PARTLABEL=\"{label}\" MOUNTPOINTS=\"{mounts}\"\n",
            DISK.partition(n),
            sectors * SECTOR,
            uuid(n)
        )
    }

    fn uuid(n: u32) -> String {
        format!("{n:08x}-0000-4000-8000-000000000000")
    }

    /// The bench's own four, as its installer and the owner left them: the
    /// ESP, `/boot`, the root and DATA, then free space.
    fn bench() -> String {
        let gib = 2 * 1024 * 1024;
        let linux = "0fc63daf-8483-4772-8e79-3d69d8477de4";
        let mut out = format!("NAME=\"/dev/nvme0n1\" TYPE=\"disk\" RM=\"0\" PARTN=\"\" START=\"\" SIZE=\"{}\" PARTTYPE=\"\" PARTUUID=\"\" PARTLABEL=\"\" MOUNTPOINTS=\"\"\n", SECTORS * SECTOR);
        out += &line(1, 2048, gib, "c12a7328-f81f-11d2-ba4b-00a0c93ec93b", "", "/boot/efi");
        out += &line(2, 2048 + gib, 2 * gib, linux, "", "/boot");
        out += &line(3, 2048 + 3 * gib, 32 * gib, linux, "", "/");
        out += &line(4, 2048 + 35 * gib, 170 * gib, "064e3777-5076-4c71-8e07-90ad24cfe8d6", "ToyOS\\x20data", "");
        out
    }

    /// The install area as an install leaves it, after `bench`.
    fn installed() -> String {
        let mut out = bench();
        let mut at = 2048 + 205 * 2 * 1024 * 1024;
        for o in Owned::ALL {
            let sectors = o.mib() * MIB / SECTOR;
            let kind = o.kind().to_string().to_ascii_lowercase();
            out += &line(o.index(), at, sectors, &kind, o.label(), "");
            at += sectors;
        }
        out
    }

    fn recognised(text: &str) -> Result<Area, String> {
        recognise(&Disk::parse(text, &DISK)?)
    }

    #[test]
    fn an_lsblk_pair_line_is_read_with_its_escapes() {
        let row = pairs("NAME=\"/dev/nvme0n1p4\" PARTLABEL=\"ToyOS\\x20data\" MOUNTPOINTS=\"/a\\x0a/b\" EMPTY=\"\"").unwrap();
        assert_eq!(row["PARTLABEL"], "ToyOS data");
        assert_eq!(row["MOUNTPOINTS"], "/a\n/b");
        assert_eq!(row["EMPTY"], "");
        for bad in ["NAME=/dev/x", "NAME=\"open", "NAME=\"\\x2\"", "NAME=\"\\q00\""] {
            assert!(pairs(bad).is_err(), "{bad}");
        }
        let disk = Disk::parse(&bench(), &DISK).unwrap();
        assert_eq!((disk.sectors, disk.removable, disk.parts.len()), (SECTORS, false, 4));
        assert!(disk.parts[0].mounted && !disk.parts[3].mounted);
        assert_eq!(disk.parts[3].kind, Some(Guid::TOYOS_DATA));
    }

    /// **The bench's layout with nothing after DATA is an area to make, and
    /// the area this tool made is one to write.**
    #[test]
    fn the_bench_layout_is_recognised_with_its_area_absent_or_made() {
        assert_eq!(recognised(&bench()), Ok(Area::Absent));
        let Ok(Area::Present(parts)) = recognised(&installed()) else { panic!("the made area is not recognised") };
        assert_eq!(parts.iter().map(|p| p.index).collect::<Vec<_>>(), [5, 6, 7, 8, 9, 10, 11]);
        // A partition past the area is somebody else's and is left alone.
        let later = installed() + &line(12, 480_000_000, 2048, "0fc63daf-8483-4772-8e79-3d69d8477de4", "scratch", "");
        assert!(matches!(recognised(&later), Ok(Area::Present(_))));
    }

    /// **Every layout this tool did not make or does not know is refused**,
    /// by what it is: nothing after them is created or written.
    #[test]
    fn a_layout_it_does_not_know_is_refused() {
        let swap = |text: String, from: &str, to: &str| text.replacen(from, to, 1);
        let cases: Vec<(String, &str)> = vec![
            (swap(bench(), "RM=\"0\"", "RM=\"1\""), "removable"),
            (bench().lines().filter(|l| !l.contains("nvme0n1p3")).map(|l| format!("{l}\n")).collect(), "no partition 3"),
            (swap(bench(), "c12a7328-f81f-11d2-ba4b-00a0c93ec93b", "0fc63daf-8483-4772-8e79-3d69d8477de4"), "partition 1"),
            (swap(bench(), "ToyOS\\x20data", "data"), "partition 4"),
            (swap(bench(), "064e3777-5076-4c71-8e07-90ad24cfe8d6", "0fc63daf-8483-4772-8e79-3d69d8477de4"), "partition 4"),
            // Something after DATA that is not the area.
            (bench() + &line(5, 480_000_000, 2048, "0fc63daf-8483-4772-8e79-3d69d8477de4", "scratch", ""), "no partition this tool made"),
            (bench() + &line(12, 480_000_000, 2048, "0fc63daf-8483-4772-8e79-3d69d8477de4", "scratch", ""), "did not make"),
            // Half an area.
            (installed().lines().filter(|l| !l.contains("nvme0n1p9")).map(|l| format!("{l}\n")).collect(), "partition 9 is missing"),
            // The area's name on another type.
            (swap(installed(), "ToyOS-internal-root-A\"", "ToyOS-internal-root-B\""), "no partition this tool made"),
        ];
        for (text, said) in cases {
            let why = recognised(&text).expect_err(said);
            assert!(why.contains(said), "{said:?} not in {why:?}");
        }
        // Too little room after DATA for the area: the AMD's free space
        // holds it, a T14 whose DATA grew does not.
        let full = swap(bench(), &format!("SIZE=\"{}\"", 170 * 2 * 1024 * 1024 * SECTOR), &format!("SIZE=\"{}\"", 200 * 2 * 1024 * 1024 * SECTOR));
        assert!(recognised(&full).expect_err("too small").contains("free space after partition 4"));
        // Room for the area after DATA, but a gap before DATA larger than
        // what the area leaves: the last partitions would land in that gap.
        // p4 moves 15 GiB later and shrinks by 15 GiB, so the gap after it is
        // unchanged at 33.5 GiB and the gap before it is 15 GiB.
        let gib = 2 * 1024 * 1024;
        let gapped = swap(
            bench(),
            &format!("START=\"{}\" SIZE=\"{}\"", 2048 + 35 * gib, 170 * gib * SECTOR),
            &format!("START=\"{}\" SIZE=\"{}\"", 2048 + 50 * gib, 155 * gib * SECTOR),
        );
        assert_ne!(gapped, bench(), "the fixture moved nothing");
        assert!(recognised(&gapped).expect_err("the remainder is not the largest").contains("larger than any other free block"));
        let small_gap = swap(
            bench(),
            &format!("START=\"{}\" SIZE=\"{}\"", 2048 + 35 * gib, 170 * gib * SECTOR),
            &format!("START=\"{}\" SIZE=\"{}\"", 2048 + 36 * gib, 169 * gib * SECTOR),
        );
        assert_eq!(recognised(&small_gap), Ok(Area::Absent), "a 1 GiB gap before DATA is smaller than what the area leaves");
    }

    #[test]
    fn a_guid_already_on_another_disk_is_refused_and_the_replaced_ones_are_not() {
        let image = Guid::parse("69DDC8F6-FAB2-423F-9818-93BB0BA7349C").unwrap();
        let holders = "NAME=\"/dev/sda\" PARTUUID=\"\"\nNAME=\"/dev/sda3\" PARTUUID=\"69ddc8f6-fab2-423f-9818-93bb0ba7349c\"\n\
                       NAME=\"/dev/nvme0n1p7\" PARTUUID=\"69ddc8f6-fab2-423f-9818-93bb0ba7349c\"\n";
        let internal = |name: &str| name.starts_with("/dev/nvme0n1p");
        let why = collision(holders, &[image], internal).unwrap_err();
        assert!(why.contains("/dev/sda3") && why.contains(&image.to_string()), "{why}");
        assert_eq!(collision(holders, &[image], |name| name.starts_with("/dev/sda") || internal(name)), Ok(()));
        assert_eq!(collision(holders, &[Guid::TOYOS_ROOT], |_| false), Ok(()));
    }

    /// An image disk carrying `parts`, each `(type, GUID)`, and a slot table
    /// naming `slots`.
    fn image(dir: &Path, name: &str, parts: &[(Guid, Guid)], slots: [Option<(Guid, Guid)>; 2]) -> PathBuf {
        const ALIGN: u64 = 2048;
        const TOTAL: u64 = 32 << 20;
        let entries: Vec<toyos_gpt::edit::Slot> = (1..)
            .zip(parts)
            .map(|(i, (kind, unique))| toyos_gpt::edit::Slot::new(*kind, *unique, i * ALIGN, i * ALIGN + 511, "x").unwrap())
            .collect();
        let mut out = vec![0u8; TOTAL as usize];
        toyos_build::image::write_gpt(&mut std::io::Cursor::new(&mut out), TOTAL, toyos_build::image::random_guid(), &entries);
        let slot = |s: Option<(Guid, Guid)>| s.map(|(b, r)| toyos_update::slots::Slot { boot: b.0, root: r.0, version: 1 });
        let slots = toyos_update::slots::Table { sequence: 1, marked: toyos_update::slots::Which::A, slots: slots.map(slot) };
        let at = parts.iter().position(|(k, _)| *k == Guid::TOYOS_SLOTS).expect("a slot table") as u64 + 1;
        let at = (at * ALIGN * u64::from(LBA)) as usize;
        out[at..at + toyos_update::slots::BLOCK].copy_from_slice(&slots.encode());
        let path = dir.join(name);
        std::fs::write(&path, out).unwrap();
        path
    }

    #[test]
    fn an_image_is_admitted_piece_by_piece_by_its_slot_table() {
        let dir = toyos_tmpdir::TempDir::new("internal");
        let g = |_| toyos_build::image::random_guid();
        let [esp, slots, log, ba, ra, bb, rb, other]: [Guid; 8] = std::array::from_fn(g);
        let built = [
            (Guid::EFI_SYSTEM, esp), (Guid::TOYOS_SLOTS, slots), (Guid::MICROSOFT_BASIC, log),
            (Guid::TOYOS_BOOT, ba), (Guid::TOYOS_ROOT, ra), (Guid::TOYOS_BOOT, bb), (Guid::TOYOS_ROOT, rb),
        ];
        let two = admit(&image(&dir, "two.img", &built, [Some((ba, ra)), Some((bb, rb))])).unwrap();
        for (o, (_, unique)) in Owned::ALL.iter().zip(built) {
            assert_eq!(two.piece(*o).map(|p| p.unique), Some(unique), "{o:?}");
        }
        assert_eq!(two.piece(Owned::Esp).unwrap().at, 2048 * 512);
        // The GUIDs the image names go on, and only those.
        let named: Vec<(Owned, Guid)> = two.guids(|| other);
        assert_eq!(named, [(Owned::Log, log), (Owned::BootA, ba), (Owned::RootA, ra), (Owned::BootB, bb), (Owned::RootB, rb)]);

        let one = admit(&image(&dir, "one.img", &built[..5], [Some((ba, ra)), None])).unwrap();
        assert_eq!((one.piece(Owned::BootB), one.piece(Owned::RootB)), (None, None));
        assert_eq!(one.guids(|| other)[3..], [(Owned::BootB, other), (Owned::RootB, other)]);

        let refused = |name: &str, parts: &[(Guid, Guid)], slots| admit(&image(&dir, name, parts, slots)).unwrap_err();
        let stray = [&built[..5], &[(Guid::TOYOS_DATA, other)]].concat();
        assert!(refused("stray.img", &stray, [Some((ba, ra)), None]).contains("none the slot table names"));
        let two_logs = [&built[..5], &[(Guid::MICROSOFT_BASIC, other)]].concat();
        assert!(refused("logs.img", &two_logs, [Some((ba, ra)), None]).contains("two partitions for Log"));
        let why = refused("no-a.img", &built[..5], [None, Some((ba, ra))]);
        assert!(why.contains("slot"), "{why}");
        assert!(refused("half-b.img", &built[..6], [Some((ba, ra)), Some((bb, rb))]).contains("slot B"));
        let swapped = [&built[..3], &[(Guid::TOYOS_ROOT, ba), (Guid::TOYOS_BOOT, ra)]].concat();
        assert!(refused("swapped.img", &swapped, [Some((ba, ra)), None]).contains("is of type"));
    }
}
