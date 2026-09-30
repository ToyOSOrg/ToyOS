//! `dladdr`'s search on an image laid out here, with each way a table says how
//! many symbols it has: `.gnu.hash`, `DT_HASH`, and neither. The answers are
//! glibc's `dladdr` rule (`elf/dl-addr.c`): an exported symbol, no TLS,
//! defined in a section, holding the address in `[value, value + size)` or at
//! `value` when its size is 0, the highest value of several.

use crate::elfsym::Image;

const SIZE: usize = 0x2000;
const PHDRS: usize = 0x40;
const DYNAMIC: usize = 0x100;
const SYMTAB: usize = 0x200;
const STRTAB: usize = 0x400;
const HASH: usize = 0x500;

const GLOBAL: u8 = 1 << 4;
const WEAK: u8 = 2 << 4;
const FUNC: u8 = 2;
const OBJECT: u8 = 1;
const TLS: u8 = 6;
const HIDDEN: u8 = 2;
const ABS: u16 = 0xfff1;

/// Name, `st_info`, `st_other`, `st_shndx`, value, size; index 0 is the null
/// symbol, and 1 the one local, which a `.gnu.hash` leaves unhashed.
const SYMBOLS: &[(&str, u8, u8, u16, u64, u64)] = &[
    ("", 0, 0, 0, 0, 0),
    ("local", FUNC, 0, 1, 0x1000, 0x800),
    ("func", GLOBAL | FUNC, 0, 1, 0x1100, 0x100),
    ("big", GLOBAL | FUNC, 0, 1, 0x1000, 0x300),
    ("weak", WEAK | FUNC, 0, 1, 0x1200, 0x80),
    ("mark", GLOBAL, 0, 1, 0x1280, 0),
    ("hidden", GLOBAL | FUNC, HIDDEN, 1, 0x1300, 0x40),
    ("tls", GLOBAL | TLS, 0, 1, 0x1340, 0x40),
    ("undef", GLOBAL | FUNC, 0, 0, 0, 0),
    ("abs", GLOBAL, 0, ABS, 0x1400, 0x40),
    ("last", GLOBAL | OBJECT, 0, 1, 0x1500, 0x10),
];

#[derive(Clone, Copy)]
enum Count {
    GnuHash,
    SysvHash,
    Gap,
}

/// An image at the start of an 8-aligned buffer, every table addressed from
/// it, `.dynamic` naming the symbols as `count` says.
fn image(count: Count) -> Vec<u64> {
    let mut bytes = vec![0u8; SIZE];
    let mut put = |at: usize, value: &[u8]| bytes[at..at + value.len()].copy_from_slice(value);
    // PT_LOAD over the whole image, then PT_DYNAMIC.
    put(PHDRS, &1u32.to_le_bytes());
    put(PHDRS + 32, &(SIZE as u64).to_le_bytes());
    put(PHDRS + 40, &(SIZE as u64).to_le_bytes());
    put(PHDRS + 56, &2u32.to_le_bytes());
    put(PHDRS + 56 + 16, &(DYNAMIC as u64).to_le_bytes());
    let mut names = vec![0u8];
    for (i, (name, info, other, shndx, value, size)) in SYMBOLS.iter().enumerate() {
        let at = SYMTAB + i * 24;
        let offset = if name.is_empty() { 0 } else { names.len() as u32 };
        if !name.is_empty() {
            names.extend_from_slice(name.as_bytes());
            names.push(0);
        }
        put(at, &offset.to_le_bytes());
        put(at + 4, &[*info, *other]);
        put(at + 6, &shndx.to_le_bytes());
        put(at + 8, &value.to_le_bytes());
        put(at + 16, &size.to_le_bytes());
    }
    put(STRTAB, &names);
    let symbols = SYMBOLS.len() as u32;
    let mut tags = vec![(6u64, SYMTAB as u64), (5, STRTAB as u64), (10, names.len() as u64)];
    match count {
        Count::GnuHash => {
            // One bucket, one bloom word, every symbol from 2 hashed in one
            // chain whose last value has its low bit set.
            let (nbuckets, symoffset, bloom_words) = (1u32, 2u32, 1u32);
            put(HASH, &nbuckets.to_le_bytes());
            put(HASH + 4, &symoffset.to_le_bytes());
            put(HASH + 8, &bloom_words.to_le_bytes());
            put(HASH + 12, &6u32.to_le_bytes());
            put(HASH + 16, &u64::MAX.to_le_bytes());
            put(HASH + 24, &symoffset.to_le_bytes());
            for i in symoffset..symbols {
                let last = u32::from(i == symbols - 1);
                put(HASH + 28 + (i - symoffset) as usize * 4, &((i << 1) | last).to_le_bytes());
            }
            tags.push((0x6fff_fef5, HASH as u64));
        }
        Count::SysvHash => {
            put(HASH, &1u32.to_le_bytes());
            put(HASH + 4, &symbols.to_le_bytes());
            tags.push((4, HASH as u64));
        }
        Count::Gap => {}
    }
    for (i, (tag, value)) in tags.into_iter().chain([(0, 0)]).enumerate() {
        put(DYNAMIC + i * 16, &tag.to_le_bytes());
        put(DYNAMIC + i * 16 + 8, &value.to_le_bytes());
    }
    bytes.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect()
}

fn loaded(words: &[u64]) -> Image {
    let base = words.as_ptr() as u64;
    Image { base, phdr: (base as usize + PHDRS) as *const u8, phnum: 2 }
}

/// The symbol `dladdr` names for `offset` into the image, by name, with the
/// offset of its value.
fn named(image: Image, offset: u64) -> Option<(String, u64)> {
    // SAFETY: the image is the buffer `image` laid out, whole.
    let (name, at) = unsafe { image.symbol(image.base + offset) }?;
    // SAFETY: a name in the image's string table, NUL-terminated there.
    let name = unsafe { std::ffi::CStr::from_ptr(name.cast()) };
    Some((name.to_str().unwrap().to_string(), at - image.base))
}

#[test]
fn every_count_finds_glibcs_symbol() {
    for count in [Count::GnuHash, Count::SysvHash, Count::Gap] {
        let words = image(count);
        let image = loaded(&words);
        for (offset, want) in [
            (0x1000, Some(("big", 0x1000))),
            (0x1150, Some(("func", 0x1100))),
            (0x1210, Some(("weak", 0x1200))),
            (0x1280, Some(("mark", 0x1280))),
            (0x1281, Some(("big", 0x1000))),
            (0x1310, None),
            (0x1350, None),
            (0x1410, None),
            (0x1505, Some(("last", 0x1500))),
            (0x1510, None),
            (0x0800, None),
        ] {
            let want = want.map(|(n, v)| (n.to_string(), v));
            assert_eq!(named(image, offset), want, "offset {offset:#x}");
        }
    }
}

#[test]
fn an_image_holds_its_load_segment_and_starts_at_its_page() {
    let words = image(Count::GnuHash);
    let image = loaded(&words);
    let base = image.base;
    // SAFETY: the image's program headers, in the buffer.
    unsafe {
        assert!(image.contains(base) && image.contains(base + SIZE as u64 - 1));
        assert!(!image.contains(base + SIZE as u64) && !image.contains(base - 1));
        assert_eq!(image.start(), base);
    }
}
