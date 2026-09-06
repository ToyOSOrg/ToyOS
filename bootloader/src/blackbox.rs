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
use toyos_wallclock::Civil;

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
/// **Runs before the log is open**, because whether this pass appends to the
/// last one's file or replaces it is what the page decides — so a refusal is
/// returned as a line for the caller to write rather than printed here, where a
/// machine with no console would lose it.
pub fn claim(system_table: &SystemTable<Boot>) -> (Option<Page>, Option<String>) {
    match system_table.boot_services().allocate_pages(
        AllocateType::Address(PHYS),
        MemoryType::LOADER_DATA,
        1,
    ) {
        Ok(at) => {
            // `AllocateType::Address` allocates that address or fails; firmware
            // answering with another one has not done what was asked of it.
            assert_eq!(at, PHYS, "AllocatePages(AllocateAddress) answered {at:#x} for {PHYS:#x}");
            (Some(Page(at)), None)
        }
        Err(e) => (
            None,
            Some(alloc::format!(
                "{HEAD} firmware would not give {PHYS:#x} ({e}), so this boot leaves nothing \
                 behind and the next one has nothing to read"
            )),
        ),
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
    let at = page?;
    let page = bytes(at);
    let (state, stamp, text) = toyos_blackbox::recover(page)?;
    let mut lines = Vec::new();
    lines.push(when(stamp));
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
        // The one finding an absence makes: the loader armed it, and nothing in
        // that kernel — not even its exception entry — reached the page.
        State::Armed => lines.push(alloc::format!(
            "{PREVIOUS_PANIC} the page still reads {}, so that kernel died without reaching \
             either its panic path or its shutdown",
            state.named()
        )),
        // Registers and no report: an exception entry sealed what it could and
        // whatever it was going to say, it never said.
        State::Fault => match toyos_blackbox::Fault::from_bytes(text) {
            Some(fault) => lines.extend(fault_lines(&fault)),
            None => lines.push(alloc::format!(
                "{PREVIOUS_PANIC} the page reads {} and its {} bytes are not a record, so that \
                 boot's registers are not readable",
                state.named(),
                text.len()
            )),
        },
    }
    // Cleared once it has been read, so the boot after this one does not report
    // a death two boots old as its predecessor's — **and written back, and then
    // read again to see that it was**. A clear that stays in this CPU's cache is
    // a clear a reset discards, and the boot after it reports the same crash a
    // second time off a stick that was freshly flashed; that is what run 13 did.
    toyos_blackbox::clear(page);
    flush(at);
    if toyos_blackbox::recover(page).is_some() {
        lines.push(alloc::format!(
            "{HEAD} {PHYS:#x} still reads as a record after being cleared and written back, so \
             the boot after this one will report the crash above a second time"
        ));
    }
    Some(Finding { lines, ends_the_chain: true })
}

/// When the boot this record came from was armed, as the loader stamped it.
///
/// **The first line of every report**, because a record the next boot finds is
/// either this boot's predecessor's or a stale one nothing cleared, and until
/// run 13 there was no way to tell those apart from the stick.
fn when(stamp: u64) -> String {
    if stamp == 0 {
        return alloc::format!("{HEAD} the record below carries no date, so this machine's \
                               firmware would not say what time that boot was armed");
    }
    alloc::format!("{HEAD} the record below is from the boot armed at {}", Civil::from_unix_secs(stamp).stem())
}

/// Write the page back out of this CPU's caches, and every other CPU's.
///
/// The kernel's `blackbox::flush` is the same loop for the same reason; the
/// instruction is each binary's because `toyos-blackbox` forbids unsafe code,
/// and `toyos_blackbox::CACHE_LINE` is the one decision they share.
fn flush(page: Page) {
    let mut line = 0usize;
    while line < BYTES {
        // SAFETY: `CLFLUSH` writes back and invalidates the line containing the
        // address and touches nothing else; the address is inside the page this
        // image allocated, and the instruction faults on nothing a canonical
        // address can be.
        unsafe {
            core::arch::asm!(
                "clflush [{addr}]",
                addr = in(reg) (page.0 + line as u64) as *const u8,
                options(nostack, preserves_flags),
            );
        }
        line += toyos_blackbox::CACHE_LINE;
    }
    // SAFETY: `SFENCE` orders those writebacks ahead of whatever ends this
    // machine; it touches no memory or register.
    unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
}

/// A sealed [`toyos_blackbox::Fault`] as lines for the log.
///
/// **Decoded here and nowhere on the machine that wrote it**: the entry that
/// sealed these words was one fault away from a triple fault and could not
/// format a digit, and this pass has firmware, a filesystem and all the time
/// there is.
fn fault_lines(fault: &toyos_blackbox::Fault) -> Vec<String> {
    let cpu = if fault.cpu == toyos_blackbox::Fault::NO_CPU {
        String::from("cpu unknown")
    } else {
        alloc::format!("cpu{}", fault.cpu)
    };
    let mut lines = alloc::vec![
        alloc::format!(
            "{PREVIOUS_PANIC} its exception entry sealed registers and never got to a report"
        ),
        alloc::format!(
            "| vector {} err={:#x} on {cpu}",
            fault.vector, fault.error_code
        ),
        alloc::format!(
            "| rip={:#018x} rsp={:#018x} rflags={:#x}",
            fault.rip, fault.rsp, fault.rflags
        ),
        alloc::format!("| cr2={:#018x} cr3={:#018x}", fault.cr2, fault.cr3),
    ];
    // Three to a line, so a photograph of the panel carries them all.
    for row in toyos_blackbox::REGISTERS.chunks(3).enumerate() {
        let (r, names) = row;
        let mut line = String::from("|");
        for (c, name) in names.iter().enumerate() {
            let value = fault.registers.get(r * 3 + c).copied().unwrap_or(0);
            line.push_str(&alloc::format!(" {name}={value:#018x}"));
        }
        lines.push(line);
    }
    lines
}

/// Seal `ARMED` into the page, which is what makes the next boot's silence a
/// finding, stamped with the time this pass armed it.
pub fn arm(page: Option<Page>, stamp: u64) {
    let Some(page) = page else { return };
    toyos_blackbox::seal(bytes(page), State::Armed, stamp, &[]);
    // Written back before the handoff: everything after this point either ends
    // in a reset or hands the machine to a kernel, and neither writes this line
    // out for us.
    flush(page);
    println!(
        "{HEAD} {PHYS:#x} armed at {}, and the kernel is told so on its parameter line",
        Civil::from_unix_secs(stamp).stem()
    );
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
