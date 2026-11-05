//! A shared object's image, as the loader lays it out and relocates it: every
//! table it reads, through `toyos-elf`, out of the image's own mapped bytes.
//! It touches nothing but the memory it is handed, so the host tests it on
//! images it lays out (`toyos-libc-copies`).
//!
//! An image is the process's own input: a crafted one corrupts only the
//! process that loads it, and is refused by name wherever `toyos-elf` or the
//! rules here can tell.

use alloc::vec::Vec;
use core::marker::PhantomData;

use toyos_elf::dynamic::ENTRY_SIZE as DYN_SIZE;
use toyos_elf::{
    rela, Dynamic, GnuHash, InitArray, Layout, Machine, Op, RelaTable, RelocError, SegmentFlags, Sym, SymTab, TlsRef,
    TlsSegment,
};

/// The page an image's protections are given in.
pub(crate) const PAGE: u64 = 4096;

/// The machine this library runs on, whose images it loads.
#[cfg(target_arch = "x86_64")]
pub(crate) const MACHINE: Machine = Machine::X86_64;
#[cfg(target_arch = "aarch64")]
pub(crate) const MACHINE: Machine = Machine::Aarch64;

const DT_NEEDED: u64 = 1;

/// What an image's header says it is, before anything of it is laid out.
pub(crate) struct Plan {
    layout: Layout,
    /// Bytes from its first page to past its last.
    pub(crate) span: usize,
}

/// The image `header` begins, `header` reaching at least past its program
/// header table: one whose addresses start at zero, whose program header table
/// a `PT_LOAD` maps and whose segments share no page.
pub(crate) fn plan(header: &[u8]) -> Result<Plan, &'static str> {
    let layout = Layout::parse(header, MACHINE).map_err(|e| e.as_str())?;
    // Every bound below is image-relative and every `r_offset` a vaddr, so a
    // non-zero `vaddr_min` would shift the two against each other.
    if layout.extent().min() != 0 {
        return Err("ELF: a shared object must begin at vaddr 0");
    }
    if layout.program_headers().is_none() {
        return Err("ELF: no PT_LOAD maps the program header table");
    }
    if layout.overlapping_load_pages(PAGE).is_some() {
        return Err("ELF: two PT_LOAD segments share a page");
    }
    let span = layout.span().checked_next_multiple_of(PAGE).and_then(|s| usize::try_from(s).ok());
    let span = span.ok_or("ELF: an image span no address space holds")?;
    Ok(Plan { layout, span })
}

impl Plan {
    /// Copy every `PT_LOAD`'s file bytes from `file` into `image`, its first
    /// `span` bytes zeroed and writable.
    pub(crate) fn lay(&self, file: &[u8], image: &mut [u8]) -> Result<(), &'static str> {
        for seg in self.layout.segments() {
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
    pub(crate) fn tables(&self, check: bool) -> Result<Option<Tables<'m>>, &'static str> {
        const OUTSIDE: &str = "ELF: a dynamic table outside the image's segments";
        let Some(dynamic) = self.plan.layout.dynamic() else { return Ok(None) };
        let range = dynamic.image();
        let dynamic = self.view(range.start().get(), range.len()).ok_or(OUTSIDE)?;
        let parsed = Dynamic::parse(dynamic);
        let gnu_hash = parsed.gnu_hash.map(|at| self.view_to_end(at).ok_or(OUTSIDE)).transpose()?;
        let count = parsed.sym_count(gnu_hash).unwrap_or_else(|gap| gap);
        let syms = match parsed.symtab {
            Some(at) => {
                let whole = self.view_to_end(at).ok_or(OUTSIDE)?;
                &whole[..whole.len().min(count.saturating_mul(toyos_elf::sym::ENTRY_SIZE))]
            }
            None => &[][..],
        };
        let strs = match parsed.strtab {
            Some(at) => self.view(at, parsed.strsz.unwrap_or(0)).ok_or(OUTSIDE)?,
            None => &[][..],
        };
        let table = |t: Option<toyos_elf::Table>| t.map(|t| self.view(t.vaddr, t.size).ok_or(OUTSIDE)).transpose();
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
pub(crate) struct Tables<'a> {
    dynamic: &'a [u8],
    symbols: SymTab<'a>,
    gnu_hash: Option<GnuHash<'a>>,
    rela: Option<&'a [u8]>,
    jmprel: Option<&'a [u8]>,
    rules: rela::Rules,
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
            .map(move |(_, at)| toyos_elf::cstr(strs, at))
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

    /// The address this image, mapped at `base`, gives the non-TLS symbol it
    /// defines as `name`: any defined one, as `dlsym` reads it.
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
        let sym = self.exported(name).filter(|s| s.kind() == toyos_elf::sym::STT_TLS)?;
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pass {
    Library,
    Executable,
}

/// Apply `tables`' relocations to `mapped`, module `id` of the process,
/// against `scope` and then the image's own definitions. Every value is
/// computed before the first write, so a refused image is left as laid.
/// Answers how many name a symbol nothing defines, each written as zero, or
/// for a `DTPMOD`, as the image's own id.
pub(crate) fn relocate(mapped: &Mapped<'_, '_>, tables: &Tables<'_>, id: usize, scope: &dyn Scope, pass: Pass) -> Result<usize, &'static str> {
    let base = mapped.base();
    let symbol = |i: toyos_elf::SymIndex| -> (Sym, &str) {
        let sym = tables.symbols.get(i.get()).expect("dlimage: a parse bounded the index by the table");
        (sym, sym.name_in(tables.symbols.strings()))
    };
    let own_tls = |at: toyos_elf::TlsOffset| at.get();
    let mut unresolved = 0;
    let mut writes = Vec::new();
    for raw in tables.relocations() {
        let Some(r) = rela::parse(raw, &tables.rules, tables.symbols).map_err(|e| e.as_str())? else { continue };
        let value: u64 = match r.op() {
            Op::Relative(_) if pass == Pass::Executable => continue,
            Op::Relative(target) => base + target.get(),
            Op::Bind(i) => {
                let (sym, name) = symbol(i);
                let own = sym.address(tables.rules.extent).map(|at| base + at.get());
                match scope.address(name).or(own) {
                    Some(at) => at,
                    None => {
                        unresolved += 1;
                        0
                    }
                }
            }
            Op::Tpoff64(_) | Op::Tpoff32(_) => return Err("ELF: initial-exec TLS outside the executable's own block"),
            Op::DtpMod64(i) => match i.map(symbol) {
                None => id as u64,
                Some((sym, _)) if sym.is_defined() => id as u64,
                Some((_, name)) => match scope.tls(name, 0) {
                    Some(found) => found.map_err(|e| e.as_str())?.0 as u64,
                    None => {
                        unresolved += 1;
                        id as u64
                    }
                },
            },
            Op::DtpOff64(TlsRef::Own(at)) => own_tls(at),
            Op::DtpOff64(TlsRef::Symbol(s)) => {
                let (sym, name) = symbol(s.sym());
                if sym.is_defined() {
                    let segment = tables.rules.tls.ok_or(RelocError::TlsOutsideSegment.as_str())?;
                    sym.tls_offset(s.addend(), segment).ok_or(RelocError::TlsOutsideSegment.as_str())?.get()
                } else {
                    match scope.tls(name, s.addend()) {
                        Some(found) => found.map_err(|e| e.as_str())?.1,
                        None => {
                            unresolved += 1;
                            0
                        }
                    }
                }
            }
        };
        writes.push((r.offset(), value));
    }
    for (at, value) in writes {
        mapped.write(at, &value.to_le_bytes());
    }
    Ok(unresolved)
}
