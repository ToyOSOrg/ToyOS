//! Where a firmware window a signed image's row grants a claim lies, decided
//! from firmware's own words and checked against what the kernel owns.
//!
//! **Firmware is trusted, and every check here is a bug fence**: a window is
//! refused where a page of it is memory the kernel hands out, a page a device
//! decodes, or memory firmware's map does not keep for firmware.
//!
//! The IGD OpRegion's facts are Intel's "IGD OpRegion/Software SCI/_DSM for
//! Skylake Processors BIOS Specification", revision 0.5 (2016): the ASLS
//! register holds its physical address, its header starts with the signature
//! "IntelGraphicsMem" (§3.1.1) and gives the whole region's size in KB at 10h
//! (§3.1.2), and it is "Firmware Reserved", "Non-Volatile Storage (NVS)"
//! memory under 4 GB (§2.3).

use toyos_abi::boot::MemoryMapEntry;
use toyos_abi::pci::WindowRefused;

use crate::firmware::{EFI_ACPI_NVS, EFI_RESERVED};
use crate::span::PAGE_4K;

/// §3.1.1.
pub const OPREGION_SIGNATURE: &[u8; 16] = b"IntelGraphicsMem";
/// The header bytes held to §3.1.1-§3.1.2: the signature, then the size in
/// KB.
pub const OPREGION_HEADER: usize = 20;
/// The size §3.1.2 gives: "OpRegion Size = (incl. Header) in KB = 8 KB".
pub const OPREGION_KB: u32 = 8;
const OPREGION_BYTES: u64 = OPREGION_KB as u64 * 1024;

/// A window decided: `bytes` of whole pages from `start`, the window
/// `offset` bytes into them and `len` long.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Window {
    pub start: u64,
    pub bytes: u64,
    pub offset: u64,
    pub len: u64,
}

/// The OpRegion's 8 KB at `asls`, decided before a byte of it is read: every
/// page inside what the kernel's direct map reaches (`mapped_end`), reserved
/// or ACPI NVS in firmware's map, none memory the kernel hands out and none
/// a device decodes.
pub fn opregion(
    map: &[MemoryMapEntry],
    mapped_end: u64,
    devices: impl IntoIterator<Item = (u64, u64)>,
    asls: u32,
) -> Result<Window, WindowRefused> {
    let at = u64::from(asls);
    let start = at & !(PAGE_4K - 1);
    let bytes = (at + OPREGION_BYTES).next_multiple_of(PAGE_4K) - start;
    let end = start + bytes;
    if end > mapped_end {
        return Err(WindowRefused::NotFirmware);
    }
    let mut page = start;
    while page < end {
        let mut holding = map.iter().filter(|entry| (entry.start..entry.end).contains(&page));
        if holding.clone().any(|entry| toyos_bootmap::is_usable_type(entry.uefi_type)) {
            return Err(WindowRefused::Usable);
        }
        if !holding.any(|entry| matches!(entry.uefi_type, EFI_RESERVED | EFI_ACPI_NVS)) {
            return Err(WindowRefused::NotFirmware);
        }
        page += PAGE_4K;
    }
    if map.iter().any(|entry| toyos_bootmap::is_usable_type(entry.uefi_type) && entry.start < end && start < entry.end) {
        return Err(WindowRefused::Usable);
    }
    if devices.into_iter().any(|(first, past)| first < end && start < past) {
        return Err(WindowRefused::Device);
    }
    Ok(Window { start, bytes, offset: at - start, len: OPREGION_BYTES })
}

/// The header read from inside [`opregion`]'s window: its signature, and
/// the size §3.1.2 gives.
pub fn opregion_header(header: &[u8; OPREGION_HEADER]) -> Result<(), WindowRefused> {
    if &header[..16] != OPREGION_SIGNATURE {
        return Err(WindowRefused::Signature);
    }
    match u32::from_le_bytes(header[16..20].try_into().expect("four bytes")) {
        OPREGION_KB => Ok(()),
        _ => Err(WindowRefused::Size),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EFI_CONVENTIONAL: u32 = 7;
    const EFI_RUNTIME_DATA: u32 = 6;
    /// The direct map's end.
    const END: u64 = 0x1_0000_0000;

    fn entry(start: u64, end: u64, uefi_type: u32) -> MemoryMapEntry {
        MemoryMapEntry { start, end, uefi_type }
    }

    /// A T14-like map: RAM below, the OpRegion's NVS range, RAM above.
    fn map() -> [MemoryMapEntry; 3] {
        [
            entry(0x1000_0000, 0x4000_0000, EFI_CONVENTIONAL),
            entry(0x4000_0000, 0x4010_0000, EFI_ACPI_NVS),
            entry(0x4010_0000, 0x8000_0000, EFI_CONVENTIONAL),
        ]
    }

    fn header(signature: &[u8; 16], kb: u32) -> [u8; OPREGION_HEADER] {
        let mut out = [0u8; OPREGION_HEADER];
        out[..16].copy_from_slice(signature);
        out[16..].copy_from_slice(&kb.to_le_bytes());
        out
    }

    #[test]
    fn an_opregion_in_nvs_is_its_eight_kb_in_whole_pages() {
        assert_eq!(opregion(&map(), END, [], 0x4002_0018), Ok(Window { start: 0x4002_0000, bytes: 0x3000, offset: 0x18, len: 0x2000 }));
        assert_eq!(opregion_header(&header(OPREGION_SIGNATURE, 8)), Ok(()));
    }

    #[test]
    fn a_header_firmware_did_not_write_is_refused() {
        assert_eq!(opregion_header(&header(b"IntelGraphicsMeM", 8)), Err(WindowRefused::Signature));
        for kb in [0, 7, 9, 64, u32::MAX] {
            assert_eq!(opregion_header(&header(OPREGION_SIGNATURE, kb)), Err(WindowRefused::Size), "{kb} KB");
        }
    }

    /// A window reaching RAM the kernel hands out, memory firmware keeps for
    /// something else, an unlisted address, a device's window or past the
    /// direct map is refused.
    #[test]
    fn a_window_past_firmwares_own_memory_is_refused() {
        assert_eq!(opregion(&map(), END, [], 0x400f_f000), Err(WindowRefused::Usable), "its last pages are RAM");
        assert_eq!(opregion(&map(), END, [], 0x3fff_f000), Err(WindowRefused::Usable));
        assert_eq!(opregion(&[entry(0x4000_0000, 0x4010_0000, EFI_RUNTIME_DATA)], END, [], 0x4000_0000), Err(WindowRefused::NotFirmware));
        assert_eq!(opregion(&map(), END, [], 0x9000_0000), Err(WindowRefused::NotFirmware), "unlisted");
        assert_eq!(opregion(&map(), 0x4000_1000, [], 0x4000_0000), Err(WindowRefused::NotFirmware), "past the direct map");
        assert_eq!(opregion(&map(), END, [(0x4002_1000, 0x4002_2000)], 0x4002_0000), Err(WindowRefused::Device));
        let overlapping = [entry(0x4000_0000, 0x4010_0000, EFI_ACPI_NVS), entry(0x4002_1000, 0x4002_2000, EFI_CONVENTIONAL)];
        assert_eq!(opregion(&overlapping, END, [], 0x4002_0000), Err(WindowRefused::Usable), "RAM wherever the map lists it");
    }
}
