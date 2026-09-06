//! The page a boot leaves behind for the next boot to find, and the three
//! things it can say.
//!
//! A ramoops-shaped black box. The loader claims one page, writes [`State::Armed`]
//! into it and hands the machine to the kernel; whatever ends that kernel, the
//! next boot is the loader again, and it reads the page to learn which of the
//! three ways the last one went:
//!
//! - [`State::Panic`] — the kernel's panic path sealed what the panel rendered.
//! - [`State::Done`] — the kernel handed the machine back on purpose.
//! - [`State::Armed`] — neither, so it died before reaching either, and that
//!   absence is itself the finding.
//!
//! **The invariant the whole thing stands on is that a reset leaves DRAM
//! untouched on the machines this runs on.** Nothing here believes it: a magic
//! and an FNV-1a checksum over the state and the recorded bytes decide, and a
//! page that does not check out is a page with nothing in it — a cold power
//! cycle, a first boot, or DRAM the reset did not preserve, all one answer.
//!
//! **The page carries an ordinary UEFI memory type and the kernel is told where
//! it is on its parameter line.** It used to carry a type of its own out of the
//! range UEFI 2.10 §7.2 reserves for OS loaders, which the kernel read back out
//! of the memory map — an exact channel that cost nothing until the owner's
//! firmware stopped returning from `ExitBootServices` with one of those
//! descriptors in the map it was handed. Nothing in that map is ours any more.
//!
//! Pure: bytes in, bytes out. The loader owns the claim and the harvest, the
//! kernel owns the two writes, and neither can be asked what it does with a
//! corrupt page.

#![no_std]
#![forbid(unsafe_code)]

/// Where the page is, chosen once and named by the loader that allocates it.
///
/// Fixed, because the loader has to find last boot's page before it has been
/// told anything, and there is nowhere to have been told it from. The *kernel*
/// is told, on its parameter line, so that a boot whose claim firmware refused
/// is one the kernel knows about rather than one it has to infer. Page-aligned
/// so `AllocatePages` can name it, and inside `toyos_bootmap::BOOT_MAP_BYTES` so
/// a panic before `mm::init` still reaches it through the boot map.
pub const PHYS: u64 = 0x0800_0000;

/// One page, which is what `AllocatePages` deals in and all a report needs.
pub const BYTES: usize = 4096;

/// The parameter the loader appends to what it read off the ESP, with the
/// page's address after it. The kernel claims the token (`kernel/src/params.rs`)
/// and reserves the page it names before its allocator takes the memory map.
pub const PARAM: &str = "blackbox=";

/// `PANC`, big-endian ASCII, so a hexdump of the page reads.
const MAGIC: u32 = 0x5041_4E43;

/// Magic, state, length, checksum.
const HEADER: usize = 16;

/// What one report may leave behind. Longer is truncated at its head, because
/// the tail of a panel is the crash and the head is how the boot went.
pub const TEXT_BYTES: usize = BYTES - HEADER;

/// What the last boot got as far as saying.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum State {
    /// The loader handed the machine to a kernel and nothing has said otherwise
    /// since. On the next boot this means the kernel died without reaching
    /// either of the two paths that write, which is a finding and not an absence.
    Armed = 1,
    /// The kernel's panic path sealed what the panel rendered, and the text is
    /// that report.
    Panic = 2,
    /// The kernel handed the machine back on purpose, so the chain ends here
    /// and the firmware's own boot order takes the machine next.
    Done = 3,
}

impl State {
    const fn code(self) -> u32 {
        self as u32
    }

    const fn of(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Armed),
            2 => Some(Self::Panic),
            3 => Some(Self::Done),
            // Not a state this tree writes: the page holds something else, or
            // something else holds the page.
            _ => None,
        }
    }

    /// One word for a log line, so the loader and a reader of the stick cannot
    /// spell the same state two ways.
    pub const fn named(self) -> &'static str {
        match self {
            Self::Armed => "ARMED",
            Self::Panic => "PANIC",
            Self::Done => "DONE",
        }
    }
}

/// Write `state` and `text` into `page` and seal it, returning the bytes recorded.
///
/// Truncation keeps the *tail*: the newest of what the panel rendered is the
/// crash itself, and a report cut off before it is a report about nothing.
///
/// The kernel calls this from inside its panic path's no-lock, no-allocation,
/// nothing-may-panic region, so nothing here indexes or unwraps: a bounds check
/// that could panic would take the machine down inside the report about why it
/// went down.
pub fn seal(page: &mut [u8; BYTES], state: State, text: &[u8]) -> usize {
    let from = text.len().saturating_sub(TEXT_BYTES);
    let kept = text.get(from..).unwrap_or(&[]);
    if let Some(slot) = page.get_mut(HEADER..HEADER.saturating_add(kept.len())) {
        slot.copy_from_slice(kept);
    }
    put(page, 0, MAGIC);
    put(page, 4, state.code());
    put(page, 8, kept.len() as u32);
    // Written last, so a machine that stopped mid-copy leaves a header the
    // checksum then refuses, rather than a report with a hole in it.
    put(page, 12, checksum(state, kept));
    kept.len()
}

fn put(page: &mut [u8; BYTES], at: usize, value: u32) {
    if let Some(slot) = page.get_mut(at..at + 4) {
        slot.copy_from_slice(&value.to_le_bytes());
    }
}

/// What a previous boot sealed into `page`, or `None` for a page that carries
/// nothing this tree wrote — never written, cleared, reused, or corrupted.
pub fn recover(page: &[u8; BYTES]) -> Option<(State, &[u8])> {
    if u32::from_le_bytes(head(page, 0)) != MAGIC {
        return None;
    }
    let state = State::of(u32::from_le_bytes(head(page, 4)))?;
    let len = u32::from_le_bytes(head(page, 8)) as usize;
    let text = page.get(HEADER..HEADER.checked_add(len)?)?;
    (checksum(state, text) == u32::from_le_bytes(head(page, 12))).then_some((state, text))
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

/// FNV-1a, 32-bit, over the state, the length and then the bytes.
///
/// The state is folded in so one sealed page cannot be re-read as another
/// state, and the length so a report truncated to a prefix of itself does not
/// keep checking out — a torn write is exactly that shape.
fn checksum(state: State, text: &[u8]) -> u32 {
    const OFFSET_BASIS: u32 = 0x811c_9dc5;
    const PRIME: u32 = 0x0100_0193;
    let mut hash = OFFSET_BASIS;
    let stamp = [state.code().to_le_bytes(), (text.len() as u32).to_le_bytes()];
    for byte in stamp.iter().flatten().chain(text) {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// The address a [`PARAM`] token names, or `None` for a token that is not one.
///
/// The loader writes it with `{:#x}` and the kernel reads it back here: one
/// spelling, so the two cannot drift into agreeing about different bytes.
pub fn address_of(token: &str) -> Option<u64> {
    let digits = token.strip_prefix(PARAM)?.strip_prefix("0x")?;
    if digits.is_empty() || digits.len() > 16 {
        return None;
    }
    let mut value: u64 = 0;
    for byte in digits.bytes() {
        let nibble = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            // Upper case is refused rather than accepted: `{:#x}` renders lower.
            _ => return None,
        };
        value = value << 4 | u64::from(nibble);
    }
    Some(value)
}

const _: () = {
    // UEFI deals in pages, and `AllocatePages` can only be given an address it deals in.
    assert!(PHYS.is_multiple_of(BYTES as u64));
    assert!(HEADER.is_multiple_of(4));
};

#[cfg(test)]
mod tests {
    extern crate std;
    use std::{format, vec};

    use super::*;

    const STATES: [State; 3] = [State::Armed, State::Panic, State::Done];

    fn blank() -> [u8; BYTES] {
        [0u8; BYTES]
    }

    #[test]
    fn a_sealed_page_comes_back_as_what_went_in() {
        for state in STATES {
            let mut page = blank();
            assert_eq!(seal(&mut page, state, b"PANIC: nobody was there"), 23);
            assert_eq!(recover(&page), Some((state, &b"PANIC: nobody was there"[..])));
        }
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
        seal(&mut page, State::Panic, b"the first boot's panic");
        assert!(recover(&page).is_some());
        clear(&mut page);
        assert_eq!(recover(&page), None);
    }

    /// **The state is inside the checksum**, so one sealed page cannot be read
    /// back as another: an ARMED page reported as a panic would invent a crash,
    /// and a panicked one reported as ARMED would lose the report.
    #[test]
    fn no_state_can_be_read_as_another() {
        for state in STATES {
            let mut page = blank();
            seal(&mut page, state, b"whatever the last boot said");
            for other in STATES.into_iter().filter(|s| *s != state) {
                let mut forged = page;
                forged[4..8].copy_from_slice(&other.code().to_le_bytes());
                assert_eq!(recover(&forged), None, "{state:?} was readable as {other:?}");
            }
        }
    }

    /// Every single-byte corruption of a sealed page is refused: the text, the
    /// header and the checksum itself. This is the whole of what stands between
    /// a report and DRAM a reset did not preserve.
    #[test]
    fn one_flipped_bit_anywhere_refuses_the_whole_page() {
        let text = b"PANIC: kernel/src/main.rs:1:1: this machine is on fire";
        let mut sealed = blank();
        seal(&mut sealed, State::Panic, text);
        for at in 0..HEADER + text.len() {
            let mut page = sealed;
            page[at] ^= 0x40;
            assert_ne!(
                recover(&page),
                Some((State::Panic, &text[..])),
                "byte {at} was allowed to change"
            );
        }
    }

    /// A length past the page is a header, not a report: the read is refused
    /// rather than reaching past what was recorded.
    #[test]
    fn a_length_past_the_page_is_refused() {
        let mut page = blank();
        put(&mut page, 0, MAGIC);
        put(&mut page, 4, State::Panic.code());
        put(&mut page, 8, u32::MAX);
        assert_eq!(recover(&page), None);
        put(&mut page, 8, TEXT_BYTES as u32 + 1);
        assert_eq!(recover(&page), None);
    }

    /// A report longer than the page keeps its tail, because the tail is the
    /// crash and the head is how the boot went.
    #[test]
    fn an_over_long_report_keeps_its_tail() {
        let mut text = vec![b'.'; TEXT_BYTES + 100];
        text.extend_from_slice(b"PANIC: the last line");
        let mut page = blank();
        assert_eq!(seal(&mut page, State::Panic, &text), TEXT_BYTES);
        let (state, back) = recover(&page).expect("a truncated report is still sealed");
        assert_eq!(state, State::Panic);
        assert_eq!(back.len(), TEXT_BYTES);
        assert!(back.ends_with(b"PANIC: the last line"));
    }

    /// ARMED carries no text at all, and that is not the same as no page.
    #[test]
    fn an_armed_page_with_nothing_in_it_is_still_a_state() {
        let mut page = blank();
        assert_eq!(seal(&mut page, State::Armed, b""), 0);
        assert_eq!(recover(&page), Some((State::Armed, &b""[..])));
    }

    /// The loader writes the address with `{:#x}` and the kernel reads it back
    /// here; everything else is refused rather than guessed at.
    #[test]
    fn the_parameter_round_trips_the_address_it_names() {
        assert_eq!(address_of(&format!("{PARAM}{PHYS:#x}")), Some(PHYS));
        assert_eq!(address_of(&format!("{PARAM}{:#x}", u64::MAX)), Some(u64::MAX));
        assert_eq!(address_of("blackbox=0x0"), Some(0));
        for wrong in
            ["blackbox=", "blackbox=0x", "blackbox=8000000", "blackbox=0X8000000", "watchdog"]
        {
            assert_eq!(address_of(wrong), None, "{wrong:?} was read as an address");
        }
        // Wider than an address, so the shift cannot silently drop the head.
        assert_eq!(address_of("blackbox=0x10000000000000000"), None);
    }

    /// Three states, three words, none of them each other's.
    #[test]
    fn every_state_has_a_word_of_its_own() {
        for (i, state) in STATES.iter().enumerate() {
            for other in &STATES[i + 1..] {
                assert_ne!(state.named(), other.named());
                assert_ne!(state.code(), other.code());
            }
            assert_eq!(State::of(state.code()), Some(*state));
        }
        assert_eq!(State::of(0), None);
        assert_eq!(State::of(4), None);
    }
}
