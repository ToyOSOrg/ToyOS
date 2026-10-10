//! `/system/bin/update`: a signed image on standard input, installed into the
//! slot this machine is not running, and marked to boot next.
//!
//! **It does not care how the image arrived**: `ssh <machine> update < image`
//! today, a pull from a release server tomorrow, a file on a local shell — the
//! bytes on standard input are the whole of its input, and the owner's
//! signature over them is the whole of its authority to install anything.
//!
//! **What it holds is the whole of what it can write** (`slots` in its
//! `system.toml` row): the slot table's partition and the idle slot's FAT
//! volume and ROOT, as claims the supervisor endows under their labels on a
//! disk the kernel drives, or as connectors in its namespace under the same
//! names, each minted for its one partition on the block service's port; the
//! slot this boot runs is never among them (`toyos_update::slots::idle`). So
//! it writes, in order:
//!
//! 1. nothing, until the signed header's signature is this machine's key's and
//!    its version is newer than the running image and no older than the idle
//!    slot's (`toyos_update::policy::installable`);
//! 2. ROOT, streamed onto the idle ROOT partition as it arrives and held to
//!    the header's hash once whole;
//! 3. the kernel, its boot parameter and the signed header onto the idle FAT
//!    volume, each held to its hash before it is written;
//! 4. a flush of each partition, which answers for these writes and no other
//!    process's;
//! 5. the slot table, marking the idle slot — the copy that is not current,
//!    so a torn write leaves the old mark — and its fsync.
//!
//! A refusal at any step leaves the mark where it was, so the machine boots
//! what it booted before. The loader checks every byte again at the next boot
//! and refuses the slot by name if anything here was wrong.

use std::io::Read;
use std::time::Instant;

use diskserver::disk::{Claimed, Disk, Served, BLOCK};
use toyos::endow::{self, Endowments};
use toyos::PartitionDev;
use toyos_update::image::{Header, HEADER_BYTES, SIGNED_BYTES};
use toyos_update::slots::{self, Which};
use toyos_fat32::BlockAccess as _;
use toyos_update::{policy, sig};

/// The key an image must be signed with: the same the loader embeds.
const KEY: [u8; 32] = sig::key_from_hex(env!("TOYOS_IMAGE_KEY"));

/// The line an install ends on, which the host reads.
const INSTALLED: &str = "update: installed";

fn main() {
    let began = Instant::now();
    match run(began) {
        Ok(line) => println!("{line}"),
        Err(why) => {
            println!("update: refused: {why}");
            std::process::exit(1);
        }
    }
}

/// One partition of the grant: its blocks, and its unique GUID.
struct Held {
    disk: Box<dyn Disk>,
    unique: [u8; 16],
}

/// The partition endowed under `label`: a claim, or a session through the
/// connector of that name, which reaches one partition and lists it.
fn held(label: &str) -> Result<Held, String> {
    let none = || format!("this process holds no `{label}`: the supervisor grants the idle slot to one update at a time, and says why where it grants none");
    if let Some(claim) = Endowments::get().take::<PartitionDev>(label) {
        let unique = claim.describe().map_err(|e| format!("the `{label}` claim: {e:?}"))?.unique_guid;
        let disk = Claimed::new(claim).map_err(|e| format!("the `{label}` claim: {e:?}"))?;
        return Ok(Held { disk: Box::new(disk), unique });
    }
    let names = endow::namespace().ok_or_else(none)?;
    let own = toyos::namespace::build().keep(names, &[label]).finish().map_err(|_| none())?;
    let listed = diskserver::list(&own, label).map_err(|why| format!("`{label}` would not list its partition: {why:?}"))?;
    let [one] = listed[..] else {
        return Err(format!("`{label}` lists {} partitions, and is minted for one", listed.len()));
    };
    let session = diskserver::Session::open(own, label, one.unique)
        .map_err(|why| format!("`{label}`'s partition would not open: {why:?}"))?;
    Ok(Held { disk: Box::new(Served::new(session)), unique: one.unique })
}

fn run(began: Instant) -> Result<String, String> {
    let (mut table_part, mut boot, mut root) = (held(slots::TABLE_LABEL)?, held(slots::BOOT_LABEL)?, held(slots::ROOT_LABEL)?);
    let (table, current) =
        slots::read(|copies| table_part.disk.read(0, copies.as_flattened_mut())).map_err(|why| why.to_string())?;
    let idle = [Which::A, Which::B]
        .into_iter()
        .find(|&w| table.slot(w).is_some_and(|s| s.boot == boot.unique && s.root == root.unique))
        .ok_or("the partitions this process holds are no slot the table names")?;
    let running = table.slot(idle.other()).ok_or("the table carries no running slot")?;

    let mut input = std::io::stdin().lock();
    let mut signed = [0u8; SIGNED_BYTES];
    input.read_exact(&mut signed).map_err(|e| format!("the input ended before a signed header: {e}"))?;
    let header = Header::parse(&signed).map_err(|why| why.to_string())?;
    let header_bytes: &[u8; HEADER_BYTES] = signed[..HEADER_BYTES].try_into().expect("a signed header begins with its header");
    sig::verify(&KEY, header_bytes, &toyos_update::image::signature_of(&signed)).map_err(|why| why.to_string())?;
    let idle_version = table.slot(idle).map(|s| s.version).filter(|&v| v != 0);
    policy::installable(header.version, running.version, idle_version).map_err(|why| why.to_string())?;
    if header.root().len > root.disk.blocks() * BLOCK as u64 {
        return Err(format!(
            "ROOT is {} bytes and slot {}'s ROOT partition holds {}",
            header.root().len,
            idle.letter(),
            root.disk.blocks() * BLOCK as u64
        ));
    }
    println!(
        "update: version {} is signed by this machine's key; writing slot {} (running {} at version {})",
        header.version,
        idle.letter(),
        idle.other().letter(),
        running.version
    );

    let kernel = take(&mut input, header.kernel().len, header.kernel().sha256, "kernel")?;
    let cmdline = take(&mut input, header.cmdline().len, header.cmdline().sha256, "cmdline")?;
    let streamed = Instant::now();
    stream_root(&mut input, &mut *root.disk, header.root().len, header.root().sha256)?;
    let root_ms = streamed.elapsed().as_millis();
    let mut rest = [0u8; 1];
    match input.read(&mut rest) {
        Ok(0) => {}
        Ok(_) => return Err("the input carries more bytes than its signed header names".into()),
        Err(e) => return Err(format!("the input's end would not read: {e}")),
    }

    write_volume(&mut *boot.disk, &kernel, &cmdline, &signed)?;
    root.disk.flush().map_err(|e| format!("slot {}'s ROOT is not durable: {e:?}", idle.letter()))?;
    boot.disk.flush().map_err(|e| format!("slot {}'s volume is not durable: {e:?}", idle.letter()))?;

    let mut next = table;
    next.marked = idle;
    let mut slot = table.slot(idle).expect("the idle slot is in the table");
    slot.version = header.version;
    next.slots[idle.index()] = Some(slot);
    let (copy, block) = slots::next_write((table, current), next);
    table_part
        .disk
        .write(copy as u64, &block)
        .map_err(|e| format!("the slot table's copy {copy} would not write: {e:?}"))?;
    table_part.disk.flush().map_err(|e| format!("the slot table is not durable: {e:?}"))?;

    Ok(format!(
        "{INSTALLED} version {} in slot {} ({} bytes of ROOT in {root_ms} ms, {} ms in all); it boots at the next reboot",
        header.version,
        idle.letter(),
        header.root().len,
        began.elapsed().as_millis()
    ))
}

/// Exactly `len` bytes of the input, held to `sha256`.
fn take(input: &mut impl Read, len: u64, sha256: toyos_update::Digest, section: &str) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0u8; len as usize];
    input.read_exact(&mut bytes).map_err(|e| format!("the input ended inside the {section}: {e}"))?;
    if toyos_update::sha256(&bytes) != sha256 {
        return Err(format!("the {section} is not the bytes its signed header names"));
    }
    Ok(bytes)
}

/// The blocks of ROOT [`stream_root`] reads and writes at a time.
const STREAM_BLOCKS: usize = 32;

/// ROOT onto the idle ROOT partition as it arrives, [`STREAM_BLOCKS`] at a
/// time, and held to `sha256` once whole.
fn stream_root(input: &mut impl Read, root: &mut dyn Disk, len: u64, sha256: toyos_update::Digest) -> Result<(), String> {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    let mut run = vec![0u8; STREAM_BLOCKS * BLOCK];
    let blocks = len / BLOCK as u64;
    let mut at = 0u64;
    while at < blocks {
        let n = (blocks - at).min(STREAM_BLOCKS as u64) as usize;
        let bytes = &mut run[..n * BLOCK];
        input.read_exact(bytes).map_err(|e| format!("the input ended inside ROOT, at block {at}: {e}"))?;
        hasher.update(&*bytes);
        root.write(at, bytes).map_err(|e| format!("ROOT's blocks from {at} would not write: {e:?}"))?;
        at += n as u64;
    }
    if <[u8; 32]>::from(hasher.finalize()) != sha256 {
        return Err("ROOT is not the bytes its signed header names; the mark is not moved".into());
    }
    Ok(())
}

/// The kernel, its boot parameter and the signed header onto the idle slot's
/// FAT volume, replacing whatever was there.
fn write_volume(boot: &mut dyn Disk, kernel: &[u8], cmdline: &[u8], signed: &[u8]) -> Result<(), String> {
    let mut fs = toyos_fat32::Fat32::mount(volume::Cached::new(boot))
        .map_err(|e| format!("the idle slot's volume does not mount: {e:?}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let time = toyos_fat32::FatTime::from_unix_secs(now);
    fs.create_dir_all("toyos", time).map_err(|e| format!("toyos/ on the idle volume: {e:?}"))?;
    for (path, bytes) in [
        (slots::SIGNED_FILE, signed),
        (slots::KERNEL_FILE, kernel),
        (slots::CMDLINE_FILE, cmdline),
    ] {
        if fs.exists(path).map_err(|e| format!("{path}: {e:?}"))? {
            fs.remove(path).map_err(|e| format!("removing the old {path}: {e:?}"))?;
        }
        let mut file = fs.create(path, time).map_err(|e| format!("creating {path}: {e:?}"))?;
        fs.write(&mut file, 0, bytes).map_err(|e| format!("writing {path}: {e:?}"))?;
        fs.flush_meta(&mut file, time).map_err(|e| format!("recording {path}: {e:?}"))?;
    }
    fs.sync().map_err(|e| format!("the idle volume's metadata: {e:?}"))?;
    fs.into_device().flush().map_err(|e| format!("the idle volume's blocks: {e:?}"))
}

mod volume;
