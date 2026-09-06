//! The page a panicked kernel leaves behind for the next boot to find.
//!
//! A ramoops-shaped black box, and the one channel a crash has on a machine
//! with no serial port and nobody in the room: the kernel writes what the panel
//! renders into one page of DRAM, resets, and the next boot's loader copies it
//! into `loader.log` before anything else can use that memory.
//!
//! **The invariant the whole thing stands on is that a warm reset through the
//! FADT's reset register leaves DRAM untouched on the machines this runs on.**
//! Nothing here believes it: [`seal`] writes a magic and a checksum over the
//! recorded bytes, [`recover`] answers `None` for anything that does not check
//! out, and a page that came back as noise is a page with nothing in it. The
//! checksum is what decides, never the reset's reputation.
//!
//! Pure: bytes in, bytes out. The loader owns the allocation and the kernel
//! owns the write, and neither of them can be asked what it does with a
//! corrupt page.

#![no_std]
#![forbid(unsafe_code)]

/// Where the page lives, chosen once and named by both binaries.
///
/// Fixed, because the loader has to find last boot's page before it has been
/// told anything, and there is nowhere to have been told it from. Page-aligned
/// so `AllocatePages` can name it; inside `toyos_bootmap::BOOT_MAP_BYTES` so a
/// panic before `mm::init` can still reach it through the boot map; and high
/// enough above the megabyte firmware keeps its own furniture in that no
/// machine this kernel boots has it spoken for. A machine whose firmware
/// disagrees refuses the allocation and says so — there is no second address
/// to fall back to, because a fallback is one more thing the next boot would
/// have to guess at.
pub const PHYS: u64 = 0x0800_0000;

/// One page, which is what `AllocatePages` deals in and all a report needs.
pub const BYTES: usize = 4096;

/// The UEFI memory type the loader allocates the page as.
///
/// **This is also how the kernel is told the allocation happened**, and it is
/// why the type is in the OS-defined range (UEFI 2.10 §7.2, `0x80000000` and
/// above, "reserved for use by UEFI OS loaders"): firmware may not produce one,
/// so an entry of this type in the memory map the kernel is handed came from
/// this loader and from nothing else. The kernel needs no field of its own for
/// the question and there is no flag either side can get wrong — the map says
/// it, or the page is not this kernel's to write.
///
/// The low half is arbitrary. All it has to be is one value both binaries name.
pub const MEMORY_TYPE: u32 = 0x8000_B0C5;

/// UEFI 2.10 §7.2 gives `0x80000000` and above to OS loaders, which is the
/// whole of how the kernel knows the loader claimed this page; `AllocatePages`
/// can only be given an address it deals in.
const _: () = {
    assert!(MEMORY_TYPE >= 0x8000_0000);
    assert!(PHYS.is_multiple_of(BYTES as u64));
};

/// `PANC`, big-endian ASCII, so a hexdump of the page reads.
const MAGIC: u32 = 0x5041_4E43;

/// Magic, then the recorded length, then the checksum.
const HEADER: usize = 12;

/// What one report may leave behind. Longer is truncated at its head, because
/// the tail of a panel is the crash and the head is how the boot went.
pub const TEXT_BYTES: usize = BYTES - HEADER;

/// Write `text` into `page` and seal it, returning the bytes recorded.
///
/// Truncation keeps the *tail*: the newest of what the panel rendered is the
/// crash itself, and a report cut off before it is a report about nothing.
/// The kernel calls this from inside its panic path's no-lock, no-allocation,
/// nothing-may-panic region, so nothing here indexes or unwraps: a bounds check
/// that could panic would take the machine down inside the report about why it
/// went down.
pub fn seal(page: &mut [u8; BYTES], text: &[u8]) -> usize {
    let from = text.len().saturating_sub(TEXT_BYTES);
    let kept = text.get(from..).unwrap_or(&[]);
    if let Some(slot) = page.get_mut(HEADER..HEADER.saturating_add(kept.len())) {
        slot.copy_from_slice(kept);
    }
    put(page, 0, MAGIC);
    put(page, 4, kept.len() as u32);
    // Written last, so a machine that stopped mid-copy leaves a length and a
    // magic the checksum then refuses, rather than a report with a hole in it.
    put(page, 8, checksum(kept));
    kept.len()
}

fn put(page: &mut [u8; BYTES], at: usize, value: u32) {
    if let Some(slot) = page.get_mut(at..at + 4) {
        slot.copy_from_slice(&value.to_le_bytes());
    }
}

/// The text a previous boot sealed into `page`, or `None` for a page that
/// carries none — never written, cleared, reused by firmware, or corrupted.
pub fn recover(page: &[u8; BYTES]) -> Option<&[u8]> {
    if u32::from_le_bytes(head(page, 0)) != MAGIC {
        return None;
    }
    let len = u32::from_le_bytes(head(page, 4)) as usize;
    let text = page.get(HEADER..HEADER.checked_add(len)?)?;
    (checksum(text) == u32::from_le_bytes(head(page, 8))).then_some(text)
}

/// Take the magic off, so the same report is not harvested by a third boot.
/// The header alone: the text stays where it is and answers to nothing without it.
pub fn clear(page: &mut [u8; BYTES]) {
    if let Some(header) = page.get_mut(..HEADER) {
        header.fill(0);
    }
}

fn head(page: &[u8; BYTES], at: usize) -> [u8; 4] {
    let mut out = [0u8; 4];
    if let Some(slot) = page.get(at..at + 4) {
        out.copy_from_slice(slot);
    }
    out
}

/// FNV-1a, 32-bit, over the length and then the bytes.
///
/// The length is folded in so a report truncated to a prefix of itself does not
/// keep checking out — a torn write is exactly that shape.
fn checksum(text: &[u8]) -> u32 {
    const OFFSET_BASIS: u32 = 0x811c_9dc5;
    const PRIME: u32 = 0x0100_0193;
    let mut hash = OFFSET_BASIS;
    for byte in (text.len() as u32).to_le_bytes().iter().chain(text) {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec;

    use super::*;

    fn blank() -> [u8; BYTES] {
        [0u8; BYTES]
    }

    #[test]
    fn a_sealed_page_comes_back_as_what_went_in() {
        let mut page = blank();
        assert_eq!(seal(&mut page, b"PANIC: nobody was there"), 23);
        assert_eq!(recover(&page), Some(&b"PANIC: nobody was there"[..]));
    }

    /// The state every first boot is in, and the one a false positive would
    /// turn into a report about a crash that never happened.
    #[test]
    fn a_page_nobody_wrote_carries_nothing() {
        assert_eq!(recover(&blank()), None);
        assert_eq!(recover(&[0xffu8; BYTES]), None);
        let mut noise = blank();
        for (i, byte) in noise.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(31).wrapping_add(7);
        }
        assert_eq!(recover(&noise), None);
    }

    #[test]
    fn a_cleared_page_is_not_harvested_twice() {
        let mut page = blank();
        seal(&mut page, b"the first boot's panic");
        assert!(recover(&page).is_some());
        clear(&mut page);
        assert_eq!(recover(&page), None);
    }

    /// Every single-byte corruption of a sealed page is refused: the text, the
    /// length and the checksum itself. This is the whole of what stands between
    /// a report and DRAM the reset did not preserve.
    #[test]
    fn one_flipped_bit_anywhere_refuses_the_whole_page() {
        let text = b"PANIC: kernel/src/main.rs:1:1: this machine is on fire";
        let mut sealed = blank();
        seal(&mut sealed, text);
        for at in 0..HEADER + text.len() {
            let mut page = sealed;
            page[at] ^= 0x40;
            assert_ne!(recover(&page), Some(&text[..]), "byte {at} was allowed to change");
        }
    }

    /// A length past the page is a header, not a panic: the read is refused
    /// rather than reaching past what was recorded.
    #[test]
    fn a_length_past_the_page_is_refused() {
        let mut page = blank();
        put(&mut page, 0, MAGIC);
        put(&mut page, 4, u32::MAX);
        assert_eq!(recover(&page), None);
        put(&mut page, 4, TEXT_BYTES as u32 + 1);
        assert_eq!(recover(&page), None);
    }

    /// A report longer than the page keeps its tail, because the tail is the
    /// crash and the head is how the boot went.
    #[test]
    fn an_over_long_report_keeps_its_tail() {
        let mut text = vec![b'.'; TEXT_BYTES + 100];
        text.extend_from_slice(b"PANIC: the last line");
        let mut page = blank();
        assert_eq!(seal(&mut page, &text), TEXT_BYTES);
        let back = recover(&page).expect("a truncated report is still sealed");
        assert_eq!(back.len(), TEXT_BYTES);
        assert!(back.ends_with(b"PANIC: the last line"));
    }

    /// An empty report seals and recovers as one, rather than reading as a page
    /// nothing wrote: a panic with nothing captured is still a panic.
    #[test]
    fn an_empty_report_is_not_an_absent_one() {
        let mut page = blank();
        assert_eq!(seal(&mut page, b""), 0);
        assert_eq!(recover(&page), Some(&b""[..]));
    }


}
