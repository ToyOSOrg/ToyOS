//! `dladdr`'s walk of an image laid out here: the segment holding an address,
//! the image's first byte, and the tables it hands `toyos-elf`'s choice, as
//! many symbols as `.gnu.hash` describes, or with none as the gap to the
//! string table holds.

use crate::elfsym::Image;

const SIZE: usize = 0x2000;
const PHDRS: usize = 0x40;
const DYNAMIC: usize = 0x100;
const SYMTAB: usize = 0x200;
const STRTAB: usize = 0x400;
/// Below the symbols, so that only the `.gnu.hash` counts them.
const STRTAB_FIRST: usize = 0x180;
const HASH: usize = 0x500;

const GLOBAL_FUNC: u8 = (1 << 4) | 2;
const LOCAL_FUNC: u8 = 2;

/// Name, `st_info`, value, size; index 0 is the null symbol, and 1 the one
/// local, which a `.gnu.hash` leaves unhashed.
const SYMBOLS: &[(&str, u8, u64, u64)] = &[
    ("", 0, 0, 0),
    ("local", LOCAL_FUNC, 0x1000, 0x800),
    ("func", GLOBAL_FUNC, 0x1100, 0x100),
    ("big", GLOBAL_FUNC, 0x1000, 0x300),
    ("last", GLOBAL_FUNC, 0x1500, 0x10),
];

/// An image at the start of an 8-aligned buffer, every table addressed from
/// it, its symbols counted by a `.gnu.hash` or else by the gap to the string
/// table.
fn image(hashed: bool) -> Vec<u64> {
    let mut bytes = vec![0u8; SIZE];
    let mut put = |at: usize, value: &[u8]| bytes[at..at + value.len()].copy_from_slice(value);
    // PT_LOAD over the whole image, then PT_DYNAMIC.
    put(PHDRS, &1u32.to_le_bytes());
    put(PHDRS + 32, &(SIZE as u64).to_le_bytes());
    put(PHDRS + 40, &(SIZE as u64).to_le_bytes());
    put(PHDRS + 56, &2u32.to_le_bytes());
    put(PHDRS + 56 + 16, &(DYNAMIC as u64).to_le_bytes());
    put(PHDRS + 56 + 40, &0x100u64.to_le_bytes());
    let mut names = vec![0u8];
    for (i, (name, info, value, size)) in SYMBOLS.iter().enumerate() {
        let at = SYMTAB + i * 24;
        let offset = if name.is_empty() { 0 } else { names.len() as u32 };
        if !name.is_empty() {
            names.extend_from_slice(name.as_bytes());
            names.push(0);
        }
        put(at, &offset.to_le_bytes());
        put(at + 4, &[*info]);
        put(at + 6, &1u16.to_le_bytes());
        put(at + 8, &value.to_le_bytes());
        put(at + 16, &size.to_le_bytes());
    }
    let strtab = if hashed { STRTAB_FIRST } else { STRTAB };
    put(strtab, &names);
    let mut tags = vec![(6u64, SYMTAB as u64), (5, strtab as u64), (10, names.len() as u64)];
    if hashed {
        // One bucket, one bloom word, every symbol from 2 hashed in one chain
        // whose last value has its low bit set.
        let (symbols, symoffset) = (SYMBOLS.len() as u32, 2u32);
        for (i, word) in [1u32, symoffset, 1, 6].into_iter().enumerate() {
            put(HASH + i * 4, &word.to_le_bytes());
        }
        put(HASH + 16, &u64::MAX.to_le_bytes());
        put(HASH + 24, &symoffset.to_le_bytes());
        for i in symoffset..symbols {
            let last = u32::from(i == symbols - 1);
            put(HASH + 28 + (i - symoffset) as usize * 4, &((i << 1) | last).to_le_bytes());
        }
        tags.push((0x6fff_fef5, HASH as u64));
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

#[test]
fn either_count_reaches_the_symbol_at_its_address() {
    for hashed in [true, false] {
        let words = image(hashed);
        let image = loaded(&words);
        for (offset, want) in [
            (0x1000, Some(("big", 0x1000))),
            (0x1150, Some(("func", 0x1100))),
            (0x1505, Some(("last", 0x1500))),
            (0x1510, None),
            (0x0800, None),
        ] {
            // SAFETY: the image is the buffer `image` laid out, whole.
            let got = unsafe { image.symbol(image.base + offset) }
                .map(|(name, at)| (name.to_str().unwrap(), at - image.base));
            assert_eq!(got, want, "offset {offset:#x}, hashed {hashed}");
        }
    }
}

#[test]
fn an_image_holds_its_load_segment_and_starts_at_its_page() {
    let words = image(true);
    let image = loaded(&words);
    let base = image.base;
    // SAFETY: the image's program headers, in the buffer.
    unsafe {
        assert!(image.contains(base) && image.contains(base + SIZE as u64 - 1));
        assert!(!image.contains(base + SIZE as u64) && !image.contains(base - 1));
        assert_eq!(image.start(), base);
    }
}
