//! Seeded random images over the ELF *value* path: `r_addend`, `st_value`,
//! `DT_INIT_ARRAY`, the TLS segment's size and alignment, and every `TPOFF`
//! derived from them.
//!
//! Every other test here varies where a table is, never what it says. Each
//! image is a structurally valid
//! module — text, data, `PT_DYNAMIC`, `PT_TLS`, a relocation table, a symbol
//! table, an init array — whose numbers are drawn honest most of the time and
//! hostile the rest, and then run through the calls the kernel's loader makes,
//! in the order it makes them. The property:
//!
//! - nothing panics — the test binary runs with overflow checks on, as the
//!   kernel does, so an unchecked sum here is the kernel panic it stands for;
//! - an image is refused, or every address derived from it lies inside the
//!   image placed at the highest address its span fits at, and every
//!   thread-pointer offset inside the thread's TLS block.
//!
//! The generator also counts what it got past the parse, so a regression that
//! refuses everything cannot pass as one that accepts nothing wrong.

#[allow(dead_code)]
mod common;

use common::*;
use toyos_elf::dynamic::{Dynamic, InitArray};
use toyos_elf::rela::{self, Op, ReadTables, RelaTable, Rules, TlsRef};
use toyos_elf::sym::{self, SymTab};
use toyos_elf::tls::{self, Static, TlsOffset, Variant};
use toyos_elf::{ImageRange, Layout, Machine, TlsSegment};

/// Images per run: every one of them reaches the relocation parse unless its
/// own headers are what the mutation broke.
const ITERATIONS: u64 = 500_000;

const SIZE: usize = 0x4000;
const RW: u64 = 0x2000;
const DYNAMIC: u64 = 0x800;
const RELA: u64 = 0x1000;
const SYMTAB: u64 = 0x1400;
const STRTAB: u64 = 0x1800;
const INIT: u64 = 0x2100;
const TDATA: u64 = 0x3000;
const MAX_RELAS: u64 = 32;
const MAX_SYMS: u64 = 16;

const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_SYMTAB: i64 = 6;
const DT_STRTAB: i64 = 5;
const DT_STRSZ: i64 = 10;
const DT_INIT_ARRAY: i64 = 25;
const DT_INIT_ARRAYSZ: i64 = 27;

/// The kernel's TLS constants: a 64-byte TCB, a 64-entry DTV behind a
/// two-word header, 2 MiB allocations.
const TCB: usize = 64;
const DTV: usize = 16 + 64 * 8;
const GRANULE: usize = 2 * 1024 * 1024;

/// Where the kernel places an executable: `USER_VM_BASE`.
const USER_VM_BASE: u64 = 0x100_0000_0000;

/// xorshift64*: deterministic, so a red names its seed and iteration.
struct Rng {
    state: u64,
    /// Percent of the numbers `value` draws that are hostile, per image.
    hostile: u64,
}

impl Rng {
    fn next(&mut self) -> u64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[self.below(from.len() as u64) as usize]
    }

    /// `honest` most of the time; otherwise a number chosen to sit on an edge
    /// the loader has to get right, or anywhere at all.
    fn value(&mut self, honest: u64, edges: &[u64]) -> u64 {
        if !self.chance(self.hostile) {
            return honest;
        }
        const WILD: [u64; 14] = [
            0,
            1,
            8,
            0xfff,
            0x1000,
            i64::MAX as u64,
            1 << 63,
            u64::MAX,
            u64::MAX - 7,
            u64::MAX - 0xfff,
            USER_VM_BASE,
            1 << 32,
            (1 << 32) - 1,
            0xFFFF_8000_0000_0000,
        ];
        match self.below(3) {
            0 => self.pick(&WILD),
            1 if !edges.is_empty() => self.pick(edges),
            _ => self.next(),
        }
    }
}

/// One module, with the numbers it was built from.
struct Case {
    bytes: Vec<u8>,
    machine: Machine,
}

fn reloc_number(machine: Machine, which: usize) -> u32 {
    // RELATIVE, GLOB_DAT, JUMP_SLOT, DTPMOD64, DTPOFF64, TPOFF64, TPOFF32.
    const X86: [u32; 7] = [8, 6, 7, 16, 17, 18, 23];
    const A64: [u32; 7] = [1027, 1025, 1026, 1028, 1029, 1030, 1027];
    match machine {
        Machine::X86_64 => X86[which],
        Machine::Aarch64 => A64[which],
    }
}

fn generate(rng: &mut Rng) -> Case {
    // Most images carry one or two hostile numbers, some none, some many: a
    // rate per number would refuse nearly every image over its first one.
    rng.hostile = rng.pick(&[0, 1, 2, 5, 15]);
    let machine = if rng.chance(50) { Machine::X86_64 } else { Machine::Aarch64 };
    // Where the kernel loads a library: at vaddr 0.
    let vmin = 0u64;
    let bss = rng.pick(&[0u64, 0x800, 0x3000]);
    let span = SIZE as u64 + bss;

    let honest_memsz = 0x40 + rng.below(0x400);
    let tls_memsz = rng.value(honest_memsz, &[0, 0x40, 0x1F_FFF0, 1 << 40, i64::MAX as u64]);
    let tls_align = if rng.chance(95) { rng.pick(&[0u64, 1, 8, 16, 64]) } else { rng.value(3, &[]) };
    let tls_filesz = 0x40.min(tls_memsz);

    let n_relas = rng.below(MAX_RELAS + 1);
    let n_syms = 1 + rng.below(MAX_SYMS);
    let n_init = rng.below(8);

    let elf_machine = match machine {
        Machine::X86_64 => EM_X86_64,
        Machine::Aarch64 => EM_AARCH64,
    };
    let mut bytes = Elf::new(SIZE)
        .machine(elf_machine)
        .entry(vmin)
        .ph(Phdr::load(0, vmin, RW, RW, PF_R | PF_X))
        .ph(Phdr::load(RW, vmin + RW, SIZE as u64 - RW, SIZE as u64 - RW + bss, PF_R | PF_W))
        .ph(Phdr { kind: PT_DYNAMIC, flags: PF_R, offset: DYNAMIC, vaddr: vmin + DYNAMIC, filesz: 0x100, memsz: 0x100, align: 8 })
        .ph(Phdr {
            kind: PT_TLS,
            flags: PF_R,
            offset: TDATA,
            vaddr: vmin + TDATA,
            filesz: tls_filesz,
            memsz: tls_memsz,
            align: tls_align,
        })
        .build();

    let edges = |v: u64| [vmin + span, vmin + span + 1, vmin.wrapping_sub(1), v.wrapping_add(1)];
    let tags = [
        (DT_RELA, rng.value(vmin + RELA, &edges(RELA))),
        (DT_RELASZ, rng.value(n_relas * 24, &[24 * MAX_RELAS + 1, u64::MAX - 23])),
        (DT_SYMTAB, rng.value(vmin + SYMTAB, &edges(SYMTAB))),
        (DT_STRTAB, rng.value(vmin + STRTAB, &edges(STRTAB))),
        (DT_STRSZ, 0x100),
        (DT_INIT_ARRAY, rng.value(vmin + INIT, &edges(vmin + span - 8))),
        (DT_INIT_ARRAYSZ, rng.value(n_init * 8, &[7, 9, span, u64::MAX - 7, 1 << 63])),
    ];
    put(&mut bytes, DYNAMIC, &dynamic(&tags));

    // Names "a", "b", ... so a symbol resolves by name to its own index.
    for i in 0..MAX_SYMS {
        put(&mut bytes, STRTAB + 1 + 2 * i, &[b'a' + i as u8, 0]);
    }
    for i in 1..n_syms {
        let tls = rng.chance(30);
        let info = (sym::STB_GLOBAL << 4) | if tls { sym::STT_TLS } else { rng.pick(&[1u8, sym::STT_FUNC]) };
        let shndx = if rng.chance(15) { 0 } else { rng.pick(&[1u16, 2, 0xfff1]) };
        let honest = if tls { rng.below(tls_memsz.min(0x400) + 1) } else { vmin + rng.below(span + 1) };
        let value = rng.value(honest, &[vmin + span, vmin + span + 1, tls_memsz, tls_memsz.wrapping_add(1)]);
        put(&mut bytes, SYMTAB + 24 * i, &sym(1 + 2 * (i as u32 - 1), info, shndx, value));
    }

    for i in 0..n_relas {
        let which = rng.below(7) as usize;
        let r_type = if rng.chance(3) { rng.next() as u32 } else { reloc_number(machine, which) };
        let slot = rng.below((SIZE as u64 - RW) / 8);
        let offset = rng.value(vmin + RW + 8 * slot, &[vmin + span - 4, vmin + span, 0xFF9]);
        let sym = if rng.chance(40) { 0 } else { rng.below(n_syms + 1) as u32 };
        let sym = if rng.chance(5) { rng.next() as u32 } else { sym };
        let honest = match which {
            0 => vmin + rng.below(span + 1),
            3..=6 if sym == 0 => rng.below(tls_memsz.min(0x400) + 1),
            _ => rng.below(17).wrapping_sub(8),
        };
        let addend = rng.value(honest, &[vmin + span, vmin + span + 1, tls_memsz, tls_memsz.wrapping_add(1)]) as i64;
        put(&mut bytes, RELA + 24 * i, &rela(offset, sym, r_type, addend));
    }

    for i in 0..n_init {
        put(&mut bytes, INIT + 8 * i, &(vmin + rng.below(RW)).to_le_bytes());
    }

    // And some noise no structure chose, anywhere past the file header.
    if rng.chance(10) {
        for _ in 0..1 + rng.below(4) {
            let at = 64 + rng.below(SIZE as u64 - 64) as usize;
            bytes[at] = rng.next() as u8;
        }
    }

    Case { bytes, machine }
}

fn put(bytes: &mut [u8], at: u64, data: &[u8]) {
    bytes[at as usize..at as usize + data.len()].copy_from_slice(data);
}

/// How far each image got, over the whole run.
#[derive(Default, Debug)]
struct Reached {
    images: u64,
    accepted: u64,
    relative: u64,
    bind: u64,
    tpoff: u64,
    dtpoff: u64,
    init_arrays: u64,
    symbols: u64,
}

impl Reached {
    /// Fold in one accepted image's counts.
    fn add(&mut self, image: &Reached) {
        self.accepted += 1;
        self.relative += image.relative;
        self.bind += image.bind;
        self.tpoff += image.tpoff;
        self.dtpoff += image.dtpoff;
        self.init_arrays += image.init_arrays;
        self.symbols += image.symbols;
    }
}

/// The bytes the image holds at `range`, as a loader holding the image reads
/// them. This image's file offsets are its image offsets.
fn at(bytes: &[u8], range: ImageRange) -> &[u8] {
    let start = range.start().get() as usize;
    bytes.get(start..start + range.len() as usize).unwrap_or(&[])
}

/// Where the image goes: where the kernel puts an executable, or the highest
/// address its whole span fits below — a sum that fits one may not fit the
/// other.
#[derive(Clone, Copy, Debug)]
enum Placement {
    UserVmBase,
    Highest,
}

/// Everything the kernel's loader decides about one image, in its order,
/// through the calls it makes. `Err` is a refusal; every address derived from
/// an accepted image is checked against the image as `placement` places it.
fn load(case: &Case, placement: Placement, reached: &mut Reached) -> Result<(), ()> {
    let layout = Layout::parse(&case.bytes, case.machine).map_err(|_| ())?;
    let extent = layout.extent();
    let span = layout.span();
    let image_start = match placement {
        Placement::UserVmBase => USER_VM_BASE,
        Placement::Highest => u64::MAX - span,
    };
    // A span no placement holds is refused before anything is derived, as
    // `toyos_userbound::rebase_base` refuses it.
    image_start.checked_add(span).ok_or(())?;
    // Where an address `image_start + offset` has to stay: the placement is
    // only legal because the whole span fits above it.
    let place = |offset: u64| -> u64 {
        assert!(offset <= span, "offset {offset:#x} past the span {span:#x}");
        image_start.checked_add(offset).expect("an offset inside the span leaves the placement")
    };
    if let Some(table) = layout.program_headers() {
        place(table.image().end().get());
    }

    let dyn_bytes = at(&case.bytes, layout.dynamic().ok_or(())?.image());
    let dynamic = Dynamic::parse(dyn_bytes);

    if let Some(init) = InitArray::parse(dynamic.init_array, extent).map_err(|_| ())? {
        place(init.range().end().get());
        place(init.range().start().get());
        assert_eq!(init.count() * 8, init.range().len());
        reached.init_arrays += 1;
    }

    let table = |vaddr: Option<u64>, len: u64| -> Result<ImageRange, ()> {
        vaddr.and_then(|v| extent.range(v, len)).ok_or(())
    };
    let sym_range = table(dynamic.symtab, MAX_SYMS * sym::ENTRY_SIZE as u64)?;
    let str_range = table(dynamic.strtab, 0x100)?;
    let symbols = SymTab::new(at(&case.bytes, sym_range), at(&case.bytes, str_range));
    let tls = layout.tls();
    symbols.bounded(extent, tls).map_err(|_| ())?;
    for (_, s) in symbols.defined() {
        if let Some(off) = s.address(extent) {
            place(off.get());
            reached.symbols += 1;
        } else {
            let segment = tls.expect("a TLS symbol `bounded` accepted has a segment");
            let off = s.tls_offset(0, segment).expect("a TLS symbol `bounded` accepted");
            assert!(off.get() <= segment.memsz());
        }
    }

    let rela_range = match dynamic.rela {
        Some(t) => extent.range(t.vaddr, t.size).ok_or(())?,
        None => return Err(()),
    };
    let rela_bytes = at(&case.bytes, rela_range);
    let window = layout.writable_window().ok_or(())?;
    let span_of = |r: ImageRange| (r.start().get(), r.end().get());
    let tables = ReadTables {
        dynsym: span_of(sym_range),
        dynstr: span_of(str_range),
        rela: span_of(rela_range),
        jmprel: (0, 0),
    };
    rela::tables_outside_window(&tables, window, 4096).map_err(|_| ())?;
    let rules = Rules { extent, window, tls };
    let mut relocs = Vec::new();
    for raw in RelaTable::new(rela_bytes, case.machine).iter() {
        if let Some(r) = rela::parse(raw, &rules, symbols).map_err(|_| ())? {
            relocs.push(r);
        }
    }

    // The thread's static block: this module alone, placed as the kernel's
    // `build_tls_layout` places an executable's.
    let variant = Variant::of(case.machine);
    let (block, base_offset, memsz) = match tls.and_then(TlsSegment::occupied) {
        Some(t) => {
            let memsz = usize::try_from(t.memsz()).map_err(|_| ())?;
            let align = usize::try_from(t.align()).map_err(|_| ())?;
            let placed = match variant {
                Variant::II => tls::exe_extent(memsz, align).ok_or(())?,
                Variant::I => memsz,
            };
            let (base, total) = tls::place_module(0, placed, align).ok_or(())?;
            (Static::new(variant, total, align, align).ok_or(())?, base, t.memsz())
        }
        None => (Static::empty(variant), 0, 0),
    };
    let plan = block.plan(TCB, DTV, GRANULE).ok_or(())?;
    let thread_offset = |at: TlsOffset, width_i32: bool| -> Result<i64, ()> {
        let tpoff = block.tpoff(base_offset, at).ok_or(())?;
        if width_i32 {
            i32::try_from(tpoff).map_err(|_| ())?;
        }
        // `tp + tpoff` is the datum, and it lies in the block's TLS data.
        let datum = i128::from(plan.tp_offset as u64) + i128::from(tpoff);
        let data = i128::from(plan.tls_start as u64);
        assert!(
            (data..=data + i128::from(block.total_memsz() as u64)).contains(&datum),
            "TPOFF {tpoff:#x} names {datum:#x}, outside the TLS data at {data:#x}+{:#x}",
            block.total_memsz(),
        );
        Ok(tpoff)
    };
    let resolve = |r: TlsRef| -> Result<Option<TlsOffset>, ()> {
        match r {
            TlsRef::Own(off) => Ok(Some(off)),
            // Defined here: bounded against this module's segment. Undefined:
            // another module's, which the kernel resolves by name and this
            // image has no other module to find it in.
            TlsRef::Symbol(s) => match symbols.get(s.sym().get()).filter(|d| d.is_defined()) {
                Some(d) => tls.and_then(|t| d.tls_offset(s.addend(), t)).map(Some).ok_or(()),
                None => Ok(None),
            },
        }
    };

    for r in relocs {
        let width = match r.op() {
            Op::Tpoff32(_) => 4,
            _ => 8,
        };
        assert!(r.offset() >= window.0 && r.offset() + width <= window.1, "{r:?} outside {window:x?}");
        match r.op() {
            Op::Relative(off) => {
                place(off.get());
                reached.relative += 1;
            }
            Op::Bind(idx) => {
                if let Some(s) = symbols.get(idx.get()) {
                    if let Some(off) = s.address(extent) {
                        place(off.get());
                    }
                }
                reached.bind += 1;
            }
            Op::Tpoff64(t) | Op::Tpoff32(t) => {
                if let Some(off) = resolve(t)? {
                    thread_offset(off, matches!(r.op(), Op::Tpoff32(_)))?;
                    reached.tpoff += 1;
                }
            }
            Op::DtpOff64(t) => {
                if let Some(off) = resolve(t)? {
                    assert!(off.get() <= memsz, "DTPOFF {:#x} past PT_TLS {memsz:#x}", off.get());
                    reached.dtpoff += 1;
                }
            }
            Op::DtpMod64(_) => {}
        }
    }
    Ok(())
}

#[test]
fn every_derived_address_lies_inside_the_image_or_the_image_is_refused() {
    let mut reached = Reached::default();
    for seed in [0x9E37_79B9_7F4A_7C15u64, 0xD1B5_4A32_D192_ED03] {
        let mut rng = Rng { state: seed, hostile: 0 };
        for i in 0..ITERATIONS / 2 {
            let case = generate(&mut rng);
            for placement in [Placement::UserVmBase, Placement::Highest] {
                reached.images += 1;
                let mut this = Reached::default();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    load(&case, placement, &mut this)
                }));
                match outcome {
                    Ok(Ok(())) => reached.add(&this),
                    Ok(Err(())) => {}
                    Err(_) => panic!(
                        "seed {seed:#x} iteration {i} ({:?}, placed {placement:?}) \
                         panicked; its image is {} bytes",
                        case.machine,
                        case.bytes.len(),
                    ),
                }
            }
        }
    }
    println!("{reached:?}");

    // The run reached every question, not just the headers.
    assert!(reached.accepted * 10 >= reached.images, "{reached:?}");
    for (what, n) in [
        ("RELATIVE", reached.relative),
        ("GLOB_DAT/JUMP_SLOT", reached.bind),
        ("TPOFF", reached.tpoff),
        ("DTPOFF", reached.dtpoff),
        ("DT_INIT_ARRAY", reached.init_arrays),
        ("defined symbols", reached.symbols),
    ] {
        assert!(n >= 1000, "only {n} accepted {what}: {reached:?}");
    }
}

/// Seeded note segments, honest most of the time and hostile the rest: no
/// size, name, alignment or cut makes `note::build_id` panic, and every id it
/// answers is a `GNU` build-id note's whole descriptor out of the bytes given.
#[test]
fn no_note_segment_panics_the_build_id_read() {
    use toyos_elf::note::{self, MAX_BUILD_ID, NT_GNU_BUILD_ID};
    let mut rng = Rng { state: 0xA076_1D64_78BD_642F, hostile: 20 };
    let mut found = 0u64;
    const ITERATIONS: u64 = 200_000;
    for i in 0..ITERATIONS {
        let align = rng.pick(&[4u64, 4, 8, 0, 1, 2, 16, u64::MAX]);
        let pad = if align == 8 { 8 } else { 4 };
        let mut bytes = Vec::new();
        for _ in 0..=rng.below(3) {
            let name: &[u8] = rng.pick(&[&b"GNU\0"[..], b"GNU", b"abcde\0", b""]);
            let desc_len = rng.below(MAX_BUILD_ID as u64 + 4) as usize;
            let namesz = rng.value(name.len() as u64, &[u32::MAX as u64, 0x7fff_ffff]) as u32;
            let descsz = rng.value(desc_len as u64, &[u32::MAX as u64, 0x7fff_ffff]) as u32;
            let kind = rng.value(NT_GNU_BUILD_ID as u64, &[1, 2, 4]) as u32;
            bytes.extend(namesz.to_le_bytes());
            bytes.extend(descsz.to_le_bytes());
            bytes.extend(kind.to_le_bytes());
            bytes.extend(name);
            bytes.resize(bytes.len().next_multiple_of(pad), 0);
            bytes.extend((0..desc_len).map(|_| rng.next() as u8));
            bytes.resize(bytes.len().next_multiple_of(pad), 0);
        }
        if rng.chance(10) {
            bytes.truncate(rng.below(bytes.len() as u64 + 1) as usize);
        }
        let read = std::panic::catch_unwind(|| note::build_id(&bytes, align).map(<[u8]>::to_vec));
        let Ok(read) = read else { panic!("iteration {i}: a {}-byte segment at align {align} panicked", bytes.len()) };
        if let Some(id) = read {
            assert!((1..=MAX_BUILD_ID).contains(&id.len()), "iteration {i}: {} bytes", id.len());
            assert!(bytes.windows(id.len()).any(|w| w == id), "iteration {i}: an id not in the segment");
            found += 1;
        }
    }
    assert!(found * 20 >= ITERATIONS, "only {found} build-ids read: the generator reaches nothing");
}
