//! A firmware file's container (§6): an 88-byte header, then records back to
//! back, each a kind, a length and its contents, padded to 4 bytes. Of a
//! section only its load address is read; its bytes are handed to the device
//! unread.

use alloc::vec::Vec;

use crate::bytes::{u16_at, u32_at, words};
use crate::part::Hardware;
use crate::versions::Declared;

pub const MAGIC: u32 = 0x0a4c_5749;
pub const HEADER_BYTES: usize = 88;
const NAME_AT: usize = 8;
const NAME_BYTES: usize = 64;
const VERSION_AT: usize = 72;
const BUILD_AT: usize = 76;

/// The record kinds this loader reads (§6.3).
pub mod kind {
    pub const PROBE_MAX: u32 = 6;
    pub const FLAGS: u32 = 18;
    pub const SECTION: u32 = 19;
    pub const PHY_CONFIG: u32 = 23;
    pub const CPUS: u32 = 27;
    pub const API: u32 = 29;
    pub const CAPABILITIES: u32 = 30;
    pub const SCAN_CHANNELS: u32 = 31;
    pub const VERSION: u32 = 36;
    pub const COMMAND_VERSIONS: u32 = 48;
    pub const IMAGE_LOADER: u32 = 52;
    pub const UMAC_DEBUG: u32 = 54;
    pub const LMAC_DEBUG: u32 = 55;
    /// 12 bytes in every pinned file, of which only the MAC and RF types,
    /// the first two u16s, are named; the rest is left unread.
    pub const HARDWARE: u32 = 58;
    pub const STATIONS: u32 = 0x100;
}

/// Kinds the loader skips: their contents decide nothing it does (§6.3).
/// Kind 32, the paging size, is among them: the paging image's sections say
/// its size.
fn ignored(kind: u32) -> bool {
    matches!(kind, 7 | 22 | 32 | 51 | 57 | 60 | 61 | 68 | 69 | 0x101 | 0x102 | 0x100_0005..=0x100_000c)
}

/// The load address that ends the LMAC image, and the one that ends the UMAC
/// image; the sections after the second are the paging image (§6.4).
pub const LMAC_END: u32 = 0xffff_cccc;
pub const UMAC_END: u32 = 0xaaaa_bbbb;

/// The CPUs a file for these parts runs on, one per MAC (§6.3).
pub const CPUS: u32 = 2;

/// The words a set of flag bits fills: bit 256 and above names no word.
const FLAG_WORDS: usize = 8;

/// One record: its kind, its contents, and where it starts in the file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Record<'a> {
    pub(crate) at: usize,
    pub(crate) kind: u32,
    pub(crate) contents: &'a [u8],
}

/// The records from `at` to the end of `bytes` (§6.2), stopping at the first
/// that runs past it, which it yields as `Err` with that record's offset.
pub(crate) fn records(bytes: &[u8], at: usize) -> Records<'_> {
    Records { bytes, at: Some(at) }
}

pub(crate) struct Records<'a> {
    bytes: &'a [u8],
    at: Option<usize>,
}

impl<'a> Iterator for Records<'a> {
    type Item = Result<Record<'a>, usize>;

    fn next(&mut self) -> Option<Self::Item> {
        let at = self.at.filter(|&at| at != self.bytes.len())?;
        let record = record(self.bytes, at);
        self.at = match record {
            Ok((_, next)) => Some(next),
            Err(()) => None,
        };
        Some(record.map(|(record, _)| record).map_err(|()| at))
    }
}

/// The record at `at` and where the next one starts. A file may end at the
/// end of its last record's contents, without that record's padding.
fn record(bytes: &[u8], at: usize) -> Result<(Record<'_>, usize), ()> {
    let kind = u32_at(bytes, at).ok_or(())?;
    let len = u32_at(bytes, at.checked_add(4).ok_or(())?).ok_or(())?;
    let start = at.checked_add(8).ok_or(())?;
    let end = start.checked_add(usize::try_from(len).map_err(|_| ())?).ok_or(())?;
    let contents = bytes.get(start..end).ok_or(())?;
    let padded = end.checked_next_multiple_of(4).ok_or(())?;
    let next = match padded <= bytes.len() {
        true => padded,
        false if end == bytes.len() => end,
        false => return Err(()),
    };
    Ok((Record { at, kind, contents }, next))
}

/// Which of a file's three runtime images a section belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Image {
    Lmac,
    Umac,
    Paging,
}

/// One section: where the device places it, and its bytes, unread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section<'a> {
    pub load: u32,
    pub bytes: &'a [u8],
}

/// The firmware's version record (kind 36).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub local: u32,
}

/// A set of flag bits, filled one 32-bit word per record (kinds 29 and 30).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bits([u32; FLAG_WORDS]);

impl Bits {
    pub fn has(&self, bit: u32) -> bool {
        let (word, bit) = (bit / 32, bit % 32);
        usize::try_from(word).ok().and_then(|w| self.0.get(w)).is_some_and(|w| w & (1 << bit) != 0)
    }

    /// Every bit set, in order.
    pub fn set(&self) -> impl Iterator<Item = u32> + '_ {
        (0..32 * 8).filter(|&bit| self.has(bit))
    }

    /// One record's word into its place; `given` is the words already filled.
    fn fill(&mut self, given: &mut u8, kind: u32, contents: &[u8], at: usize) -> Result<(), Refused> {
        let length = Refused::Length { kind, at, len: contents.len() };
        let (index, bits) = match (contents.len(), u32_at(contents, 0), u32_at(contents, 4)) {
            (8, Some(index), Some(bits)) => (index, bits),
            _ => return Err(length),
        };
        let word = usize::try_from(index).ok().and_then(|i| self.0.get_mut(i)).ok_or(Refused::WordIndex { kind, index })?;
        let mask = 1u8 << index;
        if *given & mask != 0 {
            return Err(Refused::Repeated { kind, at });
        }
        *given |= mask;
        *word = bits;
        Ok(())
    }
}

/// What a loader reads out of one firmware file (§6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Firmware<'a> {
    /// The build's name, without its NUL padding.
    pub name: &'a [u8],
    /// The header's API number.
    pub api_version: u32,
    pub build: u32,
    pub version: Version,
    /// The hardware the file is for (kind 58), which a part must read as (§1).
    pub hardware: Hardware,
    /// Kind 18: bit 2 management-frame protection, bit 24 U-APSD.
    pub flags: u32,
    /// Kind 23: transmit chains in bits 19:16, receive chains in 23:20.
    pub phy_config: u32,
    /// The longest probe request the scan's template may hold (kind 6).
    pub probe_max: u32,
    /// The most channels one scan request may list (kind 31).
    pub scan_channels: u32,
    /// Kind 0x100.
    pub stations: u32,
    pub api: Bits,
    pub capabilities: Bits,
    pub lmac: Vec<Section<'a>>,
    pub umac: Vec<Section<'a>>,
    pub paging: Vec<Section<'a>>,
    /// The image loader the second loading generation hands over (kind 52).
    pub loader: Option<&'a [u8]>,
    /// Device-memory addresses of the error tables (§12.2).
    pub lmac_error_table: u32,
    pub umac_error_table: u32,
    /// What the file declares of each message (kind 48).
    pub declared: Declared,
}

/// Why a firmware file is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Shorter than the header.
    Short,
    /// The header's first word is not zero: an older format.
    Format(u32),
    Magic(u32),
    /// The record at this offset runs past the file's end.
    Truncated { at: usize },
    /// A kind this loader neither reads nor knows to skip.
    Unknown { kind: u32, at: usize },
    /// A record whose contents are not the length its kind has.
    Length { kind: u32, at: usize, len: usize },
    /// A second record of a kind there is one of, or a flag word given twice.
    Repeated { kind: u32, at: usize },
    Missing(u32),
    /// A flag word past the [`FLAG_WORDS`] this loader holds.
    WordIndex { kind: u32, index: u32 },
    /// A separator out of its order: an image would be ended twice or never.
    Separator { at: usize },
    /// An image without a section.
    Empty(Image),
    Cpus(u32),
}

/// The singleton records, each read once.
#[derive(Default)]
struct Found<'a> {
    version: Option<Version>,
    hardware: Option<Hardware>,
    flags: Option<u32>,
    phy_config: Option<u32>,
    probe_max: Option<u32>,
    scan_channels: Option<u32>,
    stations: Option<u32>,
    cpus: Option<u32>,
    loader: Option<&'a [u8]>,
    lmac_error_table: Option<u32>,
    umac_error_table: Option<u32>,
    declared: Option<Declared>,
}

/// `value` into `slot`, refusing a second record of its kind.
fn once<T>(slot: &mut Option<T>, value: T, record: &Record<'_>) -> Result<(), Refused> {
    match slot.replace(value) {
        None => Ok(()),
        Some(_) => Err(Refused::Repeated { kind: record.kind, at: record.at }),
    }
}

fn length(record: &Record<'_>) -> Refused {
    Refused::Length { kind: record.kind, at: record.at, len: record.contents.len() }
}

/// A record that is one u32.
fn word(record: &Record<'_>) -> Result<u32, Refused> {
    match record.contents.len() {
        4 => u32_at(record.contents, 0).ok_or_else(|| length(record)),
        _ => Err(length(record)),
    }
}

/// The first u32 of a record of whole words.
fn first_word(record: &Record<'_>) -> Result<u32, Refused> {
    words(record.contents).and_then(|mut w| w.next()).ok_or_else(|| length(record))
}

impl<'a> Firmware<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Firmware<'a>, Refused> {
        let header = bytes.get(..HEADER_BYTES).ok_or(Refused::Short)?;
        match u32_at(header, 0) {
            Some(0) => {}
            other => return Err(Refused::Format(other.unwrap_or_default())),
        }
        let magic = u32_at(header, 4).unwrap_or_default();
        if magic != MAGIC {
            return Err(Refused::Magic(magic));
        }
        let name = header.get(NAME_AT..NAME_AT.saturating_add(NAME_BYTES)).unwrap_or_default();
        let name = name.split(|&b| b == 0).next().unwrap_or_default();
        let api_version = u32_at(header, VERSION_AT).unwrap_or_default();
        let build = u32_at(header, BUILD_AT).unwrap_or_default();

        let mut found = Found::default();
        let (mut api, mut capabilities) = (Bits::default(), Bits::default());
        let (mut api_given, mut capabilities_given) = (0, 0);
        let (mut lmac, mut umac, mut paging) = (Vec::new(), Vec::new(), Vec::new());
        let mut image = Image::Lmac;
        for record in records(bytes, HEADER_BYTES) {
            let record = record.map_err(|at| Refused::Truncated { at })?;
            match record.kind {
                kind::SECTION => {
                    let load = u32_at(record.contents, 0).ok_or_else(|| length(&record))?;
                    image = match (load, image) {
                        (LMAC_END, Image::Lmac) => Image::Umac,
                        (UMAC_END, Image::Umac) => Image::Paging,
                        (LMAC_END | UMAC_END, _) => return Err(Refused::Separator { at: record.at }),
                        _ => {
                            let section = Section { load, bytes: record.contents.get(4..).unwrap_or_default() };
                            match image {
                                Image::Lmac => lmac.push(section),
                                Image::Umac => umac.push(section),
                                Image::Paging => paging.push(section),
                            }
                            image
                        }
                    };
                }
                kind::VERSION => {
                    let v = match (record.contents.len(), words(record.contents)) {
                        (12, Some(mut w)) => (w.next(), w.next(), w.next()),
                        _ => return Err(length(&record)),
                    };
                    let (Some(major), Some(minor), Some(local)) = v else { return Err(length(&record)) };
                    once(&mut found.version, Version { major, minor, local }, &record)?;
                }
                kind::HARDWARE => {
                    let hardware = match (u16_at(record.contents, 0), u16_at(record.contents, 2)) {
                        (Some(mac), Some(rf)) => Hardware { mac, rf },
                        _ => return Err(length(&record)),
                    };
                    once(&mut found.hardware, hardware, &record)?;
                }
                kind::FLAGS => once(&mut found.flags, word(&record)?, &record)?,
                kind::PHY_CONFIG => once(&mut found.phy_config, word(&record)?, &record)?,
                kind::PROBE_MAX => once(&mut found.probe_max, word(&record)?, &record)?,
                kind::SCAN_CHANNELS => once(&mut found.scan_channels, word(&record)?, &record)?,
                kind::STATIONS => once(&mut found.stations, word(&record)?, &record)?,
                kind::CPUS => once(&mut found.cpus, word(&record)?, &record)?,
                kind::API => api.fill(&mut api_given, record.kind, record.contents, record.at)?,
                kind::CAPABILITIES => {
                    capabilities.fill(&mut capabilities_given, record.kind, record.contents, record.at)?
                }
                kind::IMAGE_LOADER => once(&mut found.loader, record.contents, &record)?,
                kind::LMAC_DEBUG => once(&mut found.lmac_error_table, first_word(&record)?, &record)?,
                kind::UMAC_DEBUG => once(&mut found.umac_error_table, first_word(&record)?, &record)?,
                kind::COMMAND_VERSIONS => {
                    let declared = Declared::parse(record.contents).ok_or_else(|| length(&record))?;
                    once(&mut found.declared, declared, &record)?;
                }
                kind if ignored(kind) => {}
                kind => return Err(Refused::Unknown { kind, at: record.at }),
            }
        }
        if image != Image::Paging {
            return Err(Refused::Separator { at: bytes.len() });
        }
        for (sections, image) in [(&lmac, Image::Lmac), (&umac, Image::Umac), (&paging, Image::Paging)] {
            if sections.is_empty() {
                return Err(Refused::Empty(image));
            }
        }
        let cpus = found.cpus.ok_or(Refused::Missing(kind::CPUS))?;
        if cpus != CPUS {
            return Err(Refused::Cpus(cpus));
        }
        Ok(Firmware {
            name,
            api_version,
            build,
            version: found.version.ok_or(Refused::Missing(kind::VERSION))?,
            hardware: found.hardware.ok_or(Refused::Missing(kind::HARDWARE))?,
            flags: found.flags.ok_or(Refused::Missing(kind::FLAGS))?,
            phy_config: found.phy_config.ok_or(Refused::Missing(kind::PHY_CONFIG))?,
            probe_max: found.probe_max.ok_or(Refused::Missing(kind::PROBE_MAX))?,
            scan_channels: found.scan_channels.ok_or(Refused::Missing(kind::SCAN_CHANNELS))?,
            stations: found.stations.ok_or(Refused::Missing(kind::STATIONS))?,
            api,
            capabilities,
            lmac,
            umac,
            paging,
            loader: found.loader,
            lmac_error_table: found.lmac_error_table.ok_or(Refused::Missing(kind::LMAC_DEBUG))?,
            umac_error_table: found.umac_error_table.ok_or(Refused::Missing(kind::UMAC_DEBUG))?,
            declared: found.declared.ok_or(Refused::Missing(kind::COMMAND_VERSIONS))?,
        })
    }

    /// The sections of `image`, in load order.
    pub fn image(&self, image: Image) -> &[Section<'a>] {
        match image {
            Image::Lmac => &self.lmac,
            Image::Umac => &self.umac,
            Image::Paging => &self.paging,
        }
    }
}
