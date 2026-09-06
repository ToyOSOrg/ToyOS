//! The loader's half of the boot chain: it claims the page, reads what the last
//! boot sealed into it, and arms the next one.
//!
//! **Claim first, read second.** `AllocatePages` does not zero what it hands
//! back, so a page this loader now owns still holds whatever the boot before it
//! wrote there; a page firmware would not give is a page whose contents were
//! never this project's to read, and the refusal is by name rather than a report
//! about somebody else's memory. Firmware that does zero its allocations costs
//! the machine its black box and nothing else — the magic is simply gone.
//!
//! The type is `LoaderData` and nothing in the memory map says the page is ours:
//! the kernel is told the address on its own parameter line instead. A type of
//! this project's own out of the range UEFI 2.10 §7.2 reserves for OS loaders
//! was an exact channel that cost nothing, until the owner's firmware stopped
//! returning from `ExitBootServices` with one of those descriptors in the map.

use alloc::string::String;
use alloc::vec::Vec;
use uefi::prelude::*;
use uefi::table::boot::{AllocateType, MemoryType};

use toyos_blackbox::{BYTES, PHYS, State};

/// The line a harvested report is written under; a reader of the stick and the
/// judge both look for this and nothing else.
pub const PREVIOUS_PANIC: &str = "Previous boot's panic:";

/// The head of every line this module writes about the page itself.
pub const HEAD: &str = "Black box:";

/// The page, once claimed. `None` is a boot with no black box at all, which is
/// a machine and not a failure: it boots the kernel and leaves nothing behind.
#[derive(Clone, Copy)]
pub struct Page(u64);

/// Claim the page for this boot, or say by name why this machine has none.
pub fn claim(system_table: &SystemTable<Boot>) -> Option<Page> {
    match system_table.boot_services().allocate_pages(
        AllocateType::Address(PHYS),
        MemoryType::LOADER_DATA,
        1,
    ) {
        Ok(at) => {
            // `AllocateType::Address` allocates that address or fails; firmware
            // answering with another one has not done what was asked of it.
            assert_eq!(at, PHYS, "AllocatePages(AllocateAddress) answered {at:#x} for {PHYS:#x}");
            Some(Page(at))
        }
        Err(e) => {
            println!(
                "{HEAD} firmware would not give {PHYS:#x} ({e}), so this boot leaves nothing \
                 behind and the next one has nothing to read"
            );
            None
        }
    }
}

/// What the last boot left, as lines for this pass's log, and whether this pass
/// should hand the machine back to the firmware instead of booting a kernel.
pub struct Finding {
    pub lines: Vec<String>,
    /// True where the chain ends here: the last boot has been accounted for, so
    /// booting the kernel again would start the same loop over.
    pub ends_the_chain: bool,
}

/// Read the page, clear it, and say what it held.
///
/// `None` is a page carrying nothing this tree wrote — a cold power cycle, a
/// first boot, or DRAM a reset did not preserve, which are one answer — and the
/// caller boots the kernel normally.
pub fn harvest(page: Option<Page>) -> Option<Finding> {
    let page = bytes(page?);
    let (state, text) = toyos_blackbox::recover(page)?;
    let mut lines = Vec::new();
    match state {
        State::Panic => {
            lines.push(alloc::format!("{PREVIOUS_PANIC} {} bytes off {PHYS:#x}", text.len()));
            for line in text.split(|byte| *byte == b'\n') {
                if !line.is_empty() {
                    lines.push(alloc::format!("| {}", Ascii(line)));
                }
            }
        }
        State::Done => lines.push(alloc::format!(
            "{HEAD} the last boot read {}, so it handed the machine back on purpose and this \
             chain ends here",
            state.named()
        )),
        // The one finding an absence makes: the loader armed it, and neither of
        // the kernel's two writers reached the page.
        State::Armed => lines.push(alloc::format!(
            "{PREVIOUS_PANIC} the page still reads {}, so that kernel died without reaching \
             either its panic path or its shutdown",
            state.named()
        )),
    }
    // Cleared once it has been read, so the boot after this one does not report
    // a death two boots old as its predecessor's.
    toyos_blackbox::clear(page);
    Some(Finding { lines, ends_the_chain: true })
}

/// Seal `ARMED` into the page, which is what makes the next boot's silence a finding.
pub fn arm(page: Option<Page>) {
    let Some(page) = page else { return };
    toyos_blackbox::seal(bytes(page), State::Armed, &[]);
    println!("{HEAD} {PHYS:#x} armed, and the kernel is told so on its parameter line");
}

/// The parameter word naming the page, to be appended to what the ESP carried.
pub fn param(page: Option<Page>) -> Option<String> {
    page.map(|Page(at)| alloc::format!("{}{at:#x}", toyos_blackbox::PARAM))
}

fn bytes(page: Page) -> &'static mut [u8; BYTES] {
    // SAFETY: `Page` is minted only by `claim`, from an `AllocatePages` that
    // answered with this address, and boot services identity-map physical
    // memory. Nothing else in this image names the address, and the one page
    // asked for is exactly `BYTES`.
    unsafe { &mut *(page.0 as *mut [u8; BYTES]) }
}

/// One recovered line as a log file may carry it: the panel's own alphabet,
/// codepoints `0x20..=0x7E` and a dot for everything else.
///
/// Not a lossy UTF-8 decode. The bytes crossed a reset inside DRAM, the checksum
/// says they are the ones that were written and says nothing about what they
/// are, and this file is compared byte for byte against a console the firmware
/// rendered — where a codepoint it has no glyph for is a line the two channels
/// disagree about on one machine and not on the next.
struct Ascii<'a>(&'a [u8]);

impl core::fmt::Display for Ascii<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for byte in self.0 {
            let ch = if (0x20..=0x7E).contains(byte) { *byte as char } else { '.' };
            core::fmt::Write::write_char(f, ch)?;
        }
        Ok(())
    }
}
