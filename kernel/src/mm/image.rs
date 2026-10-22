//! The kernel's own image where the loader placed it, and what each of its
//! pages may be used for: its `PT_LOAD` segments' flags, page by page.

use toyos_elf::{Layout, Segment};

use super::policy::Prot;
use super::{Region, PAGE_SIZE, PHYS_OFFSET};

/// The loader's allocation for the kernel: the image at its own link
/// addresses, then the boot stack.
pub struct Image<'a> {
    memory: Region,
    /// Where image offset 0 is.
    base: u64,
    segments: &'a [Segment],
    /// The one executable segment's pages, physical.
    text: Region,
}

impl<'a> Image<'a> {
    /// The image `layout` describes, loaded into `memory`.
    ///
    /// Panics where a page could not have a single protection that is not
    /// both writable and executable: an executable segment that is writable,
    /// two segments that share a page, or other than one executable segment,
    /// which is what the loader's boot map maps read-only.
    pub fn new(layout: &'a Layout, memory: Region) -> Self {
        let segments = layout.segments();
        if let Some((a, b)) = layout.overlapping_load_pages(PAGE_SIZE) {
            panic!("kernel image: PT_LOAD segments {a} and {b} share a page");
        }
        let mut executable = segments.iter().filter(|segment| segment.flags().executable());
        let (Some(text), None) = (executable.next(), executable.next()) else {
            panic!("kernel image: not exactly one executable PT_LOAD segment");
        };
        assert!(!text.writable(), "kernel image: its executable segment is writable");
        let base = memory.start + layout.extent().min();
        let (start, end) = text.page_range(PAGE_SIZE);
        let text = Region { start: base + start, end: base + end };
        assert!(memory.start <= text.start && text.end <= memory.end, "kernel image: its text is outside its memory");
        Self { memory, base, segments, text }
    }

    /// Whether any byte of `[phys, phys + len)` is the image's.
    fn overlaps(&self, phys: u64, len: u64) -> bool {
        phys < self.memory.end && self.memory.start < phys + len
    }

    /// What the 4 KiB page at `phys` may be used for, or `None` for a page
    /// that is not the image's: its segment's flags, and a page no segment
    /// holds — the boot stack — is data.
    pub fn prot(&self, phys: u64) -> Option<Prot> {
        if !self.overlaps(phys, PAGE_SIZE) {
            return None;
        }
        let holder = phys.checked_sub(self.base).and_then(|offset| {
            self.segments.iter().find(|segment| {
                let (start, end) = segment.page_range(PAGE_SIZE);
                (start..end).contains(&offset)
            })
        });
        Some(match holder {
            Some(segment) if segment.flags().executable() => Prot::ReadExec,
            Some(segment) if !segment.writable() => Prot::Read,
            _ => Prot::ReadWrite,
        })
    }

    /// What all of the 2 MiB page at `phys` may be used for, where it is one
    /// thing: a page not the image's is data, and a page that holds two
    /// protections is `None`, to be mapped page by page.
    pub fn block(&self, phys: u64) -> Option<Prot> {
        let of = |page: u64| self.prot(page).unwrap_or(Prot::ReadWrite);
        let first = of(phys);
        (1..super::PAGE_2M / PAGE_SIZE).all(|i| of(phys + i * PAGE_SIZE) == first).then_some(first)
    }

    /// The text's pages, as the direct map addresses them.
    pub fn text(&self) -> core::ops::Range<u64> {
        PHYS_OFFSET + self.text.start..PHYS_OFFSET + self.text.end
    }
}
