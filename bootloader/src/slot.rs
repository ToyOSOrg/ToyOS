//! Which slot this boot hands the machine to, and every byte of it held to the
//! owner's signature before it is.
//!
//! The slot table marks one slot; that one is tried first and the other only
//! when the marked one is refused (`toyos_update::policy::order`). A slot is
//! booted only once, in this order: its signed header is on its FAT
//! partition, the signature is [`KEY`]'s, its image did not die on its last
//! boot, its version is at or above the floor a boot has proven, and its
//! kernel, boot parameter and ROOT each hash to what the header names. Each
//! refusal is said by name, and the boot that falls back tells the kernel
//! which slot it refused and why, so the next boot's log carries it.
//!
//! **A death is the one refusal a slot comes back from**: where no slot
//! verifies but one whose image died, that one boots, and says so — a machine
//! with one slot, or with two that both died, boots what it has rather than
//! nothing, which is what it did before it had slots.
//!
//! **The loader is the part signing does not cover**: it reads the key it
//! checks against out of its own binary, and only UEFI Secure Boot over the
//! loader closes that. Until then a writable ESP is the gap.

use alloc::string::String;
use alloc::vec::Vec;

use toyos_update::image::{Header, SIGNATURE_BYTES, SIGNED_BYTES};
use toyos_update::policy::{self, Refusal};
use toyos_update::record::{self, Record};
use toyos_update::slots::{self, Slot, Which};
use toyos_update::{sig, Digest};
use uefi::prelude::*;
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::CString16;

use crate::rootimage::{Disk, RootImage};

/// The owner's public key, embedded at build time: the only key this loader
/// boots an image under.
pub const KEY: [u8; 32] = sig::key_from_hex(env!("TOYOS_IMAGE_KEY"));

/// The head of every line this module writes.
const HEAD: &str = "Slot";

/// A slot every byte of which its signature vouches for.
pub struct Chosen {
    pub which: Which,
    pub version: u64,
    /// The SHA-256 of its signed header, which names the image exactly.
    pub digest: Digest,
    pub kernel: Vec<u8>,
    pub cmdline: Vec<u8>,
    pub root: RootImage,
    /// The slot tried before this one, and why it was refused.
    pub refused: Option<(Which, Refusal)>,
}

/// The slot to boot, or every refusal and why there is nothing to boot.
pub fn choose(
    handle: Handle,
    system_table: &SystemTable<Boot>,
    floor: u64,
    record: &Record,
) -> Result<Chosen, String> {
    let bs = system_table.boot_services();
    let disk = crate::rootimage::boot_disk(handle, bs)?;
    let mut disk = Disk::open(bs, disk)?;
    let table = disk.slot_table()?;
    println!(
        "{HEAD}s: the table marks {} (sequence {}); slot A {}, slot B {}; the floor is {floor}",
        table.marked.letter(),
        table.sequence,
        if table.slots[0].is_some() { "present" } else { "absent" },
        if table.slots[1].is_some() { "present" } else { "absent" },
    );
    let mut refused: Option<(Which, Refusal)> = None;
    let mut dead: alloc::vec::Vec<Which> = alloc::vec::Vec::new();
    for which in policy::order(&table).into_iter().flatten() {
        let slot = table.slot(which).expect("`order` names only slots the table carries");
        match verify(bs, &mut disk, which, slot, floor, Some(record)) {
            Ok(mut chosen) => {
                chosen.refused = refused;
                return Ok(chosen);
            }
            Err(why) => {
                println!("{HEAD} {}: REFUSED, {why}", which.letter());
                if why == Refusal::Died {
                    dead.push(which);
                }
                // The marked slot's refusal is the one the kernel is told: it
                // is why this boot is not the one the table asked for.
                refused.get_or_insert((which, why));
            }
        }
    }
    for which in dead {
        let slot = table.slot(which).expect("a slot `order` named");
        println!("{HEAD} {}: no slot verifies but this one, whose image died on its last boot; it boots again", which.letter());
        match verify(bs, &mut disk, which, slot, floor, None) {
            Ok(chosen) => return Ok(chosen),
            Err(why) => println!("{HEAD} {}: REFUSED, {why}", which.letter()),
        }
    }
    Err(alloc::format!(
        "no slot verifies: the marked slot {} was refused ({})",
        table.marked.letter(),
        refused.map_or(String::new(), |(_, why)| alloc::format!("{why}"))
    ))
}

/// `slot`, held to [`KEY`] and the floor, and to the record's deaths where
/// there is a record to hold it to.
fn verify(
    bs: &BootServices,
    disk: &mut Disk<'_>,
    which: Which,
    slot: Slot,
    floor: u64,
    record: Option<&Record>,
) -> Result<Chosen, Refusal> {
    let letter = which.letter();
    // First, because it costs one read of the table and refuses by name a
    // ROOT its own table does not vouch for — one overlapping a neighbour —
    // before a byte of the slot is read.
    let part = disk.locate(slot.root).map_err(|why| {
        println!("{HEAD} {letter}: {why}");
        Refusal::Unreadable("root")
    })?;
    let signed = match read_file(bs, &slot.boot, slots::SIGNED_FILE, SIGNED_BYTES as u64) {
        Ok(bytes) => bytes,
        Err(FileRefused::Missing) => return Err(Refusal::Unsigned),
        Err(FileRefused::Other(why)) => {
            println!("{HEAD} {letter}: {} {why}", slots::SIGNED_FILE);
            return Err(Refusal::Unreadable("signed header"));
        }
    };
    let signed: [u8; SIGNED_BYTES] = signed.as_slice().try_into().map_err(|_| Refusal::Malformed)?;
    let header = Header::parse(&signed).map_err(|why| {
        println!("{HEAD} {letter}: {why}");
        Refusal::Malformed
    })?;
    let header_bytes: &[u8; toyos_update::image::HEADER_BYTES] =
        signed[..toyos_update::image::HEADER_BYTES].try_into().expect("a signed header begins with its header");
    let signature: [u8; SIGNATURE_BYTES] = toyos_update::image::signature_of(&signed);
    sig::verify(&KEY, header_bytes, &signature).map_err(|why| {
        println!("{HEAD} {letter}: {why}");
        Refusal::Signature
    })?;
    let digest = toyos_update::sha256(&signed);
    let mut hex = [0u8; 64];
    println!(
        "{HEAD} {letter}: signed header {} verifies under this loader's key, version {}",
        toyos_update::hex(&digest, &mut hex),
        header.version
    );
    if record.is_some_and(|record| record::died(record, which, &digest)) {
        return Err(Refusal::Died);
    }
    policy::admits(floor, header.version)?;

    let kernel = read_section(bs, &slot.boot, slots::KERNEL_FILE, header.kernel().len, "kernel")?;
    held(&kernel, header.kernel().sha256, "kernel")?;
    let cmdline = read_section(bs, &slot.boot, slots::CMDLINE_FILE, header.cmdline().len, "cmdline")?;
    held(&cmdline, header.cmdline().sha256, "cmdline")?;

    let root = disk.read_root(bs, &part, header.root().len).map_err(|why| {
        println!("{HEAD} {letter}: ROOT: {why}");
        Refusal::Unreadable("root")
    })?;
    let began = crate::tsc();
    let root_hash = toyos_update::sha256(root.bytes());
    println!("{HEAD} {letter}: ROOT hashed in {} TSC cycles", crate::tsc().wrapping_sub(began));
    if root_hash != header.root().sha256 {
        root.free(bs);
        return Err(Refusal::Hash("root"));
    }
    println!("{HEAD} {letter}: kernel, cmdline and ROOT are the bytes the signed header names");
    Ok(Chosen { which, version: header.version, digest, kernel, cmdline, root, refused: None })
}

/// `bytes` is the section whose header entry names `want`.
fn held(bytes: &[u8], want: Digest, section: &'static str) -> Result<(), Refusal> {
    if toyos_update::sha256(bytes) != want {
        return Err(Refusal::Hash(section));
    }
    Ok(())
}

/// A section's file, exactly `len` bytes long.
fn read_section(bs: &BootServices, guid: &[u8; 16], path: &str, len: u64, section: &'static str) -> Result<Vec<u8>, Refusal> {
    match read_file(bs, guid, path, len) {
        Ok(bytes) if bytes.len() as u64 == len => Ok(bytes),
        Ok(bytes) => {
            println!("{HEAD}: {path} is {} bytes and its signed header names {len}", bytes.len());
            Err(Refusal::Hash(section))
        }
        Err(FileRefused::Missing) => {
            println!("{HEAD}: {path} is missing");
            Err(Refusal::Unreadable(section))
        }
        Err(FileRefused::Other(why)) => {
            println!("{HEAD}: {path} {why}");
            Err(Refusal::Unreadable(section))
        }
    }
}

enum FileRefused {
    Missing,
    Other(String),
}

/// The file at `path` on the FAT partition `guid` names, if it is at most
/// `max` bytes.
fn read_file(bs: &BootServices, guid: &[u8; 16], path: &str, max: u64) -> Result<Vec<u8>, FileRefused> {
    let handle = crate::loaderlog::volume_handle(bs, guid).map_err(FileRefused::Other)?;
    let mut fs = bs
        .open_protocol_exclusive::<SimpleFileSystem>(handle)
        .map_err(|e| FileRefused::Other(alloc::format!("would not open its volume ({e})")))?;
    let mut root = fs.open_volume().map_err(|e| FileRefused::Other(alloc::format!("has no volume ({e})")))?;
    let name = CString16::try_from(path.replace('/', "\\").as_str())
        .map_err(|_| FileRefused::Other(String::from("is no UCS-2 path")))?;
    let file = match root.open(&name, FileMode::Read, FileAttribute::empty()) {
        Ok(file) => file,
        Err(e) if e.status() == Status::NOT_FOUND => return Err(FileRefused::Missing),
        Err(e) => return Err(FileRefused::Other(alloc::format!("would not open ({e})"))),
    };
    let mut file = file.into_regular_file().ok_or(FileRefused::Other(String::from("is a directory")))?;
    let info = file
        .get_boxed_info::<FileInfo>()
        .map_err(|e| FileRefused::Other(alloc::format!("would not say its size ({e})")))?;
    let size = info.file_size();
    if size > max {
        return Err(FileRefused::Other(alloc::format!("is {size} bytes, past the {max} expected")));
    }
    let mut bytes = crate::alloc_uninit(size as usize);
    let read = file.read(&mut bytes).map_err(|e| FileRefused::Other(alloc::format!("would not read ({e})")))?;
    if read != bytes.len() {
        return Err(FileRefused::Other(alloc::format!("read short: {read} of {size} bytes")));
    }
    Ok(bytes)
}
