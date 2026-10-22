//! An image whose marked slot's `file` carries one bent byte and whose other
//! slot carries the same image whole: the loader refuses the marked slot by
//! the section that file is, before it exits boot services, and boots the
//! other. A boot that falls back, rather than one with nothing to boot, is the
//! only refusal a metal bench comes back from, since a loader with no slot
//! powers the machine off.

use std::io::{Seek, SeekFrom, Write};

use toyos_build::build::Parts;
use toyos_build::image::{self, SecondSlot, Signing};

/// The bytes of `parts` as such an image, its slot A's `file` (a
/// `toyos_update::slots::*_FILE`) bent.
pub fn bent_fallback(
    arch: toyos_build::arch::Arch,
    parts: &Parts,
    params: &str,
    signing: Signing<'_>,
    file: &str,
) -> Vec<u8> {
    let userland = &parts.userland;
    let room = SecondSlot { root_bytes: userland.root.len() as u64 };
    let disk = image::create_boot_image(arch, &parts.bootloader, userland.release(&parts.kernel), params, signing, Some(room));
    let scratch = toyos_tmpdir::TempDir::new("bent-slot");
    let path = scratch.join("disk.img");
    std::fs::write(&path, &disk).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&path).expect("the image just written");
    let table = image::slot_table_of(&mut f).expect("the image's slot table");
    let (a, b) = (table.slots[0].expect("slot A"), table.slots[1].expect("slot B"));
    let extent = |f: &mut std::fs::File, guid| image::partition_extent(f, guid).expect("a slot's partition");
    let (a_boot, b_boot) = (extent(&mut f, a.boot), extent(&mut f, b.boot));
    let (a_root, b_root) = (extent(&mut f, a.root), extent(&mut f, b.root));
    assert_eq!(a_boot.1, b_boot.1, "the two slots' volumes are one size");
    assert!(b_root.1 >= userland.root.len() as u64, "slot B's ROOT holds slot A's");

    // Slot B: slot A's volume and ROOT, byte for byte, so it verifies under
    // the one signed header both carry.
    let mut copy = |from: (u64, u64), to: u64| {
        let mut bytes = vec![0u8; from.1 as usize];
        std::io::Read::read_exact(&mut (&disk[from.0 as usize..]), &mut bytes).expect("inside the image");
        f.seek(SeekFrom::Start(to)).and_then(|_| f.write_all(&bytes)).expect("write slot B");
    };
    copy(a_boot, b_boot.0);
    copy((a_root.0, userland.root.len() as u64), b_root.0);

    // Slot A: one byte of the file bent where it sits on the volume, found by
    // a run of its own bytes that occurs once on that volume.
    let carried = image::read_file_on(&mut f, a.boot, file).expect("slot A carries the file");
    let volume = &disk[a_boot.0 as usize..(a_boot.0 + a_boot.1) as usize];
    let once = |probe: &[u8]| {
        let mut hits = volume.windows(probe.len()).enumerate().filter(|(_, w)| *w == probe).map(|(i, _)| i);
        match (hits.next(), hits.next()) {
            (Some(at), None) => Some(at),
            _ => None,
        }
    };
    let at = (1..16)
        .map(|sixteenth| carried.len() * sixteenth / 16)
        .find_map(|from| once(&carried[from..from + 64]))
        .unwrap_or_else(|| panic!("no run of {file}'s bytes occurs once on slot A's volume"));
    let byte = a_boot.0 + at as u64;
    f.seek(SeekFrom::Start(byte)).and_then(|_| f.write_all(&[volume[at] ^ 1])).expect("bend slot A's file");
    drop(f);

    let bent = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut reread = std::fs::File::open(&path).expect("the bent image");
    assert_ne!(image::read_file_on(&mut reread, a.boot, file).expect("still a file"), carried, "{file} was not bent");
    assert_eq!(image::read_file_on(&mut reread, b.boot, file).expect("slot B's copy"), carried, "slot B's copy was bent");
    bent
}
