//! Naming a recorded offset out of the file the record names: against a real
//! binary, against `object`'s symbol table reader (not ToyOS code), and
//! against every build-id disagreement.

use object::{Object, ObjectSymbol, SymbolKind};
use toyos_symbols::frame::BuildId;
use toyos_symbols::{name, Unnamed};

const BINARY: &[u8] = include_bytes!("fixtures/input-test.bin");

/// `input-test` with its `PT_GNU_EH_FRAME` turned into a `PT_NOTE` over a
/// build-id note appended past its end, far past its header page.
fn with_build_id(id: &[u8]) -> Vec<u8> {
    let mut file = BINARY.to_vec();
    let at = file.len().next_multiple_of(4);
    file.resize(at, 0);
    file.extend(4u32.to_le_bytes());
    file.extend((id.len() as u32).to_le_bytes());
    file.extend(3u32.to_le_bytes());
    file.extend(b"GNU\0");
    file.extend(id);
    // `readelf -l`: the fourth program header is GNU_EH_FRAME.
    let ph = 64 + 3 * 56;
    assert_eq!(file[ph..ph + 4], 0x6474_e550u32.to_le_bytes());
    file[ph..ph + 4].copy_from_slice(&4u32.to_le_bytes());
    file[ph + 8..ph + 16].copy_from_slice(&(at as u64).to_le_bytes());
    file[ph + 32..ph + 40].copy_from_slice(&((16 + id.len()) as u64).to_le_bytes());
    file[ph + 48..ph + 56].copy_from_slice(&4u64.to_le_bytes());
    file
}

#[test]
fn a_real_binarys_offset_names_its_function() {
    // `readelf --syms`: `3902: 00000000000013a0  55 FUNC GLOBAL DEFAULT 1 main`
    assert_eq!(name(BINARY, 0x13a0 + 10, None), Ok(("main", 10)));
}

/// Every function `object` reads out of the same file names one of the
/// functions `object` puts at its address, at its middle byte.
#[test]
fn every_function_names_what_object_reads() {
    let file = object::File::parse(BINARY).expect("object reads input-test");
    let functions: Vec<_> = file
        .symbols()
        .filter(|s| s.kind() == SymbolKind::Text && s.size() > 0 && s.address() > 0)
        .collect();
    for sym in &functions {
        let at = sym.address() + sym.size() / 2;
        let ours = name(BINARY, at, None);
        let at_address: Vec<&str> =
            functions.iter().filter(|o| o.address() == sym.address()).filter_map(|o| o.name().ok()).collect();
        assert!(
            matches!(ours, Ok((n, within)) if at_address.contains(&n) && within == at - sym.address()),
            "{at:#x}: ours {ours:?}, object's {at_address:?}"
        );
    }
    assert!(functions.len() > 1000, "only {} functions", functions.len());
}

#[test]
fn the_build_the_record_names_is_the_one_named() {
    let id = [0x5a; 20];
    let file = with_build_id(&id);
    let id = BuildId::new(&id).unwrap();
    assert_eq!(BuildId::find(&file, |s| file.get(s.offset as usize..(s.offset + s.filesz) as usize)), Some(id));
    assert_eq!(name(&file, 0x13a0, Some(&id)), Ok(("main", 0)));
}

#[test]
fn another_build_is_refused_by_name() {
    let file = with_build_id(&[0x5a; 20]);
    let carried = BuildId::new(&[0x5a; 20]);
    let other = BuildId::new(&[0x5b; 20]).unwrap();
    assert_eq!(name(&file, 0x13a0, Some(&other)), Err(Unnamed::OtherBuild { file: carried }));
    assert_eq!(name(&file, 0x13a0, None), Err(Unnamed::OtherBuild { file: carried }));
    assert_eq!(name(BINARY, 0x13a0, Some(&other)), Err(Unnamed::OtherBuild { file: None }));
}

#[test]
fn a_file_that_cannot_name_says_why() {
    assert!(matches!(name(&BINARY[..32], 0, None), Err(Unnamed::NotElf(_))));
    assert!(matches!(name(&[], 0, None), Err(Unnamed::NotElf(_))));
    // Past its section header table.
    assert_eq!(name(&BINARY[..0x1000], 0x13a0, None), Err(Unnamed::NoSymbols));
    assert_eq!(name(BINARY, 0, None), Err(Unnamed::NoSymbol));
}

/// Seeded corruptions of the real file's headers and tables: none panics the
/// namer.
#[test]
fn no_corrupted_file_panics_the_namer() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let (_, strtab) = toyos_symbols::locate(BINARY).unwrap();
    let shoff = u64::from_le_bytes(BINARY[40..48].try_into().unwrap()) as usize;
    let strtab_at = BINARY.windows(strtab.len().min(64)).position(|w| w == &strtab[..w.len()]).unwrap();
    // The file header, the program headers, the section headers and the first
    // symbols: every structure the namer reads before a name.
    let regions = [0..0x190, shoff..BINARY.len(), strtab_at - 0x400..strtab_at + 0x400];
    let mut file = with_build_id(&[1; 20]);
    let id = BuildId::new(&[1; 20]).unwrap();
    let pristine = file.clone();
    for _ in 0..3000 {
        file.copy_from_slice(&pristine);
        for _ in 0..1 + next() % 8 {
            let region = &regions[(next() % regions.len() as u64) as usize];
            let at = region.start + (next() as usize) % (region.end - region.start);
            file[at] = next() as u8;
        }
        let cut = if next() % 4 == 0 { (next() as usize) % file.len() } else { file.len() };
        let _ = name(&file[..cut], next() % 0x2_0000, Some(&id));
    }
}
