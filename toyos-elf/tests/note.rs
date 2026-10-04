//! `PT_NOTE` and the build-id in it, against crafted notes and against
//! `object`'s note reader, which is not ToyOS code.

#[allow(dead_code)]
mod common;

use common::*;
use object::elf::{FileHeader64, NT_GNU_BUILD_ID as OBJECT_NT_GNU_BUILD_ID};
use object::read::elf::NoteIterator;
use object::LittleEndian;
use toyos_elf::note::{self, NoteSegment, MAX_BUILD_ID, NT_GNU_BUILD_ID};

const PT_NOTE: u32 = 4;
const OTHER: u32 = 1;

/// One note: `name` with its NUL, `desc`, each padded to `align`.
fn note(name: &[u8], kind: u32, desc: &[u8], align: usize) -> Vec<u8> {
    let pad = |v: &mut Vec<u8>| v.resize(v.len().next_multiple_of(align), 0);
    let mut out = Vec::new();
    out.extend((name.len() as u32).to_le_bytes());
    out.extend((desc.len() as u32).to_le_bytes());
    out.extend(kind.to_le_bytes());
    out.extend(name);
    pad(&mut out);
    out.extend(desc);
    pad(&mut out);
    out
}

fn gnu_id(desc: &[u8], align: usize) -> Vec<u8> {
    note(b"GNU\0", NT_GNU_BUILD_ID, desc, align)
}

/// `object`'s answer for the same bytes: the first note it reads named `GNU`
/// of type `NT_GNU_BUILD_ID`.
fn objects(notes: &[u8], align: u64) -> Option<Vec<u8>> {
    let mut iter = NoteIterator::<FileHeader64<LittleEndian>>::new(LittleEndian, align, notes).ok()?;
    while let Ok(Some(n)) = iter.next() {
        if n.name() == b"GNU" && n.n_type(LittleEndian) == OBJECT_NT_GNU_BUILD_ID {
            return Some(n.desc().to_vec());
        }
    }
    None
}

/// Ours, and `object`'s on the same bytes, which must agree.
fn agreed(notes: &[u8], align: u64) -> Option<Vec<u8>> {
    let ours = note::build_id(notes, align).map(<[u8]>::to_vec);
    assert_eq!(ours, objects(notes, align), "toyos-elf and object disagree on {notes:02x?}");
    ours
}

const ID: [u8; 20] = [0xab; 20];

#[test]
fn the_constant_is_the_gabis() {
    assert_eq!(NT_GNU_BUILD_ID, OBJECT_NT_GNU_BUILD_ID);
}

#[test]
fn a_lone_build_id_is_read() {
    assert_eq!(agreed(&gnu_id(&ID, 4), 4), Some(ID.to_vec()));
}

#[test]
fn no_build_id_note_names_none() {
    assert_eq!(agreed(&note(b"GNU\0", OTHER, &[1, 2, 3, 4], 4), 4), None);
    assert_eq!(agreed(&[], 4), None);
}

#[test]
fn a_build_id_under_another_name_is_none() {
    assert_eq!(agreed(&note(b"GNV\0", NT_GNU_BUILD_ID, &ID, 4), 4), None);
}

#[test]
fn a_build_id_of_the_wrong_type_is_none() {
    assert_eq!(agreed(&note(b"GNU\0", OTHER, &ID, 4), 4), None);
}

#[test]
fn a_descriptor_of_0_or_past_32_bytes_is_no_build_id() {
    assert_eq!(note::build_id(&gnu_id(&[], 4), 4), None);
    assert_eq!(note::build_id(&gnu_id(&[7; MAX_BUILD_ID + 1], 4), 4), None);
    assert_eq!(agreed(&gnu_id(&[7; MAX_BUILD_ID], 4), 4), Some(vec![7; MAX_BUILD_ID]));
    assert_eq!(agreed(&gnu_id(&[7], 4), 4), Some(vec![7]));
}

/// A first note whose name is not a multiple of four: its padding is what puts
/// the second note where it is.
#[test]
fn the_build_id_after_an_unpadded_name_is_found() {
    let mut notes = note(b"abcde\0", OTHER, &[9; 5], 4);
    notes.extend(gnu_id(&ID, 4));
    assert_eq!(agreed(&notes, 4), Some(ID.to_vec()));
}

#[test]
fn an_eight_aligned_segment_pads_to_eight() {
    let mut notes = note(b"abcdef\0", OTHER, &[9; 5], 8);
    notes.extend(gnu_id(&ID, 8));
    assert_eq!(agreed(&notes, 8), Some(ID.to_vec()));
    // The same bytes read at four find no build-id where the note is.
    assert_ne!(note::build_id(&notes, 4), Some(&ID[..]));
}

#[test]
fn an_alignment_no_note_section_has_names_none() {
    assert_eq!(note::build_id(&gnu_id(&ID, 4), 16), None);
}

#[test]
fn a_note_running_past_its_segment_names_none() {
    let notes = gnu_id(&ID, 4);
    for cut in 0..notes.len() {
        assert_eq!(note::build_id(&notes[..cut], 4), None, "cut at {cut}");
    }
    let mut lying = gnu_id(&ID, 4);
    lying[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(note::build_id(&lying, 4), None);
    lying[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(note::build_id(&lying, 4), None);
}

#[test]
fn the_first_of_two_build_ids_is_the_answer() {
    let mut notes = gnu_id(&ID, 4);
    notes.extend(gnu_id(&[1; 20], 4));
    assert_eq!(agreed(&notes, 4), Some(ID.to_vec()));
}

#[test]
fn every_pt_note_is_listed_wherever_its_bytes_are() {
    let bytes = Elf::honest(0x3000)
        .ph(Phdr { kind: PT_NOTE, flags: PF_R, offset: 0x2000, vaddr: 0x2000, filesz: 0x24, memsz: 0x24, align: 4 })
        .ph(Phdr { kind: PT_NOTE, flags: PF_R, offset: 0x300, vaddr: 0x300, filesz: 0x10, memsz: 0x10, align: 8 })
        .build();
    let listed: Vec<NoteSegment> = note::segments(&bytes).collect();
    assert_eq!(
        listed,
        [
            NoteSegment { offset: 0x2000, filesz: 0x24, align: 4 },
            NoteSegment { offset: 0x300, filesz: 0x10, align: 8 },
        ]
    );
}

#[test]
fn a_header_with_no_readable_table_lists_no_note() {
    assert_eq!(note::segments(&[]).count(), 0);
    let bytes = Elf::honest(0x1000).phoff(0x2000).build();
    assert_eq!(note::segments(&bytes).count(), 0);
}
