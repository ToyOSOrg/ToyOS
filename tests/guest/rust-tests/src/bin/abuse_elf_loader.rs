//! A crafted ELF must not panic the kernel, and must not place a mapping the
//! kernel's own address-space rules forbid.
//!
//! `abuse_elf_segments` covers the sizes a program header declares.  This one
//! covers what the kernel *derives* from them: the address a segment lands at
//! once the image is rebased to `USER_VM_BASE`. Each case is a kernel panic or
//! a forbidden mapping before the fix; each must be an error return after it,
//! with the kernel intact. A library is the program's own to load (libc's
//! `dlopen`), so a crafted one is that program's to refuse, and its loader's
//! host tests (`toyos-libc-copies`) hold that.
//!
//! An executable's dynamic section and thread-local storage are the program's
//! own (`toyos::relocate`, `toyos::tls`): the spawn cases crafted in them are
//! answered either way, the kernel lives, and a child the kernel starts dies
//! ([`dies`]). A real std image whose dynamic section names what its own
//! `_start` refuses exits [`REFUSED_STATUS`] with the line it says
//! ([`refuses_itself`]).

#[path = "../image.rs"]
mod image;

use std::fs;
use std::process::Command;

use toyos_abi::syscall::{self, SpawnArgs, SyscallError};
use toyos_abi::RawHandle;
use toyos_abi::end::End;


/// Where each case's file is written. A spawn reads it back into a memory
/// object and hands the kernel that, as every spawn of a served program does.
const DIR: &str = "/tmp/abuse_loader_exe";

/// The two cases about a table larger than one kernel allocation need a file
/// larger than one kernel allocation, because `read_file_range` clamps a
/// declared length to what the file actually holds. `/tmp` is where that is
/// cheap: its pages are memory, so a 3 MiB file costs no device I/O, and
/// the case under test is the loader rather than the filesystem.
///
/// Both cases asserted on the *declared* length alone until `/tmp` became
/// loadable — nothing in the tree could produce a >2 MiB file the loader would
/// open, so they passed without ever reaching the heap assert they exist for.
const BIG_DIR: &str = "/tmp/abuse_loader";

/// Comfortably past `mm::MAX_HEAP_ALLOC`, and past what dlmalloc rounds a
/// 2 MiB granule request up to.
const BIG: usize = 3 * 1024 * 1024;

/// Kernel constants this test aims at, by value.
///
/// `USER_VM_BASE` was where every exe image was rebased to and `ALLOC_FLOOR`
/// the bottom of the arena every library, TLS block and mmap came out of;
/// both are now addresses the space's placement may hand out. `KERNEL_HALF`
/// is where the direct map starts.  A segment vaddr is only interesting here
/// relative to one of the three.
const USER_VM_BASE: u64 = 0x100_0000_0000;
const ALLOC_FLOOR: u64 = 0x0002_0000_0000;
const KERNEL_HALF: u64 = 0xFFFF_8000_0000_0000;

const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_TLS: u32 = 7;

const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

const DT_NEEDED: i64 = 1;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_STRSZ: i64 = 10;
const DT_JMPREL: i64 = 23;
const DT_PLTRELSZ: i64 = 2;
const DT_GNU_HASH: i64 = 0x6fff_fef5u32 as i32 as i64;

const SHT_DYNSYM: u32 = 11;

const R_X86_64_GLOB_DAT: u64 = 6;
const R_X86_64_RELATIVE: u64 = 8;
const R_X86_64_DTPMOD64: u64 = 16;
const R_X86_64_TPOFF64: u64 = 18;

const STT_TLS: u8 = 6;
const STB_GLOBAL: u8 = 1;

const PH_OFF: usize = 64;
const PH_SIZE: usize = 56;
const SH_SIZE: usize = 64;

#[derive(Clone, Copy)]
struct Phdr {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

impl Phdr {
    fn load(offset: u64, vaddr: u64, filesz: u64, memsz: u64, flags: u32) -> Self {
        Self { kind: PT_LOAD, flags, offset, vaddr, filesz, memsz, align: 0x1000 }
    }

    fn write(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.kind.to_le_bytes());
        out[4..8].copy_from_slice(&self.flags.to_le_bytes());
        out[8..16].copy_from_slice(&self.offset.to_le_bytes());
        out[16..24].copy_from_slice(&self.vaddr.to_le_bytes());
        out[24..32].copy_from_slice(&self.vaddr.to_le_bytes()); // p_paddr
        out[32..40].copy_from_slice(&self.filesz.to_le_bytes());
        out[40..48].copy_from_slice(&self.memsz.to_le_bytes());
        out[48..56].copy_from_slice(&self.align.to_le_bytes());
    }
}

/// A file the loader will accept as far as the case under test needs it to.
struct Elf {
    bytes: Vec<u8>,
    phdrs: Vec<Phdr>,
    entry: u64,
    shoff: u64,
    shnum: u16,
    shentsize: u16,
}

impl Elf {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0u8; size],
            phdrs: Vec::new(),
            entry: 0x1000,
            shoff: 0,
            shnum: 0,
            shentsize: SH_SIZE as u16,
        }
    }

    fn ph(mut self, p: Phdr) -> Self {
        self.phdrs.push(p);
        self
    }

    fn entry(mut self, v: u64) -> Self {
        self.entry = v;
        self
    }

    fn sections(mut self, shoff: u64, shnum: u16, shentsize: u16) -> Self {
        self.shoff = shoff;
        self.shnum = shnum;
        self.shentsize = shentsize;
        self
    }

    fn poke(mut self, off: usize, data: &[u8]) -> Self {
        self.bytes[off..off + data.len()].copy_from_slice(data);
        self
    }

    /// A DT_NULL-terminated dynamic table at `off`.
    fn dynamic(mut self, off: usize, tags: &[(i64, u64)]) -> Self {
        let mut at = off;
        for &(tag, val) in tags {
            self.bytes[at..at + 8].copy_from_slice(&tag.to_le_bytes());
            self.bytes[at + 8..at + 16].copy_from_slice(&val.to_le_bytes());
            at += 16;
        }
        self
    }

    /// One Elf64_Rela at file offset `off`.
    fn rela(self, off: usize, r_offset: u64, r_info: u64, r_addend: i64) -> Self {
        self.poke(off, &r_offset.to_le_bytes())
            .poke(off + 8, &r_info.to_le_bytes())
            .poke(off + 16, &r_addend.to_le_bytes())
    }

    /// One Elf64_Sym at file offset `off`.
    fn sym(self, off: usize, st_name: u32, st_info: u8, st_shndx: u16, st_value: u64) -> Self {
        self.poke(off, &st_name.to_le_bytes())
            .poke(off + 4, &[st_info, 0])
            .poke(off + 6, &st_shndx.to_le_bytes())
            .poke(off + 8, &st_value.to_le_bytes())
    }

    /// One Elf64_Shdr at file offset `off`.
    fn shdr(self, off: usize, sh_type: u32, sh_offset: u64, sh_size: u64, sh_entsize: u64) -> Self {
        self.poke(off + 4, &sh_type.to_le_bytes())
            .poke(off + 24, &sh_offset.to_le_bytes())
            .poke(off + 32, &sh_size.to_le_bytes())
            .poke(off + 56, &sh_entsize.to_le_bytes())
    }

    fn build(mut self) -> Vec<u8> {
        let phnum = self.phdrs.len();
        assert!(PH_OFF + phnum * PH_SIZE <= self.bytes.len(), "phdrs past the buffer");

        self.bytes[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        self.bytes[4] = 2; // ELFCLASS64
        self.bytes[5] = 1; // ELFDATA2LSB
        self.bytes[6] = 1; // EV_CURRENT
        self.bytes[16..18].copy_from_slice(&ET_DYN.to_le_bytes());
        self.bytes[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
        self.bytes[20..24].copy_from_slice(&1u32.to_le_bytes()); // e_version
        self.bytes[24..32].copy_from_slice(&self.entry.to_le_bytes());
        self.bytes[32..40].copy_from_slice(&(PH_OFF as u64).to_le_bytes());
        self.bytes[40..48].copy_from_slice(&self.shoff.to_le_bytes());
        self.bytes[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
        self.bytes[54..56].copy_from_slice(&(PH_SIZE as u16).to_le_bytes());
        self.bytes[56..58].copy_from_slice(&(phnum as u16).to_le_bytes());
        self.bytes[58..60].copy_from_slice(&self.shentsize.to_le_bytes());
        self.bytes[60..62].copy_from_slice(&self.shnum.to_le_bytes());

        for (i, p) in self.phdrs.iter().enumerate() {
            let at = PH_OFF + i * PH_SIZE;
            p.write(&mut self.bytes[at..at + PH_SIZE]);
        }
        self.bytes
    }
}

fn write_file(name: &str, bytes: &[u8]) -> String {
    write_file_in(DIR, name, bytes)
}

fn write_file_in(dir: &str, name: &str, bytes: &[u8]) -> String {
    let path = format!("{dir}/{name}");
    fs::write(&path, bytes).unwrap_or_else(|e| panic!("write {path}: {e}"));
    let got = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    assert_eq!(got as usize, bytes.len(), "{path}: short write");
    path
}

fn spawn_path(path: &str) -> Result<RawHandle, SyscallError> {
    spawn_argv(path, &format!("{path}\0"))
}

/// `path` read into an image and spawned with `argv`, NUL-separated, and no
/// descriptor of this process's.
fn spawn_argv(path: &str, argv: &str) -> Result<RawHandle, SyscallError> {
    let image = image::image_of(path);
    let (image, image_len) = image.spawn_words();
    unsafe {
        syscall::spawn(&SpawnArgs {
            path_ptr: path.as_ptr() as u64,
            path_len: path.len() as u64,
            argv_ptr: argv.as_ptr() as u64,
            argv_len: argv.len() as u64,
            env_ptr: 0,
            env_len: 0,
            endow_ptr: 0,
            endow_count: 0,
            labels_ptr: 0,
            labels_len: 0,
            image,
            image_len,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
            pager: u64::from(toyos_abi::HANDLE_INVALID.0),
        })
    }
}

fn spawn_result(name: &str, bytes: &[u8]) -> Result<RawHandle, SyscallError> {
    spawn_path(&write_file(name, bytes))
}

/// Spawn must refuse this file. A success is as much a failure as a panic:
/// every case here describes a process the kernel cannot safely build.
fn refused(name: &str, outcome: Result<RawHandle, SyscallError>) {
    match outcome {
        Ok(child) => panic!("{name}: spawn started {child:?} for an ELF that must be refused"),
        Err(e) => assert!(
            matches!(e, SyscallError::InvalidArgument | SyscallError::ResourceExhausted),
            "{name}: spawn gave {e:?}",
        ),
    }
}

fn spawn_refused(name: &str, bytes: &[u8]) {
    refused(name, spawn_result(name, bytes));
}

/// The kernel answers this spawn and lives, and a child it started dies. These
/// files are crafted in a dynamic section the kernel no longer reads: a
/// program relocates itself (`toyos::relocate`), and each of these enters at
/// bytes that are no program, so the child dies before it reads one.
fn dies(name: &str, outcome: Result<RawHandle, SyscallError>) {
    match outcome {
        Ok(child) => {
            // SAFETY: the handle `SYS_SPAWN` just answered, held nowhere else.
            let child = unsafe { toyos::process::Process::from_raw(child) };
            let end = child.wait().unwrap_or_else(|e| panic!("{name}: wait for the child: {e:?}"));
            assert!(!end.success(), "{name}: a child entering at bytes that are no program exited 0");
        }
        Err(e) => assert!(
            matches!(e, SyscallError::InvalidArgument | SyscallError::ResourceExhausted),
            "{name}: spawn gave {e:?}",
        ),
    }
}

fn spawn_dies(name: &str, bytes: &[u8]) {
    dies(name, spawn_result(name, bytes));
}

/// A minimal, honest exe: one PT_LOAD covering the whole file at vaddr 0.
fn base_exe(size: usize) -> Elf {
    Elf::new(size).ph(Phdr::load(0, 0, size as u64, size as u64, PF_R | PF_X)).entry(0)
}

fn main() {
    // A copy [`refuses_itself`] spawns that reached this far was not refused.
    if std::env::args().nth(1).as_deref() == Some(REFUSED_COPY) {
        panic!("a copy of this program whose relocations its _start must refuse reached main");
    }
    fs::create_dir_all(DIR).expect("create /tmp/abuse_loader_exe");
    fs::create_dir_all(BIG_DIR).expect("create /tmp/abuse_loader");

    // 1. A DT_* vaddr below every PT_LOAD.
    spawn_dies(
        "rela_below_image",
        &Elf::new(0x4000)
            .ph(Phdr::load(0, 0x1000, 0x3000, 0x3000, PF_R | PF_W))
            .ph(Phdr { kind: PT_DYNAMIC, flags: PF_R, offset: 0x1000, vaddr: 0x2000, filesz: 0x200, memsz: 0x200, align: 8 })
            .entry(0x1000)
            .dynamic(0x1000, &[(DT_RELA, 0), (DT_RELASZ, 24)])
            .build(),
    );

    // 2. Two PT_LOADs whose page-rounded ranges overlap. Each becomes a VMA,
    //    and `insert_region` asserts that an address is not already covered —
    //    a kernel-bug assert reached from a file.
    spawn_refused(
        "load_overlap",
        &base_exe(0x4000)
            .ph(Phdr::load(0x1000, 0x800, 0x1000, 0x1000, PF_R | PF_W))
            .build(),
    );

    // 3. A segment whose rebased address lands in the kernel half. The image
    //    is placed at `USER_VM_BASE - vaddr_min`, and that addition wraps, so
    //    a file can name any address in the machine. The VMA is demand-paged,
    //    so the first user touch calls `remap`, which ORs PAGE_USER onto the
    //    *shared* kernel page tables — the mapping `sys_mmap` refuses a FIXED
    //    request for, reached through the loader instead.
    spawn_refused(
        "load_kernel_half",
        &base_exe(0x4000)
            .ph(Phdr::load(0, KERNEL_HALF.wrapping_sub(USER_VM_BASE), 0, 0x1000, PF_R | PF_W))
            .build(),
    );

    // 4. And the same wrap aimed at the allocation arena rather than the
    //    kernel: one segment covering everything from ALLOC_FLOOR upwards
    //    leaves `find_gap` nothing, and the TLS mapping every process gets
    //    was an `.expect` on that.
    spawn_refused(
        "load_covers_arena",
        &base_exe(0x4000)
            // The largest p_memsz that still leaves `p_vaddr + p_memsz` inside
            // a u64, which `parse_layout` checks before anything else.
            .ph(Phdr::load(0, ALLOC_FLOOR.wrapping_sub(USER_VM_BASE), 0, 0xFD_FFFF_FFFF, PF_R | PF_W))
            .build(),
    );

    // 4b. The whole image above USER_VM_BASE: vaddr_min itself exceeds the base,
    //     so `USER_VM_BASE - vaddr_min` underflows — a whole-machine trap where
    //     cases 3 and 4's low vaddr_min and stretched span are caught by the span test.
    spawn_refused(
        "load_only_above_vm_base",
        &Elf::new(0x1000)
            .ph(Phdr::load(0, USER_VM_BASE * 2, 0, 0x1000, PF_R | PF_W))
            .entry(USER_VM_BASE * 2)
            .build(),
    );

    // 5. PT_TLS p_align is a file-chosen addend to the TLS block's size.
    //    u64::MAX wrapped the size computation, and the layout that fell out
    //    of it tripped the block's own "DTV overlaps TLS data" assert.
    spawn_refused(
        "tls_align_wrap",
        &base_exe(0x4000)
            .ph(Phdr { kind: PT_TLS, flags: PF_R, offset: 0x2000, vaddr: 0x2000, filesz: 0, memsz: 8, align: u64::MAX })
            .build(),
    );

    // 6. A TLS size that leaves no room for the DTV in front of it. The DTV
    //    is a fixed 528-byte header the kernel writes at the start of the
    //    same allocation, and nothing counted it when sizing that allocation.
    //    Legal to load once it is counted, so the assertion is survival.
    let _ = spawn_result(
        "tls_crowds_dtv",
        &base_exe(0x4000)
            .ph(Phdr { kind: PT_TLS, flags: PF_R, offset: 0x2000, vaddr: 0x2000, filesz: 0, memsz: 0x1F_FF00, align: 8 })
            .build(),
    );

    // 7. A TLS template just under 2 MiB. The kernel heap's page source
    //    asserts above 2 MiB, and dlmalloc rounds a request up to a whole
    //    2 MiB granule *plus* its own bookkeeping — so the guard on the
    //    allocation was short by exactly that bookkeeping.
    spawn_refused(
        "tls_template_2m",
        &base_exe(0x4000)
            .ph(Phdr { kind: PT_TLS, flags: PF_R, offset: 0x2000, vaddr: 0x2000, filesz: 0, memsz: 0x1F_FFF0, align: 8 })
            .build(),
    );

    // 8. A section header table of 2.4 MiB, in a file big enough to hold it,
    //    which the kernel read into one `Vec` past `mm/alloc.rs`'s assert when
    //    it looked for an executable's `.rela.dyn` and `.symtab` there.
    dies(
        "shnum_past_heap",
        spawn_path(&write_file_in(
            BIG_DIR,
            "shnum_past_heap",
            &base_exe(BIG).sections(0x1000, 40_000, 64).build(),
        )),
    );

    // 9. Same ceiling, reached through DT_STRSZ instead — a size no section
    //    header has to agree with, in a file that can back it.
    dies(
        "strsz_past_heap",
        spawn_path(&write_file_in(
            BIG_DIR,
            "strsz_past_heap",
            &Elf::new(BIG)
                .ph(Phdr::load(0, 0, BIG as u64, BIG as u64, PF_R | PF_W))
                .ph(Phdr { kind: PT_DYNAMIC, flags: PF_R, offset: 0x1000, vaddr: 0x1000, filesz: 0x200, memsz: 0x200, align: 8 })
                .entry(0)
                .dynamic(0x1000, &[(DT_STRTAB, 0x2000), (DT_STRSZ, 2 * 1024 * 1024 + 4096)])
                .build(),
        )),
    );

    // 12. Two relocation tables the ceiling accepted, whose derived index did
    //     not fit: `DT_RELASZ` and `DT_PLTRELSZ` were bounded apart and both
    //     fed one index the kernel built at spawn, 2.7 MiB in a single `Vec`.
    {
        const N: usize = 87_210; // MAX_HEAP_ALLOC / 24, the most either table can declare
        const SZ: usize = N * 24;
        const RELA: usize = 0x4000;
        const JMPREL: usize = RELA + SZ;
        let total = JMPREL + SZ;
        let mut bytes = Elf::new(total)
            .ph(Phdr::load(0, 0, total as u64, total as u64, PF_R | PF_W))
            .ph(Phdr { kind: PT_DYNAMIC, flags: PF_R, offset: 0x1000, vaddr: 0x1000, filesz: 0x200, memsz: 0x200, align: 8 })
            .entry(0)
            .dynamic(0x1000, &[
                (DT_RELA, RELA as u64), (DT_RELASZ, SZ as u64),
                (DT_JMPREL, JMPREL as u64), (DT_PLTRELSZ, SZ as u64),
            ])
            .build();
        for i in 0..N * 2 {
            let at = RELA + i * 24;
            bytes[at..at + 8].copy_from_slice(&((i as u64) * 8).to_le_bytes());
            bytes[at + 8..at + 16].copy_from_slice(&R_X86_64_RELATIVE.to_le_bytes());
        }
        dies(
            "rela_index_past_heap",
            spawn_path(&write_file_in(BIG_DIR, "rela_index_past_heap", &bytes)),
        );
    }

    // 12b. More distinct DT_NEEDED names than the kernel loaded at spawn, each
    //      one private window.
    {
        const N: usize = 65; // one past the kernel's 64, all distinct
        const STRTAB: usize = 0x2000;
        let mut tags: Vec<(i64, u64)> =
            vec![(DT_STRTAB, STRTAB as u64), (DT_STRSZ, (N * 4) as u64)];
        let mut elf = Elf::new(0x4000)
            .ph(Phdr::load(0, 0, 0x4000, 0x4000, PF_R | PF_W))
            .ph(Phdr { kind: PT_DYNAMIC, flags: PF_R, offset: 0x1000, vaddr: 0x1000, filesz: 0x800, memsz: 0x800, align: 8 })
            .entry(0);
        for i in 0..N {
            let off = i * 4;
            elf = elf.poke(STRTAB + off, &[b'a' + (i % 26) as u8, b'A' + (i / 26) as u8]);
            tags.push((DT_NEEDED, off as u64));
        }
        spawn_dies("too_many_needed_libs", &elf.dynamic(0x1000, &tags).build());
    }

    // 13. A RELATIVE write beginning in a page's last seven bytes: r_offset
    //     0x1FFE + 8 crosses 0x2000, which the kernel's per-page applier dropped.
    spawn_dies(
        "reloc_straddles_fill_page",
        &Elf::new(0x4000)
            .ph(Phdr::load(0, 0, 0x4000, 0x4000, PF_R | PF_W))
            .ph(Phdr { kind: PT_DYNAMIC, flags: PF_R, offset: 0x1000, vaddr: 0x1000, filesz: 0x200, memsz: 0x200, align: 8 })
            .entry(0)
            .dynamic(0x1000, &[(DT_RELA, 0x1200), (DT_RELASZ, 24)])
            .rela(0x1200, 0x1FFE, R_X86_64_RELATIVE, 0)
            .build(),
    );

    // 18. The values a file writes, not the places: each of these was an
    //     overflow panic in the kernel, reached by writing the file to /tmp.
    values_are_bounded_by_the_image();

    // 19. A relocation naming a symbol the executable's short-read `.dynsym` does
    //     not hold, which the kernel refused by name while it read one.
    globdat_past_short_dynsym();

    // 20. Thread-local references an executable's own start refuses.
    executable_tls_refs_die();

    // 21. A program header table no `PT_LOAD` maps: the one segment's file
    //     bytes start at 0x1000, past the table at 0x40. Its vaddr is 0, so
    //     `rebase_base` passes it and only the table refuses it.
    let unmapped = Elf::new(0x2000).ph(Phdr::load(0x1000, 0, 0x1000, 0x1000, PF_R | PF_X)).entry(0).build();
    spawn_refused("phdrs_unmapped", &unmapped);

    // 22. This program's own image, a `RELR` table in one copy, which its
    //     `_start` refuses before it writes a word, and a library in another,
    //     which its start refuses before `main`, since it links no loader.
    refuses_itself("carries_relr", DT_RELR, REFUSED_LINE);
    refuses_itself("needs_a_library", DT_NEEDED as u64, NO_LIBRARY_LINE);

    // The kernel heap is intact: allocate and touch enough to walk it, then
    // prove the real loader still works.
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    for i in 0..256 {
        blocks.push(vec![(i % 251) as u8; 4096]);
    }
    for (i, b) in blocks.iter().enumerate() {
        assert!(b.iter().all(|&x| x == (i % 251) as u8), "kernel heap corrupted block {i}");
    }

    let status = std::process::Command::new("/system/bin/echo")
        .arg("loader still works")
        .status()
        .expect("spawn /system/bin/echo");
    assert!(status.success(), "/system/bin/echo exited {status:?}");

    let _ = fs::remove_dir_all(DIR);
    let _ = fs::remove_dir_all(BIG_DIR);
    println!("crafted ELF derivations rejected, kernel intact");
}

/// Every file-chosen value an executable's start turns into an address or a
/// thread-pointer offset, set to one no image holds: a `RELATIVE` addend, a
/// TPOFF addend, a symbol's `st_value`. Each child dies; the kernel living
/// through them is what the checks at the end of `main` assert.
fn values_are_bounded_by_the_image() {
    // RELATIVE `B + A` with `A = i64::MAX`: `USER_VM_BASE + A` overflowed.
    spawn_dies(
        "relative_addend_past_image",
        &exe_with(&[], &[(0x2000, R_X86_64_RELATIVE, i64::MAX)], None).build(),
    );
    // TPOFF64 against the executable's own 16-byte PT_TLS, `A = i64::MIN`:
    // `tls::Static::tpoff` subtracted with overflow.
    spawn_dies(
        "tpoff_addend_past_tls",
        &exe_with(&[], &[(0x2000, R_X86_64_TPOFF64, i64::MIN)], Some(16)).build(),
    );
    // An export at `0xFFFF_FFFF_FFFF_F000` in an executable that needs a
    // library: the kernel built a map of the exe's exports for the library's
    // slots to bind against, and that map added the value to the load base.
    {
        let dep = "export_dep.so";
        // `.dynstr` at 0x1800 holds "far\0<dep>\0"; the export names the first.
        let name_at = 1 + FAR.len() as u64 + 1;
        let exe = exe_with(&[(DT_NEEDED, name_at)], &[], None)
            .sym(0x1418, 1, (STB_GLOBAL << 4) | STT_FUNC, 1, FAR_VALUE)
            .poke(0x1801, FAR.as_bytes())
            .poke(0x1800 + name_at as usize, dep.as_bytes());
        spawn_dies("export_past_image", &exe.build());
    }

}

const FAR: &str = "far";
const FAR_VALUE: u64 = 0xFFFF_FFFF_FFFF_F000;
const DT_INIT_ARRAY: i64 = 25;
const DT_INIT_ARRAYSZ: i64 = 27;
const STT_FUNC: u8 = 2;


/// An executable spanning `[0, 0x4000)` writable, `PT_DYNAMIC` at 0x1000, its
/// relocations at 0x1200, `.dynsym` at 0x1400 and `.dynstr` after it at 0x1800 — and a
/// 16-byte-aligned `PT_TLS` of `tls` bytes when asked.
fn exe_with(tags: &[(i64, u64)], relas: &[(u64, u64, i64)], tls: Option<u64>) -> Elf {
    let mut all = vec![(DT_SYMTAB, 0x1400), (DT_STRTAB, 0x1800), (DT_STRSZ, 0x100)];
    if !relas.is_empty() {
        all.extend([(DT_RELA, 0x1200), (DT_RELASZ, 24 * relas.len() as u64)]);
    }
    all.extend_from_slice(tags);
    let mut elf = Elf::new(0x4000)
        .ph(Phdr::load(0, 0, 0x4000, 0x4000, PF_R | PF_W))
        .ph(Phdr { kind: PT_DYNAMIC, flags: PF_R, offset: 0x1000, vaddr: 0x1000, filesz: 0x200, memsz: 0x200, align: 8 })
        .entry(0)
        .dynamic(0x1000, &all);
    if let Some(memsz) = tls {
        elf = elf.ph(Phdr { kind: PT_TLS, flags: PF_R, offset: 0x3000, vaddr: 0x3000, filesz: 0, memsz, align: 16 });
    }
    for (i, &(offset, r_type, addend)) in relas.iter().enumerate() {
        elf = elf.rela(0x1200 + 24 * i, offset, r_type, addend);
    }
    elf
}

/// Thread-local references an executable's own start refuses: one to its own
/// symbol whose `S + A` leaves its `PT_TLS`, and one to a library's symbol,
/// which no executable may reach with the initial-exec model.
fn executable_tls_refs_die() {
    spawn_dies("tls_apply_spawn", &exe_tls_ref_past_segment());

    let dep = "tpoff_overflow_dep.so";
    // `.dynstr` at 0x1800 holds "wtls\0<dep>\0".
    let exe = exe_with(&[(DT_NEEDED, 6)], &[(0x2000, (1u64 << 32) | R_X86_64_TPOFF64, i64::MAX)], None)
        .sym(0x1418, 1, (STB_GLOBAL << 4) | STT_TLS, 0, 0)
        .poke(0x1801, b"wtls\0")
        .poke(0x1806, dep.as_bytes());
    spawn_dies("tpoff_overflow_spawn", &exe.build());
}

/// An executable needing a library, whose `.gnu.hash` counts 1000 symbols
/// while the file ends 469 entries and 8 bytes into `.dynsym`, with a
/// `GLOB_DAT` naming symbol 469, the first index past the whole entries.
fn globdat_past_short_dynsym() {
    let dep = "globdat_dep.so";
    // nbuckets 1, symoffset 1000, bloom_size 1, bloom_shift 0, the bloom word
    // and bucket 0: a bucket below `symoffset` makes `symoffset` the count.
    let mut gnu_hash = Vec::new();
    for word in [1u32, 1000, 1, 0, 0, 0, 0] {
        gnu_hash.extend_from_slice(&word.to_le_bytes());
    }
    let exe = exe_with(
        &[(DT_NEEDED, 1), (DT_GNU_HASH, 0x3000)],
        &[(0x2000, (469u64 << 32) | R_X86_64_GLOB_DAT, 0)],
        None,
    )
    .poke(0x1801, dep.as_bytes())
    .poke(0x3000, &gnu_hash);
    spawn_dies("globdat_past_dynsym", &exe.build());
}

/// An executable defining `xtls` (`STT_TLS`, offset 8) in a 0x20-byte `PT_TLS`,
/// with a `TPOFF64` against it whose `S + A` (0x148) leaves the segment.
fn exe_tls_ref_past_segment() -> Vec<u8> {
    Elf::new(0x4000)
        .ph(Phdr::load(0, 0, 0x4000, 0x4000, PF_R | PF_W))
        .ph(Phdr { kind: PT_DYNAMIC, flags: PF_R, offset: 0x1000, vaddr: 0x1000, filesz: 0x200, memsz: 0x200, align: 8 })
        .ph(Phdr { kind: PT_TLS, flags: PF_R, offset: 0x3000, vaddr: 0x3000, filesz: 0, memsz: 0x20, align: 16 })
        .entry(0)
        .dynamic(0x1000, &[
            (DT_SYMTAB, 0x1400), (DT_STRTAB, 0x1800), (DT_STRSZ, 0x100),
            (DT_RELA, 0x1200), (DT_RELASZ, 24),
        ])
        // sym[1] xtls: defined STT_TLS, offset 8 in the block.
        .sym(0x1418, 1, (STB_GLOBAL << 4) | STT_TLS, 1, 8)
        .poke(0x1801, b"xtls\0")
        .rela(0x1200, 0x2000, (1u64 << 32) | R_X86_64_TPOFF64, 0x140)
        .build()
}

/// What `toyos::relocate` writes to its stderr, `fd:2`, when it refuses its own program.
const REFUSED_LINE: &str = "this program's relocations are refused: it cannot start\n";
/// What std's start writes there for a program that names a library and links
/// no loader, as this one links none.
const NO_LIBRARY_LINE: &str = "this program names libraries and links no loader: it cannot start\n";
/// The status it exits with then.
const REFUSED_STATUS: i32 = 127;
/// The argument a refused copy is handed, which its `main` panics on.
const REFUSED_COPY: &str = "--refused-copy";

const DT_DEBUG: u64 = 21;
const DT_RELR: u64 = 36;

/// This program's own file with its `DT_DEBUG` entry's tag made `tag`: a std
/// image as lld linked it, whose dynamic section now names `tag` ahead of its
/// `DT_RELA`.
fn own_image_tagged(tag: u64) -> Vec<u8> {
    let exe = std::env::current_exe().expect("current_exe");
    let mut bytes = fs::read(&exe).unwrap_or_else(|e| panic!("read {exe:?}: {e}"));
    let word = |bytes: &[u8], at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let phoff = word(&bytes, 32) as usize;
    let phnum = usize::from(u16::from_le_bytes([bytes[56], bytes[57]]));
    let dynamic = (0..phnum)
        .map(|i| phoff + i * PH_SIZE)
        .find(|&at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) == PT_DYNAMIC)
        .expect("lld's PT_DYNAMIC");
    let (offset, size) = (word(&bytes, dynamic + 8) as usize, word(&bytes, dynamic + 32) as usize);
    let entry = (offset..offset + size).step_by(16).find(|&at| word(&bytes, at) == DT_DEBUG).expect("lld's DT_DEBUG");
    bytes[entry..entry + 8].copy_from_slice(&tag.to_le_bytes());
    bytes
}

/// A copy of this program tagged `tag` exits [`REFUSED_STATUS`] and says
/// `line` on the stderr it is endowed. Endowed none, it says nothing and
/// exits the same.
fn refuses_itself(name: &str, tag: u64, line: &str) {
    let path = write_file(name, &own_image_tagged(tag));
    let out = Command::new(&path)
        .arg(REFUSED_COPY)
        .output()
        .unwrap_or_else(|e| panic!("{name}: spawn: {e}"));
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(REFUSED_STATUS), "{name}: exited {:?}, said {said:?}", out.status);
    assert_eq!(said, line, "{name}");

    let child = spawn_argv(&path, &format!("{path}\0{REFUSED_COPY}\0"))
        .unwrap_or_else(|e| panic!("{name} without a stderr: spawn gave {e:?}"));
    // SAFETY: the handle `SYS_SPAWN` just answered, held nowhere else.
    let child = unsafe { toyos::process::Process::from_raw(child) };
    let end = child.wait().unwrap_or_else(|e| panic!("{name} without a stderr: wait: {e:?}"));
    assert_eq!(end, End::Exited(REFUSED_STATUS), "{name} without a stderr");
}
