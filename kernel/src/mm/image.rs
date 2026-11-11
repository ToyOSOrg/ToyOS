//! The kernel's own image where the loader placed it, and what each of its
//! pages may be used for: the build's ranges for it (`toyos_kimage`), page by
//! page, and its `PT_GNU_RELRO` pages read-only, since the loader applied every
//! relocation before the kernel ran.

use toyos_abi::syscall::VmProt;
use toyos_kimage::Kernel;

use super::policy::Prot;
use super::{Region, PAGE_SIZE, PHYS_OFFSET};

/// The loader's allocation for the kernel: the image at its own addresses,
/// a page the direct map does not hold, then the boot stack above it.
pub struct Image<'a> {
    memory: Region,
    kernel: Kernel<'a>,
    /// The page under the boot stack, physical.
    guard: u64,
    /// The one executable range's pages, physical.
    text: Region,
    /// `PT_GNU_RELRO`'s whole pages, physical.
    relro: Region,
}

impl<'a> Image<'a> {
    /// The image `kernel` describes, loaded into `memory`, with the boot
    /// stack's lowest byte `stack` bytes into it.
    ///
    /// Panics where it has no RELRO of whole pages opening a writable range,
    /// so a link that loses it, or moves it into the middle of one, stops the
    /// boot, and where the loader left no page between the image and the
    /// stack. `Kernel::parse` already refused ranges that are not one
    /// executable and disjoint whole pages.
    pub fn new(kernel: &Kernel<'a>, memory: Region, stack: u64) -> Self {
        assert!(
            stack.is_multiple_of(PAGE_SIZE) && stack >= kernel.memory + PAGE_SIZE,
            "kernel image: the stack at +{stack:#x} leaves no page above the image's {:#x} bytes",
            kernel.memory,
        );
        let guard = memory.start + stack - PAGE_SIZE;
        let text = kernel.text().unwrap_or_else(|why| panic!("{why}"));
        let text = Region { start: memory.start + text.lo, end: memory.start + text.hi };
        assert!(memory.start <= text.start && text.end <= memory.end, "kernel image: its text is outside its memory");
        let (lo, hi) = kernel.relro;
        assert!(lo < hi, "kernel image: no PT_GNU_RELRO, so nothing holds its relocated pointers read-only");
        assert!(
            kernel.ranges().flatten().any(|range| range.prot == VmProt::ReadWrite && range.lo == lo),
            "kernel image: PT_GNU_RELRO at {lo:#x} opens no writable range",
        );
        let relro = Region { start: memory.start + lo, end: memory.start + hi };
        Self { memory, kernel: *kernel, guard, text, relro }
    }

    /// The page under the boot stack, as the direct map addresses it.
    pub fn stack_guard(&self) -> core::ops::Range<u64> {
        PHYS_OFFSET + self.guard..PHYS_OFFSET + self.guard + PAGE_SIZE
    }

    /// Whether any byte of `[phys, phys + len)` is the image's.
    fn overlaps(&self, phys: u64, len: u64) -> bool {
        phys < self.memory.end && self.memory.start < phys + len
    }

    /// What the 4 KiB page at `phys` may be used for, or `None` for the boot
    /// stack's guard, which is not mapped: its range's protection, and a page
    /// no range holds — the boot stack, or one not the image's — is data.
    pub fn prot(&self, phys: u64) -> Option<Prot> {
        if phys == self.guard {
            return None;
        }
        if !self.overlaps(phys, PAGE_SIZE) {
            return Some(Prot::ReadWrite);
        }
        let offset = phys - self.memory.start;
        let holder = self.kernel.ranges().flatten().find(|range| (range.lo..range.hi).contains(&offset));
        Some(match holder.map(|range| range.prot) {
            Some(VmProt::ReadExec) => Prot::ReadExec,
            Some(VmProt::Read) => Prot::Read,
            Some(_) if (self.relro.start..self.relro.end).contains(&phys) => Prot::Read,
            _ => Prot::ReadWrite,
        })
    }

    /// What all of the 2 MiB page at `phys` may be used for, where it is one
    /// thing: a page that holds two protections, or the guard, is `None`, to
    /// be mapped page by page.
    pub fn block(&self, phys: u64) -> Option<Prot> {
        let first = self.prot(phys)?;
        (1..super::PAGE_2M / PAGE_SIZE).all(|i| self.prot(phys + i * PAGE_SIZE) == Some(first)).then_some(first)
    }

    /// The text's pages, as the direct map addresses them.
    pub fn text(&self) -> core::ops::Range<u64> {
        PHYS_OFFSET + self.text.start..PHYS_OFFSET + self.text.end
    }

    /// RELRO's whole pages, as the direct map addresses them.
    pub fn relro(&self) -> core::ops::Range<u64> {
        PHYS_OFFSET + self.relro.start..PHYS_OFFSET + self.relro.end
    }
}
