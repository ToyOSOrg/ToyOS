//! Designating a bench's ToyOS DATA partition: the stamp the DATA file server
//! formats a volume off (`bcachefs::DESIGNATION_MAGIC`), written into the
//! partition's first 4 KiB block.
//!
//! **The partition is found by what it is, never by its node**: the one
//! partition of every disk the machine lists of type TOYOS-DATA and named
//! [`internal::DATA_LABEL`], on a fixed disk, unmounted, with nothing stacked
//! on it, and written through its GUID's own node. Its first block is read
//! before the write and after it. A partition that already carries this
//! stamp, or a ToyOS volume, is never written.

use toyos_gpt::Guid;

use crate::internal;

/// The block the stamp fills, the DATA volume's.
pub const BLOCK: usize = 4096;

/// Every block device, for [`find`].
pub const QUERY: &str = "lsblk --bytes --noheadings --paths --pairs --output \
                         NAME,TYPE,RM,PKNAME,SIZE,PARTTYPE,PARTUUID,PARTLABEL,MOUNTPOINTS";

/// The DATA partition [`find`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Data {
    pub node: String,
    pub unique: Guid,
    pub blocks: u64,
}

impl Data {
    /// The node udev names the partition by its GUID, which no renumbering of
    /// the machine's disks moves.
    pub fn by_guid(&self) -> String {
        format!("/dev/disk/by-partuuid/{}", self.unique.to_string().to_ascii_lowercase())
    }

    /// The stamp for this partition: it names the partition's own block count.
    pub fn stamp(&self) -> [u8; BLOCK] {
        toyos_build::image::designation(self.blocks)
    }
}

/// The one DATA partition [`QUERY`] answered, or why none can be written.
pub fn find(listing: &str) -> Result<Data, String> {
    let rows: Vec<_> =
        listing.lines().filter(|l| !l.trim().is_empty()).map(internal::pairs).collect::<Result<_, _>>()?;
    let field = |row: &std::collections::BTreeMap<String, String>, key: &str| row.get(key).cloned().unwrap_or_default();
    let data: Vec<_> = rows
        .iter()
        .filter(|r| field(r, "TYPE") == "part" && internal::guid(&field(r, "PARTTYPE")) == Some(Guid::TOYOS_DATA))
        .collect();
    let [part] = data[..] else {
        return Err(format!("the machine lists {} TOYOS-DATA partitions, and only one is designated", data.len()));
    };
    let node = field(part, "NAME");
    if field(part, "PARTLABEL") != internal::DATA_LABEL {
        return Err(format!("{node} is named {:?}, where a bench's DATA is {:?}", field(part, "PARTLABEL"), internal::DATA_LABEL));
    }
    let disk = rows.iter().find(|r| field(r, "NAME") == field(part, "PKNAME") && field(r, "TYPE") == "disk");
    if disk.is_none_or(|d| field(d, "RM") != "0") {
        return Err(format!("{node} is on no fixed disk the machine lists, and a bench's DATA is on its own"));
    }
    if !field(part, "MOUNTPOINTS").trim().is_empty() {
        return Err(format!("{node} is mounted"));
    }
    if let Some(above) = rows.iter().find(|r| field(r, "PKNAME") == node) {
        return Err(format!("{} is stacked on {node}", field(above, "NAME")));
    }
    let unique = internal::guid(&field(part, "PARTUUID")).ok_or_else(|| format!("{node} states no GUID"))?;
    let bytes: u64 = field(part, "SIZE").parse().map_err(|_| format!("{node} reads SIZE={:?}", field(part, "SIZE")))?;
    if bytes == 0 || !bytes.is_multiple_of(BLOCK as u64) {
        return Err(format!("{node} is {bytes} bytes, no whole number of {BLOCK}-byte blocks"));
    }
    Ok(Data { node, unique, blocks: bytes / BLOCK as u64 })
}

/// What a DATA partition's first block holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block0 {
    /// This partition's stamp, byte for byte.
    Stamped,
    /// A ToyOS volume's superblock: never written over.
    Volume,
    /// Anything else, a stamp for another size among it, which the stamp
    /// replaces.
    Other,
}

pub fn classify(data: &Data, block0: &[u8]) -> Block0 {
    if block0 == data.stamp() {
        Block0::Stamped
    } else if block0.starts_with(&bcachefs::MAGIC) {
        Block0::Volume
    } else {
        Block0::Other
    }
}

/// `od -An -v -tx1`'s bytes.
pub fn od_bytes(text: &str) -> Result<Vec<u8>, String> {
    text.split_whitespace().map(|b| u8::from_str_radix(b, 16).map_err(|_| format!("{b:?} is no byte od writes"))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIQUE: &str = "11111111-2222-3333-4444-555555555555";

    fn listing(label: &str, rm: &str, mounted: &str, extra: &str) -> String {
        format!(
            "NAME=\"/dev/nvme0n1\" TYPE=\"disk\" RM=\"{rm}\" PKNAME=\"\" SIZE=\"1024209543168\" PARTTYPE=\"\" PARTUUID=\"\" PARTLABEL=\"\" MOUNTPOINTS=\"\"\n\
             NAME=\"/dev/nvme0n1p3\" TYPE=\"part\" RM=\"0\" PKNAME=\"/dev/nvme0n1\" SIZE=\"34359738368\" PARTTYPE=\"0fc63daf-8483-4772-8e79-3d69d8477de4\" PARTUUID=\"aaaaaaaa-2222-3333-4444-555555555555\" PARTLABEL=\"\" MOUNTPOINTS=\"/\"\n\
             NAME=\"/dev/nvme0n1p4\" TYPE=\"part\" RM=\"0\" PKNAME=\"/dev/nvme0n1\" SIZE=\"644245094400\" PARTTYPE=\"064e3777-5076-4c71-8e07-90ad24cfe8d6\" PARTUUID=\"{UNIQUE}\" PARTLABEL=\"{label}\" MOUNTPOINTS=\"{mounted}\"\n{extra}"
        )
    }

    fn bench() -> String {
        listing("ToyOS\\x20data", "0", "", "")
    }

    #[test]
    fn the_benchs_data_is_found_by_type_and_name_and_written_by_its_guid() {
        let data = find(&bench()).unwrap();
        assert_eq!((data.node.as_str(), data.blocks), ("/dev/nvme0n1p4", 157_286_400));
        assert_eq!(data.by_guid(), format!("/dev/disk/by-partuuid/{UNIQUE}"));
        let stamp = data.stamp();
        assert_eq!(&stamp[..16], b"TOYOS-FORMAT-ME\0");
        assert_eq!(u64::from_le_bytes(stamp[16..24].try_into().unwrap()), 157_286_400);
        assert!(stamp[24..].iter().all(|&b| b == 0));
    }

    #[test]
    fn anything_but_one_fixed_free_data_partition_named_toyos_data_is_refused() {
        let refused = |text: String, said: &str| assert!(find(&text).unwrap_err().contains(said), "{said}");
        refused(listing("data", "0", "", ""), "is named \"data\"");
        refused(listing("ToyOS\\x20data", "1", "", ""), "no fixed disk");
        refused(listing("ToyOS\\x20data", "0", "/mnt", ""), "is mounted");
        let above = "NAME=\"/dev/mapper/x\" TYPE=\"crypt\" RM=\"0\" PKNAME=\"/dev/nvme0n1p4\" SIZE=\"1\" PARTTYPE=\"\" PARTUUID=\"\" PARTLABEL=\"\" MOUNTPOINTS=\"\"\n";
        refused(listing("ToyOS\\x20data", "0", "", above), "/dev/mapper/x is stacked on /dev/nvme0n1p4");
        let second = bench().replace("nvme0n1p4", "nvme0n1p5").replace(UNIQUE, "bbbbbbbb-2222-3333-4444-555555555555");
        refused(format!("{}{second}", bench()), "lists 2 TOYOS-DATA partitions");
        refused(bench().replace("064e3777", "0fc63daf"), "lists 0 TOYOS-DATA partitions");
        refused(bench().replace("644245094400", "644245094401"), "no whole number");
    }

    #[test]
    fn a_first_block_is_this_stamp_a_volume_or_anything_else() {
        let data = find(&bench()).unwrap();
        assert_eq!(classify(&data, &data.stamp()), Block0::Stamped);
        let mut volume = [0u8; BLOCK];
        volume[..4].copy_from_slice(&bcachefs::MAGIC);
        assert_eq!(classify(&data, &volume), Block0::Volume);
        assert_eq!(classify(&data, &[0u8; BLOCK]), Block0::Other);
        let other_size = toyos_build::image::designation(data.blocks - 1);
        assert_eq!(classify(&data, &other_size), Block0::Other, "a stamp for another size designates nothing");
        assert_eq!(classify(&data, &data.stamp()[..BLOCK - 1]), Block0::Other, "a short read is no stamp");
    }

    #[test]
    fn od_bytes_read_back_as_written() {
        assert_eq!(od_bytes(" 54 4f 59\n 00 ff\n").unwrap(), vec![0x54, 0x4f, 0x59, 0, 0xff]);
        assert!(od_bytes("zz").is_err());
    }
}
