//! What an image wants mapped, derived from program headers alone.
//!
//! [`Layout`] is the crate's central value and the only thing downstream of it
//! is effects. Its invariants are established once, in [`Layout::parse`], so
//! that no consumer re-checks them and none of them can be forgotten at a call
//! site.
//!
//! Every address it hands out is an [`ImageOffset`] or an [`ImageRange`]:
//! a position inside the image's own [`Extent`], which a loader adds to the
//! address it placed the image at. A raw `p_vaddr` never leaves this module.

use crate::header::{
    FileHeader, Machine, ProgramHeader, PT_DYNAMIC, PT_GNU_EH_FRAME, PT_LOAD, PT_TLS,
    SECTION_HEADER_SIZE,
};
use crate::{Error, MAX_LOAD_SEGMENTS, MAX_TLS_ALIGN};

/// An image's loadable extent, `[min, max]` with `min <= max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    min: u64,
    max: u64,
}

impl Extent {
    /// `None` for `min > max`, which no image has. Crate-private: an extent is
    /// the hull [`Layout::parse`] derives, never a bound a caller hands in.
    pub(crate) const fn new(min: u64, max: u64) -> Option<Extent> {
        if min > max {
            return None;
        }
        Some(Extent { min, max })
    }

    pub const fn min(self) -> u64 {
        self.min
    }

    pub const fn max(self) -> u64 {
        self.max
    }

    /// Bytes between the lowest and highest address the image claims.
    pub const fn span(self) -> u64 {
        self.max.abs_diff(self.min)
    }

    /// Where `vaddr` lies in the image, when it lies in it at all.
    ///
    /// The end is inclusive: one past the last byte is an address a pointer may
    /// name — `_end`, `&array[N]` — and a linker writes it as a `RELATIVE`
    /// addend or a symbol value like any other.
    pub const fn offset(self, vaddr: u64) -> Option<ImageOffset> {
        match vaddr.checked_sub(self.min) {
            Some(off) if vaddr <= self.max => Some(ImageOffset(off)),
            _ => None,
        }
    }

    /// `[vaddr, vaddr + len)`, when every byte of it lies in the image.
    pub const fn range(self, vaddr: u64, len: u64) -> Option<ImageRange> {
        let Some(start) = vaddr.checked_sub(self.min) else { return None };
        match vaddr.checked_add(len) {
            Some(end) if end <= self.max => Some(ImageRange { start, len }),
            _ => None,
        }
    }
}

/// A position inside one image, `0..=span` from its lowest address: made only
/// by [`Extent::offset`] and [`ImageRange`].
///
/// A loader turns it into an address by adding the address it placed the
/// image at, and an image placed where its whole span fits makes that sum fit
/// too — so no consumer re-checks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImageOffset(u64);

impl ImageOffset {
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// `[start, start + len)` inside one image, `start + len <= span`: made only
/// by [`Extent::range`] and the parse that builds a [`Layout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageRange {
    start: u64,
    len: u64,
}

impl ImageRange {
    pub const fn start(self) -> ImageOffset {
        ImageOffset(self.start)
    }

    pub const fn len(self) -> u64 {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// One past the last byte, also inside the image.
    pub const fn end(self) -> ImageOffset {
        ImageOffset(self.start.wrapping_add(self.len))
    }
}

/// `PF_X`, `PF_W`, `PF_R` as the file declared them.
///
/// Kept whole rather than reduced to `writable` at parse time: protection is a
/// three-way property and a loader that only records one bit cannot express
/// W^X.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentFlags(pub u32);

impl SegmentFlags {
    pub const fn executable(self) -> bool {
        self.0 & 1 != 0
    }
    pub const fn writable(self) -> bool {
        self.0 & 2 != 0
    }
    pub const fn readable(self) -> bool {
        self.0 & 4 != 0
    }
}

/// One `PT_LOAD` segment: [`Layout::parse`] is the only constructor.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    image: ImageRange,
    filesz: u64,
    file_offset: u64,
    flags: SegmentFlags,
}

impl Segment {
    /// `[p_vaddr, p_vaddr + p_memsz)`, inside the image.
    pub const fn image(&self) -> ImageRange {
        self.image
    }

    /// `p_filesz`, never above `p_memsz`.
    pub const fn filesz(&self) -> u64 {
        self.filesz
    }

    /// `p_offset`; `p_offset + p_filesz` fits a `u64`.
    pub const fn file_offset(&self) -> u64 {
        self.file_offset
    }

    pub const fn flags(&self) -> SegmentFlags {
        self.flags
    }

    pub const fn writable(&self) -> bool {
        self.flags.writable()
    }

    /// The segment rounded out to whole pages, in image offsets.
    ///
    /// Image-relative because the loader rebases the image: with a page-aligned
    /// placement, rounding an image offset and rounding the rebased address
    /// give the same answer, and only the relative form is free of the
    /// placement's own arithmetic. A round-up that would leave the last page is
    /// `u64::MAX` instead: the only consumer is an overlap test, and a range
    /// that is too long can report an overlap that is not there but never miss
    /// one that is.
    pub fn page_range(&self, page_size: u64) -> (u64, u64) {
        debug_assert!(page_size.is_power_of_two());
        let mask = page_size.wrapping_sub(1);
        let end = self.image.end().get();
        let end_page = match end.checked_add(mask) {
            Some(rounded) => rounded & !mask,
            None => u64::MAX,
        };
        (self.image.start & !mask, end_page)
    }
}

/// A `PT_TLS` segment.
///
/// `align` is zero or a power of two no larger than [`MAX_TLS_ALIGN`], and the
/// file-backed template lies inside the image. Absent TLS is `None`, never a
/// zero `memsz`: a module with a `PT_TLS` of zero size still gets a DTV slot,
/// and the two cases are not the same question.
#[derive(Clone, Copy, Debug)]
pub struct TlsSegment {
    template: ImageRange,
    memsz: u64,
    align: u64,
}

impl TlsSegment {
    /// The `.tdata` bytes, `p_filesz` of them at `p_vaddr`.
    pub const fn template(&self) -> ImageRange {
        self.template
    }

    /// `.tdata` plus `.tbss`, never below the template's length. `.tbss`
    /// occupies address space no `PT_LOAD` need cover, so this is not bounded
    /// by the image.
    pub const fn memsz(&self) -> u64 {
        self.memsz
    }

    pub const fn align(&self) -> u64 {
        self.align
    }
}

/// `PT_DYNAMIC`, where the file holds it and where the image does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DynamicSegment {
    pub(crate) file_offset: u64,
    pub(crate) image: ImageRange,
}

impl DynamicSegment {
    /// `p_offset`: where the file holds `PT_DYNAMIC`.
    pub const fn file_offset(&self) -> u64 {
        self.file_offset
    }

    /// Where the image holds it, inside the extent.
    pub const fn image(&self) -> ImageRange {
        self.image
    }
}

/// Where the section header table is, when the file has a usable one.
///
/// Section headers are optional metadata — symbol names for backtraces, and
/// the `.rela.dyn` fallback for a file with no `PT_DYNAMIC`. A table the
/// loader cannot index is dropped rather than refused, because refusing would
/// turn "no symbol names" into "this program does not run".
#[derive(Clone, Copy, Debug)]
pub struct SectionTableRef {
    pub file_offset: u64,
    pub count: u16,
    pub entry_size: u16,
}

impl SectionTableRef {
    /// Bytes the whole table occupies. Cannot overflow: both factors are
    /// `u16`.
    pub const fn byte_len(&self) -> usize {
        self.count as usize * self.entry_size as usize
    }
}

/// Everything the program headers say, validated.
///
/// # Invariants
///
/// [`Layout::parse`] is the only constructor and refuses anything that breaks
/// the following, so every `Layout` that exists already satisfies them:
///
/// - one to [`MAX_LOAD_SEGMENTS`] `PT_LOAD` segments, each with
///   `filesz <= memsz` and neither `vaddr + memsz` nor `file_offset + filesz`
///   overflowing;
/// - the extent runs from the smallest `p_vaddr` to the largest
///   `p_vaddr + p_memsz` over those segments, so it covers every one of them;
/// - the entry point, the file-backed extent of `PT_TLS`, and all of
///   `PT_DYNAMIC` and `PT_GNU_EH_FRAME`, lie inside the extent;
/// - the TLS alignment is zero or a power of two no larger than
///   [`MAX_TLS_ALIGN`], so that `!(align - 1)` is a mask and the TLS block's
///   size cannot be dominated by a number the file chose.
///
/// Downstream every size pair is a (copy length, destination size) pair — an
/// allocation of `memsz` then a copy of `filesz` — so `filesz <= memsz` is a
/// memory-safety invariant here and not an ELF formality.
#[derive(Clone, Debug)]
pub struct Layout {
    extent: Extent,
    entry: ImageOffset,
    segments: [Segment; MAX_LOAD_SEGMENTS],
    segment_count: usize,
    tls: Option<TlsSegment>,
    dynamic: Option<DynamicSegment>,
    section_headers: Option<SectionTableRef>,
    eh_frame_hdr: Option<ImageRange>,
}

impl Layout {
    /// Parse program headers out of the first bytes of a file.
    ///
    /// `data` need only reach the end of the program header table; the loader
    /// hands it 4 KiB and never reads a segment's contents to get here.
    /// `machine` is the one the caller runs: an image for any other is refused.
    pub fn parse(data: &[u8], machine: Machine) -> Result<Layout, Error> {
        let ehdr = FileHeader::parse(data)?;
        if ehdr.machine != machine {
            return Err(Error::WrongMachine);
        }
        let phdrs = ehdr.program_headers(data)?;

        let mut segment_count = 0usize;
        let mut vaddr_min = u64::MAX;
        let mut vaddr_max = 0u64;
        let mut tls = None;
        let mut dynamic = None;
        let mut eh_frame_hdr = None;

        // First pass: the extent, the singleton headers, and every `PT_LOAD`
        // field validated — so the second pass over the same headers only places
        // the segments the extent it derives here holds.
        for i in 0..usize::from(ehdr.phnum) {
            let Some(phdr) = ProgramHeader::parse(phdrs, i) else {
                return Err(Error::ProgramHeadersOutsideBuffer);
            };
            if matches!(phdr.kind, PT_LOAD | PT_TLS) && phdr.filesz > phdr.memsz {
                return Err(Error::FileszAboveMemsz);
            }
            match phdr.kind {
                PT_LOAD => {
                    let seg_end = phdr
                        .vaddr
                        .checked_add(phdr.memsz)
                        .ok_or(Error::SegmentExtentOverflows)?;
                    if phdr.offset.checked_add(phdr.filesz).is_none() {
                        return Err(Error::FileExtentOverflows);
                    }
                    if segment_count >= MAX_LOAD_SEGMENTS {
                        return Err(Error::TooManyLoadSegments);
                    }
                    segment_count = segment_count.wrapping_add(1);
                    vaddr_min = vaddr_min.min(phdr.vaddr);
                    vaddr_max = vaddr_max.max(seg_end);
                }
                // Last one wins, as it does for every other singleton header:
                // a file with two of these is malformed and the loader has no
                // better answer than a consistent one.
                PT_TLS => tls = Some(phdr),
                PT_DYNAMIC => dynamic = Some(phdr),
                PT_GNU_EH_FRAME => eh_frame_hdr = Some(phdr),
                _ => {}
            }
        }

        let extent = Extent::new(vaddr_min, vaddr_max).ok_or(Error::NoLoadSegments)?;

        let blank = Segment {
            image: ImageRange { start: 0, len: 0 },
            filesz: 0,
            file_offset: 0,
            flags: SegmentFlags(0),
        };
        let mut segments = [blank; MAX_LOAD_SEGMENTS];
        let mut placed = 0usize;
        for i in 0..usize::from(ehdr.phnum) {
            let phdr = ProgramHeader::parse(phdrs, i).ok_or(Error::ProgramHeadersOutsideBuffer)?;
            if phdr.kind != PT_LOAD {
                continue;
            }
            // Inside by construction: the extent is the hull of these.
            let image = extent.range(phdr.vaddr, phdr.memsz).ok_or(Error::SegmentExtentOverflows)?;
            segments[placed] = Segment {
                image,
                filesz: phdr.filesz,
                file_offset: phdr.offset,
                flags: SegmentFlags(phdr.flags),
            };
            placed = placed.wrapping_add(1);
        }

        // Every other program header names a vaddr the loader turns into an
        // offset into the image. Outside the extent that is a wrapping
        // subtraction into an out-of-bounds pointer, so bound them here rather
        // than at each use site. Only the file-backed part of `PT_TLS` is
        // checked: `.tbss` occupies address space the containing `PT_LOAD` need
        // not cover, and it is never read from, only zeroed in a buffer of its
        // own.
        let entry = extent
            .range(ehdr.entry, 1)
            .ok_or(Error::EntryOutsideImage)?
            .start();
        let tls = match tls {
            None => None,
            Some(t) => {
                let template = extent.range(t.vaddr, t.filesz).ok_or(Error::TlsOutsideImage)?;
                // `p_align` reaches the TLS block as both an addend to its size
                // and the mask `!(align - 1)`. Neither survives an arbitrary
                // u64: the addition overflows, and a non-power-of-two turns the
                // mask into noise that can place the TLS data on top of the DTV.
                // Zero and one mean "no alignment constraint".
                if t.align > MAX_TLS_ALIGN || !(t.align == 0 || t.align.is_power_of_two()) {
                    return Err(Error::BadTlsAlign);
                }
                Some(TlsSegment { template, memsz: t.memsz, align: t.align })
            }
        };
        let dynamic = match dynamic {
            None => None,
            Some(d) => Some(DynamicSegment {
                file_offset: d.offset,
                image: extent.range(d.vaddr, d.filesz).ok_or(Error::DynamicOutsideImage)?,
            }),
        };
        let eh_frame_hdr = match eh_frame_hdr {
            None => None,
            Some(e) => Some(extent.range(e.vaddr, e.memsz).ok_or(Error::EhFrameOutsideImage)?),
        };

        Ok(Layout {
            extent,
            entry,
            segments,
            segment_count,
            tls,
            dynamic,
            section_headers: section_table(&ehdr),
            eh_frame_hdr,
        })
    }

    pub fn segments(&self) -> &[Segment] {
        self.segments.get(..self.segment_count).unwrap_or(&[])
    }

    /// `[vaddr_min, vaddr_max]`, the bound every address the file names is
    /// held to.
    pub const fn extent(&self) -> Extent {
        self.extent
    }

    /// Bytes between the lowest and highest address any segment claims.
    pub const fn span(&self) -> u64 {
        self.extent.span()
    }

    /// `e_entry`: inside the image, with at least one byte after it.
    pub const fn entry(&self) -> ImageOffset {
        self.entry
    }

    pub const fn tls(&self) -> Option<TlsSegment> {
        self.tls
    }

    /// The `PT_TLS` `p_memsz`, when the image has a TLS segment.
    pub fn tls_memsz(&self) -> Option<u64> {
        self.tls.map(|t| t.memsz)
    }

    pub const fn dynamic(&self) -> Option<DynamicSegment> {
        self.dynamic
    }

    pub const fn section_headers(&self) -> Option<SectionTableRef> {
        self.section_headers
    }

    /// `PT_GNU_EH_FRAME`, for DWARF unwinding.
    pub const fn eh_frame_hdr(&self) -> Option<ImageRange> {
        self.eh_frame_hdr
    }

    /// The writable window `[lo, hi)` in image offsets, or `None` when no
    /// segment is writable.
    ///
    /// This is the extent a relocation may write into once the module's
    /// read-only pages are shared between processes: past it the write would
    /// land in another process's copy.
    pub fn writable_window(&self) -> Option<(u64, u64)> {
        let mut window: Option<(u64, u64)> = None;
        for seg in self.segments() {
            if !seg.writable() {
                continue;
            }
            let (lo, hi) = (seg.image.start().get(), seg.image.end().get());
            window = Some(match window {
                Some((w_lo, w_hi)) => (w_lo.min(lo), w_hi.max(hi)),
                None => (lo, hi),
            });
        }
        window
    }

    /// The first pair of `PT_LOAD` segments whose page-rounded ranges overlap.
    ///
    /// Each segment becomes a demand-paged region, and a region map holds one
    /// region per address: two segments that merely *share* a page are two
    /// regions at one address. The loader asks before it inserts anything, so a
    /// refusal leaves the address space as it found it.
    pub fn overlapping_load_pages(&self, page_size: u64) -> Option<(usize, usize)> {
        let segs = self.segments();
        for (i, a) in segs.iter().enumerate() {
            let (a_start, a_end) = a.page_range(page_size);
            for (j, b) in segs.iter().enumerate().skip(i.wrapping_add(1)) {
                let (b_start, b_end) = b.page_range(page_size);
                if a_start < b_end && b_start < a_end {
                    return Some((i, j));
                }
            }
        }
        None
    }

    /// The file offset a virtual address maps to.
    ///
    /// Falls back to extrapolating from the nearest segment at or below
    /// `vaddr`, which is what `.rela.dyn` and friends need when the linker
    /// places them past a segment's file-backed extent.
    ///
    /// `None` when there is no segment at or below `vaddr` to extrapolate
    /// from, or when the extrapolation overflows. Every `vaddr` asked here is a
    /// `DT_*` tag, so the answer to "this address is in no segment" is that the
    /// binary is malformed, not that the kernel dies.
    pub fn vaddr_to_file_offset(&self, vaddr: u64) -> Option<u64> {
        let into = |seg: &Segment| vaddr.checked_sub(self.seg_vaddr(seg));
        for seg in self.segments() {
            if let Some(within) = into(seg).filter(|&w| w < seg.filesz) {
                return seg.file_offset.checked_add(within);
            }
        }
        let mut best: Option<(&Segment, u64)> = None;
        for seg in self.segments() {
            if let Some(within) = into(seg) {
                if best.is_none_or(|(_, w)| within < w) {
                    best = Some((seg, within));
                }
            }
        }
        let (seg, within) = best?;
        seg.file_offset.checked_add(within)
    }

    /// The file offset of an image offset this layout handed out, as
    /// [`vaddr_to_file_offset`](Self::vaddr_to_file_offset) maps it.
    pub fn file_offset_of(&self, at: ImageOffset) -> Option<u64> {
        self.vaddr_to_file_offset(self.extent.min.checked_add(at.get())?)
    }

    /// How many bytes of file back `vaddr` before the segment holding it runs
    /// out.
    ///
    /// `.gnu.hash` declares no length anywhere — its extent is the section it
    /// lives in and no `DT_*` tag names one — so the honest bound is the
    /// containing segment's own file image. `None` when no segment's file
    /// image covers `vaddr`.
    pub fn file_bytes_from(&self, vaddr: u64) -> Option<u64> {
        for seg in self.segments() {
            let Some(within) = vaddr.checked_sub(self.seg_vaddr(seg)) else { continue };
            if within < seg.filesz {
                return seg.filesz.checked_sub(within);
            }
        }
        None
    }

    /// A segment's `p_vaddr`, back out of its image offset: inside the extent,
    /// so the sum cannot overflow.
    fn seg_vaddr(&self, seg: &Segment) -> u64 {
        self.extent.min.wrapping_add(seg.image.start)
    }
}

/// A section header table the loader can index, or `None`.
///
/// `e_shentsize` must be exactly 64: consumers divide a byte count by it and
/// read 64-byte fields out of each entry, so a smaller stride is a short read
/// and a larger one is a table this crate does not know the shape of.
fn section_table(ehdr: &FileHeader) -> Option<SectionTableRef> {
    if ehdr.shoff == 0 || ehdr.shnum == 0 || usize::from(ehdr.shentsize) != SECTION_HEADER_SIZE {
        return None;
    }
    Some(SectionTableRef {
        file_offset: ehdr.shoff,
        count: ehdr.shnum,
        entry_size: ehdr.shentsize,
    })
}

/// A static image loaded at its own vaddrs into one allocation with its stack
/// after it, in offsets from the allocation's base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackedImage {
    /// Bytes to allocate: the image, then the stack.
    pub size: u64,
    /// Where the stack's lowest byte is; its top is `size`.
    pub stack: u64,
}

impl StackedImage {
    /// The image ending at `vaddr_max` with a `stack_size`-byte stack after it,
    /// or `None` for one no address holds. The stack starts on the next page:
    /// where the image ends is the linker's choice, and both ABIs want the
    /// stack pointer 16-byte aligned, which AArch64's `SCTLR_EL1.SA` enforces on
    /// every access through it. `stack_size` is whole pages, so the top is too.
    pub fn place(vaddr_max: u64, stack_size: u64) -> Option<StackedImage> {
        const PAGE: u64 = 4096;
        if !stack_size.is_multiple_of(PAGE) {
            return None;
        }
        let stack = vaddr_max.checked_next_multiple_of(PAGE)?;
        Some(StackedImage { size: stack.checked_add(stack_size)?, stack })
    }
}
