//! An image, as the loader lays it out and relocates it: every table it
//! reads, through [`crate::elf`], out of the image's own mapped bytes. It touches
//! nothing but the memory it is handed, so the host tests it on images it lays
//! out.
//!
//! An image is the process's own input: a crafted one corrupts only the
//! process that loads it, and is refused by name wherever [`crate::elf`] or the
//! rules here can tell.

#[cfg(test)]
mod tests;

use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::elf::dynamic::ENTRY_SIZE as DYN_SIZE;
use crate::elf::sym::{STB_WEAK, STT_TLS};
use crate::elf::{
    rela, Dynamic, GnuHash, InitArray, Layout, Machine, Op, RelaTable, RelocError, Segment, SegmentFlags, Sym, SymTab,
    TlsRef, TlsSegment,
};

use super::Refused;

/// The page an image's protections are given in.
pub(crate) const PAGE: u64 = 4096;

/// The machine this program runs on, whose images it loads.
const MACHINE: Machine = Machine::RUNNING;

const DT_NEEDED: u64 = 1;

/// Whether a loader holds an image's file where nobody can write it, and so
/// maps the segments that are the file's own pages
/// ([`Plan::file_pages`]), or copies every segment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FilePages {
    Mapped,
    Copied,
}

/// What an image's header says it is, before anything of it is laid out.
pub(crate) struct Plan {
    layout: Layout,
    /// Bytes from its first page to past its last.
    pub(crate) span: usize,
}

/// The image `header` begins, `header` reaching at least past its program
/// header table: one whose addresses start at zero, whose program header table
/// a `PT_LOAD` maps and whose segments share no page.
pub(crate) fn plan(header: &[u8]) -> Result<Plan, Refused> {
    let layout = Layout::parse(header, MACHINE).map_err(|e| e.as_str())?;
    // Every bound below is image-relative and every `r_offset` a vaddr, so a
    // non-zero `vaddr_min` would shift the two against each other.
    if layout.extent().min() != 0 {
        return Err(Refused::Elf("ELF: an image must begin at vaddr 0"));
    }
    if layout.program_headers().is_none() {
        return Err(Refused::Elf("ELF: no PT_LOAD maps the program header table"));
    }
    if layout.overlapping_load_pages(PAGE).is_some() {
        return Err(Refused::Elf("ELF: two PT_LOAD segments share a page"));
    }
    let span = layout.span().checked_next_multiple_of(PAGE).and_then(|s| usize::try_from(s).ok());
    let span = span.ok_or("ELF: an image span no address space holds")?;
    Ok(Plan { layout, span })
}

impl Plan {
    /// A library's segments, which the loader protects as their flags say:
    /// one both writable and executable is refused rather than given either.
    pub(crate) fn library(&self) -> Result<(), Refused> {
        match self.layout.segments().iter().any(|seg| seg.flags().writable() && seg.flags().executable()) {
            true => Err(Refused::Elf("ELF: a library's PT_LOAD is both writable and executable")),
            false => Ok(()),
        }
    }

    /// Whether `seg` is the pages of its file, `file_len` bytes long, and no
    /// relocation writes a page of it: the writable window runs from the
    /// first writable segment to the last, over whatever lies between.
    fn is_file_pages(&self, seg: &Segment, file_len: u64) -> bool {
        let (lo, hi) = seg.page_range(PAGE);
        let written = self.layout.writable_window().is_some_and(|(w_lo, w_hi)| lo < w_hi && w_lo < hi);
        seg.is_file_pages(PAGE, file_len) && !written
    }

    /// Each segment that is the pages of its file, `file_len` bytes long:
    /// its whole pages `[lo, hi)` in image offsets, where in the file `lo`
    /// is, and its flags.
    pub(crate) fn file_pages(&self, file_len: u64) -> impl Iterator<Item = (u64, u64, u64, SegmentFlags)> + '_ {
        self.layout.segments().iter().filter(move |seg| self.is_file_pages(seg, file_len)).map(|seg| {
            let (lo, hi) = seg.page_range(PAGE);
            (lo, hi, seg.file_offset() & !(PAGE - 1), seg.flags())
        })
    }

    /// Copy the file bytes of every `PT_LOAD` from `file` into `image`, its
    /// first `span` bytes zeroed and writable, but those `pages` leaves to be
    /// mapped.
    pub(crate) fn lay(&self, file: &[u8], image: &mut [u8], pages: FilePages) -> Result<(), Refused> {
        for seg in self.layout.segments() {
            if pages == FilePages::Mapped && self.is_file_pages(seg, file.len() as u64) {
                continue;
            }
            let at = seg.image().start().get() as usize;
            let len = seg.filesz() as usize;
            let from = usize::try_from(seg.file_offset()).ok().filter(|&from| from.checked_add(len).is_some_and(|end| end <= file.len()));
            let from = from.ok_or("ELF: a PT_LOAD's file bytes lie past the file's end")?;
            // `Layout` holds `filesz <= memsz` and every segment inside the span.
            image[at..at + len].copy_from_slice(&file[from..from + len]);
        }
        Ok(())
    }

    /// Each segment's whole pages, `[lo, hi)` in image offsets, and what they
    /// may be used for: none shares a page with another (`plan`).
    pub(crate) fn segments(&self) -> impl Iterator<Item = (u64, u64, SegmentFlags)> + '_ {
        self.layout.segments().iter().map(|seg| {
            let (lo, hi) = seg.page_range(PAGE);
            (lo, hi, seg.flags())
        })
    }

    /// `PT_GNU_RELRO`'s whole pages, `[lo, hi)` in image offsets.
    pub(crate) fn relro(&self) -> Option<(u64, u64)> {
        self.layout.relro_pages(PAGE)
    }

    pub(crate) fn tls(&self) -> Option<TlsSegment> {
        self.layout.tls().and_then(TlsSegment::occupied)
    }

    /// `PT_GNU_EH_FRAME`'s image offset.
    pub(crate) fn eh_frame_hdr(&self) -> Option<u64> {
        self.layout.eh_frame_hdr().map(|r| r.start().get())
    }

    /// The program header table's image offset and entries.
    pub(crate) fn phdrs(&self) -> (u64, usize) {
        let table = self.layout.program_headers().expect("plan refused an image without one");
        (table.image().start().get(), usize::from(table.count()))
    }
}

/// An image laid out at `base` as `plan` describes it, its memory living for `'m`.
pub(crate) struct Mapped<'p, 'm> {
    base: *mut u8,
    plan: &'p Plan,
    memory: PhantomData<&'m [u8]>,
}

impl<'p, 'm> Mapped<'p, 'm> {
    /// # Safety
    /// Every `PT_LOAD` of `plan` is mapped readable at `base` plus its vaddr
    /// for `'m`, and nothing but [`relocate`] writes it meanwhile, into its
    /// writable window, which is writable.
    pub(crate) unsafe fn new(base: *mut u8, plan: &'p Plan) -> Self {
        Mapped { base, plan, memory: PhantomData }
    }

    pub(crate) fn base(&self) -> u64 {
        self.base as u64
    }

    /// `len` bytes from `vaddr`, all inside one `PT_LOAD`.
    fn view(&self, vaddr: u64, len: u64) -> Option<&'m [u8]> {
        let end = vaddr.checked_add(len)?;
        let inside = self.plan.layout.segments().iter().any(|seg| {
            let image = seg.image();
            image.start().get() <= vaddr && end <= image.end().get()
        });
        // SAFETY: inside a mapped `PT_LOAD` (`new`'s contract).
        inside.then(|| unsafe { core::slice::from_raw_parts(self.base.add(vaddr as usize), len as usize) })
    }

    /// From `vaddr` to the end of the `PT_LOAD` holding it.
    fn view_to_end(&self, vaddr: u64) -> Option<&'m [u8]> {
        let seg = self.plan.layout.segments().iter().find(|seg| seg.image().start().get() <= vaddr && vaddr < seg.image().end().get())?;
        self.view(vaddr, seg.image().end().get() - vaddr)
    }

    /// The image's dynamic tables, each inside a `PT_LOAD`; `None` for an
    /// image with no `PT_DYNAMIC`. `check`ed, every symbol it defines lies in
    /// it and no table on a page a relocation writes, as [`relocate`] needs: a
    /// module already published was checked as it was loaded.
    pub(crate) fn tables(&self, check: bool) -> Result<Option<Tables<'m>>, Refused> {
        const OUTSIDE: Refused = Refused::Elf("ELF: a dynamic table outside the image's segments");
        let Some(dynamic) = self.plan.layout.dynamic() else { return Ok(None) };
        let range = dynamic.image();
        let dynamic = self.view(range.start().get(), range.len()).ok_or(OUTSIDE)?;
        let parsed = Dynamic::parse(dynamic);
        let gnu_hash = parsed.gnu_hash.map(|at| self.view_to_end(at).ok_or(OUTSIDE)).transpose()?;
        let count = parsed.sym_count(gnu_hash).unwrap_or_else(|gap| gap);
        let syms = match parsed.symtab {
            Some(at) => {
                let whole = self.view_to_end(at).ok_or(OUTSIDE)?;
                &whole[..whole.len().min(count.saturating_mul(crate::elf::sym::ENTRY_SIZE))]
            }
            None => &[][..],
        };
        let strs = match parsed.strtab {
            Some(at) => self.view(at, parsed.strsz.unwrap_or(0)).ok_or(OUTSIDE)?,
            None => &[][..],
        };
        let table = |t: Option<crate::elf::Table>| t.map(|t| self.view(t.vaddr, t.size).ok_or(OUTSIDE)).transpose();
        let (rela, jmprel) = (table(parsed.rela)?, table(parsed.jmprel)?);
        let symbols = SymTab::new(syms, strs);
        let extent = self.plan.layout.extent();
        let window = self.plan.layout.writable_window().unwrap_or((extent.span(), extent.span()));
        let span_of = |bytes: &[u8]| {
            let at = bytes.as_ptr() as u64 - self.base();
            (at, at + bytes.len() as u64)
        };
        let read = rela::ReadTables {
            dynsym: span_of(syms),
            dynstr: span_of(strs),
            rela: rela.map_or((0, 0), span_of),
            jmprel: jmprel.map_or((0, 0), span_of),
        };
        if check {
            symbols.bounded(extent, self.plan.layout.tls()).map_err(|e| e.as_str())?;
            rela::tables_outside_window(&read, window, PAGE)?;
        }
        let init_array = InitArray::parse(parsed.init_array, extent).map_err(|e| e.as_str())?;
        Ok(Some(Tables {
            dynamic,
            symbols,
            gnu_hash: gnu_hash.and_then(GnuHash::parse),
            rela,
            jmprel,
            rules: rela::Rules { extent, window, tls: self.plan.layout.tls() },
            bind_now: parsed.bind_now,
            init_array: init_array.map(|a| (a.range().start().get(), a.count() as usize)),
        }))
    }

    /// Write `value`'s bytes at image offset `at`, which a parse bounded
    /// inside the writable window.
    fn write(&self, at: u64, value: &[u8]) {
        let (lo, hi) = self.plan.layout.writable_window().expect("dlimage: a write in an image with no writable segment");
        assert!(lo <= at && at + value.len() as u64 <= hi, "dlimage: a write at {at:#x} outside the window a parse bounded");
        // SAFETY: inside the writable window (`new`'s contract), which no
        // table read here lies on (`tables`).
        unsafe { core::ptr::copy_nonoverlapping(value.as_ptr(), self.base.add(at as usize), value.len()) };
    }
}

/// An image's dynamic tables, each a view of its mapped bytes.
#[derive(Clone)]
pub(crate) struct Tables<'a> {
    dynamic: &'a [u8],
    symbols: SymTab<'a>,
    gnu_hash: Option<GnuHash<'a>>,
    rela: Option<&'a [u8]>,
    jmprel: Option<&'a [u8]>,
    rules: rela::Rules,
    /// Whether a call's slot is bound as the image is loaded, not at the call.
    bind_now: bool,
    /// `DT_INIT_ARRAY`'s image offset and entries.
    pub(crate) init_array: Option<(u64, usize)>,
}

impl<'a> Tables<'a> {
    /// Every `DT_NEEDED` name, in the order the image lists them; a name
    /// past the string table is `""`.
    pub(crate) fn needed(&self) -> impl Iterator<Item = &'a str> + 'a {
        let strs = self.symbols.strings();
        self.dynamic
            .as_chunks::<DYN_SIZE>()
            .0
            .iter()
            .map(|entry| (u64::from_le_bytes(entry[..8].try_into().unwrap()), u64::from_le_bytes(entry[8..].try_into().unwrap())))
            .take_while(|&(tag, _)| tag != 0)
            .filter(|&(tag, _)| tag == DT_NEEDED)
            .map(move |(_, at)| crate::elf::cstr(strs, at))
    }

    /// The symbol this image exports as `name`, through its `.gnu.hash`
    /// where it has one.
    fn exported(&self, name: &str) -> Option<Sym> {
        let sym = match &self.gnu_hash {
            Some(hash) => self.symbols.get(hash.lookup(name, &self.symbols)?)?,
            None => self.symbols.find(name)?.1,
        };
        sym.is_exported().then_some(sym)
    }

    /// The address this image, mapped at `base`, gives the symbol it defines
    /// as `name`: any defined one, as `dlsym` reads it, but a thread-local,
    /// which has no one address.
    pub(crate) fn defined(&self, name: &str, base: u64) -> Option<u64> {
        let (_, sym) = self.symbols.find(name)?;
        Some(base + sym.address(self.rules.extent)?.get())
    }

    /// The address of the symbol this image, mapped at `base`, exports as `name`.
    pub(crate) fn address(&self, name: &str, base: u64) -> Option<u64> {
        Some(base + self.exported(name)?.address(self.rules.extent)?.get())
    }

    /// `S + A` of the TLS symbol this image exports as `name`, in its block.
    pub(crate) fn tls(&self, name: &str, addend: i64) -> Option<Result<u64, RelocError>> {
        let sym = self.exported(name).filter(|s| s.kind() == STT_TLS)?;
        let segment = self.rules.tls?;
        Some(sym.tls_offset(addend, segment).map(|at| at.get()).ok_or(RelocError::TlsOutsideSegment))
    }

    fn relocations(&self) -> impl Iterator<Item = rela::Rela> + 'a {
        let entries = |table: Option<&'a [u8]>| table.into_iter().flat_map(|t| RelaTable::new(t, MACHINE).iter());
        entries(self.rela).chain(entries(self.jmprel))
    }
}

/// What the modules loaded before an image define, which its symbols are
/// bound to before its own definitions.
pub(crate) trait Scope {
    /// The address a module gives the symbol `name`.
    fn address(&self, name: &str) -> Option<u64>;
    /// The id of the module whose thread-local `name` is, and `S + A` within
    /// its block.
    fn tls(&self, name: &str, addend: i64) -> Option<Result<(usize, u64), RelocError>>;
}

/// Which of an image's relocations this pass writes: a library's, all; an
/// executable's, all but the `RELATIVE`s it applied to itself.
pub(crate) enum Pass<'b> {
    Library,
    /// `block` is the main thread's block of the executable's thread-locals,
    /// copied from its template before this pass: what the pass writes into
    /// the template it writes there too. No other thread exists yet.
    Executable { block: &'b mut [u8] },
}

/// Apply `tables`' relocations to `mapped`, module `id` of the process,
/// against `scope` and then the image's own definitions. Every value is
/// computed before the first write, so a refused image is left as laid.
///
/// A symbol nothing defines is refused by name, but one the image names
/// weakly, whose address is 0, and a call's slot (`JUMP_SLOT`) in an image
/// not bound now: a lazy loader would bind that at the first call and fail
/// there, so it is bound to [`super::undefined_function`], which fails there
/// too. A Rust library's slot for `main` is one, which its std's `_start`
/// names and only an executable defines.
pub(crate) fn relocate(mapped: &Mapped<'_, '_>, tables: &Tables<'_>, id: usize, scope: &dyn Scope, pass: Pass<'_>) -> Result<(), Refused> {
    let base = mapped.base();
    let symbol = |i: crate::elf::SymIndex| -> (Sym, &str) {
        let sym = tables.symbols.get(i.get()).expect("dl: a parse bounded the index by the table");
        (sym, sym.name_in(tables.symbols.strings()))
    };
    let undefined = |name: &str| Refused::Undefined(String::from(name));
    let address = |i: crate::elf::SymIndex| -> Result<u64, Refused> {
        let (sym, name) = symbol(i);
        let own = sym.address(tables.rules.extent).map(|at| base + at.get());
        match scope.address(name).or(own) {
            Some(at) => Ok(at),
            None if sym.bind() == STB_WEAK => Ok(0),
            None => Err(undefined(name)),
        }
    };
    let elsewhere = |name: &str, addend: i64| -> Result<(usize, u64), Refused> {
        scope.tls(name, addend).ok_or_else(|| undefined(name))?.map_err(Refused::Relocation)
    };
    let own_block = || tables.rules.tls.ok_or(Refused::Relocation(RelocError::TlsOutsideSegment));
    let executable = matches!(pass, Pass::Executable { .. });
    let mut writes = Vec::new();
    for raw in tables.relocations() {
        let Some(r) = rela::parse(raw, &tables.rules, tables.symbols).map_err(Refused::Relocation)? else { continue };
        let value: u64 = match r.op() {
            Op::Relative(_) if executable => continue,
            Op::Relative(target) => base + target.get(),
            Op::Bind(i) => address(i)?,
            Op::Call(i) => match address(i) {
                Err(Refused::Undefined(_)) if !tables.bind_now => (super::undefined_function as *const ()).addr() as u64,
                bound => bound?,
            },
            Op::Absolute(i, addend) => i.map_or(Ok(0), address)?.wrapping_add_signed(addend),
            Op::Tpoff64(_) | Op::Tpoff32(_) => return Err(Refused::Elf("ELF: initial-exec TLS outside the executable's own block")),
            Op::DtpMod64(None) => id as u64,
            Op::DtpMod64(Some(i)) => match symbol(i) {
                (sym, _) if sym.is_defined() => id as u64,
                (_, name) => elsewhere(name, 0)?.0 as u64,
            },
            Op::DtpOff64(TlsRef::Own(at)) => at.get(),
            Op::DtpOff64(TlsRef::Symbol(s)) => match symbol(s.sym()) {
                (sym, _) if sym.is_defined() => {
                    let at = sym.tls_offset(s.addend(), own_block()?);
                    at.ok_or(Refused::Relocation(RelocError::TlsOutsideSegment))?.get()
                }
                (_, name) => elsewhere(name, s.addend())?.1,
            },
        };
        writes.push((r.offset(), value));
    }
    let template = mapped.plan.tls().map(|segment| segment.template());
    let mut block = match pass {
        Pass::Executable { block } => Some(block),
        Pass::Library => None,
    };
    for (at, value) in writes {
        let bytes = value.to_le_bytes();
        mapped.write(at, &bytes);
        if let (Some(block), Some(template)) = (block.as_deref_mut(), template) {
            let (lo, hi) = (template.start().get(), template.end().get());
            for (i, byte) in bytes.iter().enumerate() {
                let at = at + i as u64;
                if (lo..hi).contains(&at) {
                    block[(at - lo) as usize] = *byte;
                }
            }
        }
    }
    Ok(())
}
