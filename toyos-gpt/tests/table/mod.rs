//! Seeded GPT layouts, value mutations of them, and images with both CRC32s resealed.

#![allow(dead_code)]

use toyos_gpt::{crc32, Guid, Sectors};

/// splitmix64, so a red run is reproducible from its seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n`; `n` is never zero here.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }

    pub fn one_in(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }

    pub fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[self.below(from.len() as u64) as usize]
    }

    pub fn guid(&mut self) -> Guid {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&self.next().to_le_bytes());
        b[8..].copy_from_slice(&self.next().to_le_bytes());
        b[0] |= 1;
        Guid(b)
    }

    /// A name field: up to 36 units of any non-zero value, the rest zero.
    pub fn name(&mut self) -> [u16; 36] {
        let mut name = [0; 36];
        for unit in name.iter_mut().take(self.below(37) as usize) {
            *unit = self.range(1, 0xFFFF) as u16;
        }
        name
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawEntry {
    pub index: u32,
    pub type_guid: Guid,
    pub unique: Guid,
    pub first: u64,
    pub last: u64,
    /// The name field whole, as UTF-16 units.
    pub name: [u16; 36],
}

/// One copy of the table, every field as the image will state it.
#[derive(Clone, Debug)]
pub struct Table {
    pub revision: u32,
    pub header_bytes: u32,
    pub reserved: u32,
    pub my_lba: u64,
    pub first_usable: u64,
    pub last_usable: u64,
    pub disk_guid: Guid,
    pub entry_array_lba: u64,
    pub entry_count: u32,
    pub entry_bytes: u32,
    /// The used entries, at their own indices.
    pub entries: Vec<RawEntry>,
}

impl Table {
    /// This copy as the backup of a disk of `lba_count` blocks of `lba_bytes` states it.
    pub fn mirror(&self, lba_bytes: u32, lba_count: u64) -> Table {
        let array_lbas = (u64::from(self.entry_count) * u64::from(self.entry_bytes)).div_ceil(u64::from(lba_bytes));
        Table { my_lba: lba_count - 1, entry_array_lba: lba_count - 1 - array_lbas, ..self.clone() }
    }
}

/// A disk: its geometry, the primary copy, and the backup where it has one.
#[derive(Clone, Debug)]
pub struct Layout {
    pub lba_bytes: u32,
    pub lba_count: u64,
    pub primary: Table,
    pub backup: Option<Table>,
    /// What the device answers for its block count, and the granularity it floors by.
    pub reported_lba_count: u64,
    pub granularity: u64,
    pub mbr_type: u8,
    pub mbr_signature: [u8; 2],
}

/// What a generated layout may be.
pub struct Shape<'a> {
    pub lba_sizes: &'a [u32],
    pub types: &'a [Guid],
    pub entry_sizes: &'a [u32],
    /// Some layouts carry no backup copy.
    pub backup: bool,
    /// Some devices floor their block count, as the kernel's 4 KiB reader does.
    pub floored: bool,
}

/// A table UEFI 2.11 §5.3 accepts, with up to eight disjoint partitions at random indices.
pub fn valid(rng: &mut Rng, shape: &Shape<'_>) -> Layout {
    let lba_bytes = rng.pick(shape.lba_sizes);
    let entry_bytes = rng.pick(shape.entry_sizes).min(lba_bytes);
    let entry_count = if rng.one_in(3) { 128 } else { rng.range(1, 160) as u32 };
    let array_lbas = (u64::from(entry_count) * u64::from(entry_bytes)).div_ceil(u64::from(lba_bytes));
    let first_usable = 2 + array_lbas + rng.below(3);
    let data = if lba_bytes == 512 { rng.range(8, 400) } else { rng.range(8, 48) };
    let lba_count = first_usable + data + array_lbas + 1;
    let last_usable = lba_count - 2 - array_lbas;

    let used = rng.below(u64::from(entry_count.min(8)) + 1) as usize;
    let mut cuts: Vec<u64> = (0..2 * used).map(|_| rng.range(first_usable, last_usable)).collect();
    cuts.sort_unstable();
    let mut indices: Vec<u32> = Vec::new();
    while indices.len() < used {
        let i = rng.below(u64::from(entry_count)) as u32;
        if !indices.contains(&i) {
            indices.push(i);
        }
    }
    // Disjoint: each partition starts past the previous one's end.
    let mut entries = Vec::new();
    let mut floor = first_usable;
    for (k, index) in indices.into_iter().enumerate() {
        let first = cuts[2 * k].max(floor);
        let last = cuts[2 * k + 1].max(first);
        if last > last_usable {
            break;
        }
        floor = last + 1;
        entries.push(RawEntry { index, type_guid: rng.pick(shape.types), unique: rng.guid(), first, last, name: rng.name() });
    }

    let primary = Table {
        revision: 0x0001_0000,
        header_bytes: 92,
        reserved: 0,
        my_lba: 1,
        first_usable,
        last_usable,
        disk_guid: rng.guid(),
        entry_array_lba: 2,
        entry_count,
        entry_bytes,
        entries,
    };
    let backup = (shape.backup && !rng.one_in(3)).then(|| primary.mirror(lba_bytes, lba_count));
    let granularity = if shape.floored && lba_bytes == 512 && rng.one_in(4) { 8 } else { 1 };
    Layout {
        lba_bytes,
        lba_count,
        primary,
        backup,
        reported_lba_count: lba_count / granularity * granularity,
        granularity,
        mbr_type: 0xEE,
        mbr_signature: [0x55, 0xAA],
    }
}

/// Values at the edges of what `t` and the disk make meaningful, and past them.
fn edge(rng: &mut Rng, t: &Table, lba_count: u64) -> u64 {
    let (fu, lu) = (t.first_usable, t.last_usable);
    let near = [
        0,
        1,
        2,
        fu.wrapping_sub(1),
        fu,
        fu.wrapping_add(1),
        lu.wrapping_sub(1),
        lu,
        lu.wrapping_add(1),
        lba_count.wrapping_sub(2),
        lba_count.wrapping_sub(1),
        lba_count,
        lba_count.wrapping_add(1),
        u64::from(u32::MAX),
        u64::MAX - 1,
        u64::MAX,
    ];
    match rng.below(4) {
        0 => rng.below(lba_count.max(1)),
        1 => rng.next(),
        _ => rng.pick(&near),
    }
}

/// Bend one header field or one entry's value.
fn mutate_table(rng: &mut Rng, t: &mut Table, lba_bytes: u32, lba_count: u64) {
    match rng.below(14) {
        0 => t.first_usable = edge(rng, t, lba_count),
        1 => t.last_usable = edge(rng, t, lba_count),
        2 => t.entry_array_lba = edge(rng, t, lba_count),
        3 => {
            let random = rng.next() as u32;
            t.entry_count = rng.pick(&[0, 1, 2, 3, 4, 7, 8, 127, 128, 129, 1024, 1025, u32::MAX, random])
        }
        4 => t.entry_bytes = rng.pick(&[0, 1, 64, 127, 128, 192, 256, 512, 1024, 4096, 8192, u32::MAX]),
        5 => t.header_bytes = rng.pick(&[0, 91, 92, 93, 128, lba_bytes, lba_bytes + 1, u32::MAX]),
        6 => t.my_lba = rng.pick(&[0, 1, 2, lba_count - 1, lba_count, u64::MAX]),
        7 => t.revision = rng.pick(&[0x0001_0000, 0x0001_0001, 0, u32::MAX]),
        8 => t.reserved = rng.pick(&[0, 1, u32::MAX]),
        _ => mutate_entry(rng, t, lba_count),
    }
}

fn mutate_entry(rng: &mut Rng, t: &mut Table, lba_count: u64) {
    if t.entries.is_empty() || rng.one_in(6) {
        let index = rng.below(u64::from(t.entry_count.clamp(1, 256))) as u32;
        t.entries.retain(|e| e.index != index);
        let (first, last) = (edge(rng, t, lba_count), edge(rng, t, lba_count));
        t.entries.push(RawEntry { index, type_guid: Guid::EFI_SYSTEM, unique: rng.guid(), first, last, name: rng.name() });
        return;
    }
    let k = rng.below(t.entries.len() as u64) as usize;
    let other = t.entries[rng.below(t.entries.len() as u64) as usize];
    let value = edge(rng, t, lba_count);
    let e = &mut t.entries[k];
    match rng.below(9) {
        0 => e.first = value,
        1 => e.last = value,
        // The finding's own shape, and its `+ 1` twin.
        2 => (e.first, e.last) = (e.last, e.first),
        3 => (e.first, e.last) = (e.last.wrapping_add(1), e.last),
        4 => (e.first, e.last) = (0, u64::MAX),
        // Onto a neighbour's blocks.
        5 => (e.first, e.last) = (other.last, other.last.max(e.last)),
        6 => e.unique = other.unique,
        7 => e.type_guid = Guid::ZERO,
        _ => e.type_guid = Guid::TOYOS_DATA,
    }
}

/// Bend 1 to 3 values of the layout, mostly of the primary copy.
pub fn mutate(rng: &mut Rng, layout: &mut Layout) {
    for _ in 0..rng.range(1, 3) {
        let (lba_bytes, lba_count) = (layout.lba_bytes, layout.lba_count);
        match rng.below(40) {
            0 | 1 => layout.reported_lba_count = edge(rng, &layout.primary, lba_count).min(lba_count + 64),
            2 => layout.mbr_type = rng.pick(&[0x00, 0x07]),
            3 => layout.mbr_signature = [0x55, 0xAB],
            4..=11 if layout.backup.is_some() => {
                let backup = layout.backup.as_mut().expect("checked");
                mutate_table(rng, backup, lba_bytes, lba_count)
            }
            _ => mutate_table(rng, &mut layout.primary, lba_bytes, lba_count),
        }
    }
}

/// The image `layout` states, with both copies' CRCs computed over it.
pub fn image(layout: &Layout) -> Image {
    let lba = layout.lba_bytes as usize;
    let mut disk = vec![0u8; lba * layout.lba_count as usize];
    disk[446 + 4] = layout.mbr_type;
    disk[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    disk[446 + 12..446 + 16].copy_from_slice(&u32::try_from(layout.lba_count - 1).unwrap_or(u32::MAX).to_le_bytes());
    disk[510..512].copy_from_slice(&layout.mbr_signature);

    // Each copy at its own place whatever its `my_lba` says; the primary last.
    let copies: Vec<(u64, &Table)> =
        layout.backup.iter().map(|b| (layout.lba_count - 1, b)).chain([(1, &layout.primary)]).collect();
    for (_, t) in &copies {
        let stride = (t.entry_bytes as usize).max(128);
        for e in &t.entries {
            let at = (t.entry_array_lba as usize)
                .checked_mul(lba)
                .and_then(|a| a.checked_add(e.index as usize * stride));
            let Some(slot) = at.and_then(|at| disk.get_mut(at..at + 48)) else { continue };
            slot[..16].copy_from_slice(&e.type_guid.0);
            slot[16..32].copy_from_slice(&e.unique.0);
            slot[32..40].copy_from_slice(&e.first.to_le_bytes());
            slot[40..48].copy_from_slice(&e.last.to_le_bytes());
            // A nameless entry writes its first 48 bytes alone, so an array laid
            // over LBA 0 leaves the MBR's records as they are.
            let name = at.and_then(|at| disk.get_mut(at + 56..at + 128));
            if let Some(name) = name.filter(|_| e.name != [0; 36]) {
                for (pair, unit) in name.as_chunks_mut::<2>().0.iter_mut().zip(e.name) {
                    *pair = unit.to_le_bytes();
                }
            }
        }
    }
    for &(header_lba, t) in &copies {
        let at = header_lba as usize * lba;
        let mut h = vec![0u8; lba];
        h[..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&t.revision.to_le_bytes());
        h[12..16].copy_from_slice(&t.header_bytes.to_le_bytes());
        h[20..24].copy_from_slice(&t.reserved.to_le_bytes());
        h[24..32].copy_from_slice(&t.my_lba.to_le_bytes());
        let alternate = if header_lba == 1 { layout.lba_count - 1 } else { 1 };
        h[32..40].copy_from_slice(&alternate.to_le_bytes());
        h[40..48].copy_from_slice(&t.first_usable.to_le_bytes());
        h[48..56].copy_from_slice(&t.last_usable.to_le_bytes());
        h[56..72].copy_from_slice(&t.disk_guid.0);
        h[72..80].copy_from_slice(&t.entry_array_lba.to_le_bytes());
        h[80..84].copy_from_slice(&t.entry_count.to_le_bytes());
        h[84..88].copy_from_slice(&t.entry_bytes.to_le_bytes());
        let array_bytes = u64::from(t.entry_count) * u64::from(t.entry_bytes);
        let array = (t.entry_array_lba as usize)
            .checked_mul(lba)
            .zip(usize::try_from(array_bytes).ok())
            .and_then(|(at, len)| disk.get(at..at.checked_add(len)?));
        if let Some(array) = array {
            h[88..92].copy_from_slice(&crc32(array).to_le_bytes());
        }
        if let Some(covered) = h.get(..t.header_bytes as usize) {
            let crc = crc32(covered);
            h[16..20].copy_from_slice(&crc.to_le_bytes());
        }
        disk[at..at + lba].copy_from_slice(&h);
    }
    Image {
        lba_bytes: layout.lba_bytes,
        lba_count: layout.reported_lba_count,
        granularity: layout.granularity,
        bytes: disk,
        fail_at: None,
    }
}

pub struct Image {
    pub lba_bytes: u32,
    pub lba_count: u64,
    pub granularity: u64,
    pub bytes: Vec<u8>,
    /// The one block whose read does not happen.
    pub fail_at: Option<u64>,
}

impl Sectors for Image {
    fn lba_bytes(&self) -> u32 {
        self.lba_bytes
    }
    fn lba_count(&self) -> u64 {
        self.lba_count
    }
    fn lba_count_granularity(&self) -> core::num::NonZeroU64 {
        core::num::NonZeroU64::new(self.granularity).expect("1 or 8")
    }
    fn read_lba(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        if self.fail_at == Some(lba) {
            return false;
        }
        let at = (lba as usize).checked_mul(self.lba_bytes as usize);
        match at.and_then(|at| self.bytes.get(at..at.checked_add(buf.len())?)) {
            Some(src) => {
                buf.copy_from_slice(src);
                true
            }
            None => false,
        }
    }
}

/// One copy of the table as this file reads it, trusting nothing the parser decided.
#[derive(Debug)]
pub struct OnDisk {
    pub first_usable: u64,
    pub last_usable: u64,
    pub entries: Vec<RawEntry>,
}

impl OnDisk {
    /// Every copy an image carries that this file can read.
    pub fn both(img: &Image) -> Vec<OnDisk> {
        let lba = img.lba_bytes as usize;
        let last = img.bytes.len() / lba - 1;
        [1, last].into_iter().filter_map(|at| OnDisk::at(&img.bytes, lba, at)).collect()
    }

    fn at(disk: &[u8], lba: usize, header_lba: usize) -> Option<OnDisk> {
        let h = disk.get(header_lba * lba..(header_lba + 1) * lba)?;
        if &h[..8] != b"EFI PART" {
            return None;
        }
        let u64_at = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().expect("8"));
        let u32_at = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().expect("4"));
        let (count, size) = (u32_at(h, 80) as usize, u32_at(h, 84) as usize);
        if size < 48 || count.checked_mul(size)? > 1 << 20 {
            return None;
        }
        let array_at = usize::try_from(u64_at(h, 72)).ok()?.checked_mul(lba)?;
        let array = disk.get(array_at..array_at.checked_add(count * size)?)?;
        let entries = array
            .chunks_exact(size)
            .enumerate()
            .map(|(i, e)| RawEntry {
                index: i as u32,
                type_guid: Guid(e[..16].try_into().expect("16")),
                unique: Guid(e[16..32].try_into().expect("16")),
                first: u64_at(e, 32),
                last: u64_at(e, 40),
                name: e.get(56..128).map_or([0; 36], |field| {
                    std::array::from_fn(|i| u16::from_le_bytes([field[2 * i], field[2 * i + 1]]))
                }),
            })
            .filter(|e| !e.type_guid.is_zero())
            .collect();
        Some(OnDisk { first_usable: u64_at(h, 40), last_usable: u64_at(h, 48), entries })
    }

    pub fn entry(&self, index: u32) -> Option<&RawEntry> {
        self.entries.iter().find(|e| e.index == index)
    }

    /// Whether `e`'s blocks are a range inside this copy's usable blocks.
    pub fn places(&self, e: &RawEntry) -> bool {
        self.first_usable <= e.first && e.first <= e.last && e.last <= self.last_usable
    }
}
