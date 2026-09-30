//! What `dladdr` reads of a loaded image: whether an address lies in it, where
//! its first mapped byte is, and which of its dynamic symbols holds the
//! address, chosen as glibc's `dladdr` chooses. It reads nothing but the image
//! it is handed, so the host tests it on an image it lays out
//! (`toyos-libc-copies`).
//!
//! The loader relocates nothing in `PT_DYNAMIC`, so every address the dynamic
//! table holds is relative to the image's base.

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PHDR_SIZE: usize = 56;

const DT_NULL: u64 = 0;
const DT_HASH: u64 = 4;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_STRSZ: u64 = 10;
const DT_GNU_HASH: u64 = 0x6fff_fef5;

const SYM_SIZE: usize = 24;
const SHN_UNDEF: u16 = 0;
const SHN_ABS: u16 = 0xfff1;
const STB_GLOBAL: u8 = 1;
const STB_WEAK: u8 = 2;
const STT_TLS: u8 = 6;
const STV_INTERNAL: u8 = 1;
const STV_HIDDEN: u8 = 2;

/// A loaded image, as `dl_iterate_phdr` describes it.
#[derive(Clone, Copy)]
pub(crate) struct Image {
    pub(crate) base: u64,
    pub(crate) phdr: *const u8,
    pub(crate) phnum: usize,
}

/// The fields of a program header this module reads.
struct Segment {
    kind: u32,
    vaddr: u64,
    memsz: u64,
}

impl Image {
    /// # Safety
    /// `phdr` points at `phnum` readable program headers.
    unsafe fn segments(self) -> impl Iterator<Item = Segment> {
        (0..self.phnum).map(move |i| {
            // SAFETY: the caller's.
            let p = unsafe { self.phdr.add(i * PHDR_SIZE) };
            unsafe { Segment { kind: read(p, 0), vaddr: read(p, 16), memsz: read(p, 40) } }
        })
    }

    /// Whether a `PT_LOAD` segment of the image holds `addr`.
    ///
    /// # Safety
    /// As [`Image::segments`].
    pub(crate) unsafe fn contains(self, addr: u64) -> bool {
        unsafe { self.segments() }.any(|s| s.kind == PT_LOAD && addr.wrapping_sub(self.base + s.vaddr) < s.memsz)
    }

    /// The image's first mapped byte: the page its lowest `PT_LOAD` starts in.
    ///
    /// # Safety
    /// As [`Image::segments`].
    pub(crate) unsafe fn start(self) -> u64 {
        let low = unsafe { self.segments() }.filter(|s| s.kind == PT_LOAD).map(|s| s.vaddr).min().unwrap_or(0);
        self.base + (low & !0xfff)
    }

    /// The name and address of the dynamic symbol of the image that holds
    /// `addr`, as glibc chooses one: exported (global or weak, neither hidden
    /// nor internal), no thread-local, defined in a section, with `addr` in
    /// `[value, value + size)` or at `value` for a size of 0; of several, the
    /// highest value.
    ///
    /// # Safety
    /// As [`Image::segments`], and every table the image's `PT_DYNAMIC` names
    /// is mapped.
    pub(crate) unsafe fn symbol(self, addr: u64) -> Option<(*const u8, u64)> {
        let dynamic = unsafe { self.segments() }.find(|s| s.kind == PT_DYNAMIC)?;
        let at = |vaddr: u64| (self.base + vaddr) as *const u8;
        let (mut symtab, mut strtab, mut strsz, mut hash, mut gnu_hash) = (None, None, 0, None, None);
        let mut entry = at(dynamic.vaddr);
        loop {
            let (tag, value): (u64, u64) = unsafe { (read(entry, 0), read(entry, 8)) };
            match tag {
                DT_NULL => break,
                DT_SYMTAB => symtab = Some(value),
                DT_STRTAB => strtab = Some(value),
                DT_STRSZ => strsz = value,
                DT_HASH => hash = Some(value),
                DT_GNU_HASH => gnu_hash = Some(value),
                _ => {}
            }
            entry = unsafe { entry.add(16) };
        }
        let (symtab, strtab) = (symtab?, strtab?);
        let count = match (gnu_hash, hash) {
            (Some(table), _) => unsafe { gnu_hash_count(at(table)) },
            (None, Some(table)) => unsafe { read::<u32>(at(table), 4) as usize },
            // Adjacent in every layout a linker makes, as the loader also reads them.
            (None, None) => strtab.saturating_sub(symtab) as usize / SYM_SIZE,
        };
        let mut best: Option<(u64, u32)> = None;
        for i in 0..count {
            let sym = unsafe { at(symtab).add(i * SYM_SIZE) };
            let (name, info, other, shndx, value, size): (u32, u8, u8, u16, u64, u64) =
                unsafe { (read(sym, 0), read(sym, 4), read(sym, 5), read(sym, 6), read(sym, 8), read(sym, 16)) };
            let (bind, visibility) = (info >> 4, other & 3);
            let exported = (bind == STB_GLOBAL || bind == STB_WEAK) && visibility != STV_HIDDEN && visibility != STV_INTERNAL;
            if !exported || info & 0xf == STT_TLS || shndx == SHN_UNDEF || shndx == SHN_ABS || u64::from(name) >= strsz {
                continue;
            }
            let start = self.base + value;
            let holds = addr >= start && (addr - start < size || (size == 0 && addr == start));
            if holds && best.is_none_or(|(v, _)| v < value) {
                best = Some((value, name));
            }
        }
        best.map(|(value, name)| (unsafe { at(strtab).add(name as usize) }, self.base + value))
    }
}

/// How many symbols the `.gnu.hash` table at `table` covers: through the end
/// of the chain the highest bucket starts, or up to its first hashed symbol
/// when every bucket is empty.
unsafe fn gnu_hash_count(table: *const u8) -> usize {
    let (nbuckets, symoffset, bloom_words): (u32, u32, u32) = unsafe { (read(table, 0), read(table, 4), read(table, 8)) };
    let (nbuckets, symoffset) = (nbuckets as usize, symoffset as usize);
    let buckets = unsafe { table.add(16 + bloom_words as usize * 8) };
    let chains = unsafe { buckets.add(nbuckets * 4) };
    let Some(mut i) = (0..nbuckets).map(|b| unsafe { read::<u32>(buckets, b * 4) as usize }).filter(|&s| s != 0).max() else {
        return symoffset;
    };
    // A chain's last hash has its low bit set.
    while unsafe { read::<u32>(chains, (i - symoffset) * 4) } & 1 == 0 {
        i += 1;
    }
    i + 1
}

/// # Safety
/// `at + offset` holds a `T`, however aligned.
unsafe fn read<T: Copy>(at: *const u8, offset: usize) -> T {
    unsafe { at.add(offset).cast::<T>().read_unaligned() }
}
