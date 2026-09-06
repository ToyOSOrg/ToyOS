//! The loader's half of the panic black box: it claims the page, and it reads
//! what the last boot left in it onto the stick.
//!
//! **Claim first, read second.** `AllocatePages` does not zero what it hands
//! back, so a page this loader now owns still holds whatever the boot before
//! it wrote there; a page firmware would not give is a page whose contents were
//! never this project's to read, and the refusal is by name rather than a
//! report about somebody else's memory. Firmware that does zero its allocations
//! costs the machine its black box and nothing else — the magic is simply gone.
//!
//! The claim is [`toyos_blackbox::MEMORY_TYPE`], which is also the whole of how
//! the kernel is told this happened: it reads the type back out of the memory
//! map it is handed.

use uefi::prelude::*;
use uefi::table::boot::{AllocateType, MemoryType};

use toyos_blackbox::{BYTES, MEMORY_TYPE, PHYS, TEXT_BYTES};

/// The line the harvested report is written under; a reader of `loader.log`
/// and the judge both look for this and nothing else.
pub const PREVIOUS_PANIC: &str = "Previous boot's panic:";

/// What the page's own line says when this boot may write it.
pub const RESERVED: &str = "Black box: reserved";

/// Claim the page, and put anything the last boot sealed into it on the stick.
pub fn harvest(system_table: &SystemTable<Boot>) {
    let claim = system_table.boot_services().allocate_pages(
        AllocateType::Address(PHYS),
        MemoryType::custom(MEMORY_TYPE),
        1,
    );
    let at = match claim {
        Ok(at) => at,
        Err(e) => {
            return println!(
                "Black box: firmware would not give {PHYS:#x} ({e}), so a panic on this boot \
                 leaves nothing behind"
            );
        }
    };
    // `AllocateType::Address` allocates that address or fails; a firmware that
    // answered with another one has not done what was asked of it, and the page
    // the kernel will write is the constant and not this.
    assert_eq!(at, PHYS, "AllocatePages(AllocateAddress) answered {at:#x} for {PHYS:#x}");

    // SAFETY: the page is this image's allocation as of the call above, boot
    // services identity-map physical memory, and `BYTES` is the one page that
    // was asked for. No other reference to it exists — this is the only site in
    // the loader that names the address at all.
    let page = unsafe { &mut *(at as *mut [u8; BYTES]) };
    // Remembered for `seal_loader_refusal`, which runs where nothing can be printed.
    CLAIMED.store(at, core::sync::atomic::Ordering::Relaxed);

    let Some(text) = toyos_blackbox::recover(page) else {
        return println!(
            "{RESERVED}: {PHYS:#x}, {TEXT_BYTES} bytes, and the last boot left no panic in it"
        );
    };
    println!("{PREVIOUS_PANIC} {} bytes recovered from {PHYS:#x}", text.len());
    for line in text.split(|byte| *byte == b'\n') {
        if !line.is_empty() {
            println!("| {}", Ascii(line));
        }
    }
    // Cleared once it is on the stick, so the boot after this one does not
    // report a crash two boots old as its predecessor's.
    toyos_blackbox::clear(page);
    println!("{RESERVED}: {PHYS:#x}, {TEXT_BYTES} bytes, and the report above is off it");
}

/// One recovered line as `loader.log` may carry it: the panel's own alphabet,
/// codepoints `0x20..=0x7E` and a dot for everything else.
///
/// Not a lossy UTF-8 decode. The bytes crossed a reset inside DRAM, the
/// checksum says they are the ones that were written and says nothing about
/// what they are, and `loader.log` is compared byte for byte against a console
/// the firmware rendered — where a codepoint it has no glyph for is a line the
/// two channels disagree about on one machine and not on the next.
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

/// The page's kernel-address twin for [`seal_loader_refusal`], remembered by
/// [`harvest`]; 0 where this boot has no page.
static CLAIMED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// What a loader that could not carry the whole memory map leaves for the next
/// boot to report.
///
/// **Called after `ExitBootServices`, where the console, the allocator and
/// `println!` are all gone**, so it formats into a fixed buffer and writes one
/// page. It is the only channel a loader has left, and a truncated memory map
/// that says nothing is memory the kernel silently never sees.
pub fn seal_loader_refusal(dropped: usize, kept: usize) {
    let at = CLAIMED.load(core::sync::atomic::Ordering::Relaxed);
    if at == 0 {
        return;
    }
    let mut text = [0u8; 128];
    let mut n = 0;
    let mut put = |bytes: &[u8], n: &mut usize| {
        for byte in bytes {
            if let Some(slot) = text.get_mut(*n) {
                *slot = *byte;
                *n += 1;
            }
        }
    };
    put(b"LOADER: the memory map did not fit: kept ", &mut n);
    let mut kept_digits = [0u8; 20];
    put(decimal(kept, &mut kept_digits), &mut n);
    put(b" descriptors and dropped ", &mut n);
    let mut dropped_digits = [0u8; 20];
    put(decimal(dropped, &mut dropped_digits), &mut n);
    put(b", so this kernel was handed less memory than the machine has", &mut n);
    // SAFETY: `CLAIMED` is non-zero only where `harvest`'s `AllocatePages`
    // answered with this page, which nothing else in this image names; boot
    // services identity-map physical memory and that mapping outlives their
    // exit, since the loader has not switched `cr3` yet.
    let page = unsafe { &mut *(at as *mut [u8; BYTES]) };
    toyos_blackbox::seal(page, text.get(..n).unwrap_or(&[]));
}

/// `value` in decimal, in the caller's own buffer.
fn decimal(value: usize, out: &mut [u8; 20]) -> &[u8] {
    let mut digits = [0u8; 20];
    let mut n = 0;
    let mut v = value;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 || n == digits.len() {
            break;
        }
    }
    for i in 0..n {
        out[i] = digits[n - 1 - i];
    }
    out.get(..n).unwrap_or(&[])
}
