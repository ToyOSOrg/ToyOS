//! `/system/bin/update`'s install: a signed image read off its input and
//! written into the slot a machine is not running, over whatever disk holds
//! each partition. It writes, in order:
//!
//! 1. nothing, until the signed header's signature is the key's and its
//!    version is newer than the running image and no older than the idle
//!    slot's (`toyos_update::policy::installable`);
//! 2. ROOT, streamed onto the idle ROOT partition as it arrives and held to
//!    the header's hash once whole;
//! 3. the kernel, its boot parameter and the signed header onto the idle FAT
//!    volume, each held to its hash before it is written;
//! 4. a flush of each partition, which answers for these writes and no other
//!    process's;
//! 5. the slot table, marking the idle slot — the copy that is not current,
//!    so a torn write leaves the old mark — and its flush.
//!
//! A refusal at any step leaves the mark where it was, so the machine boots
//! what it booted before. The loader checks every byte again at the next boot
//! and refuses the slot by name if anything here was wrong.

use std::io::Read;
use std::time::Instant;

use toyos_disk::{Disk, BLOCK};
use toyos_update::image::{Header, HEADER_BYTES, SIGNED_BYTES};
use toyos_update::slots::{self, Which};
use toyos_update::{policy, sig};

pub(crate) mod volume;

/// One partition of the idle slot: its blocks, and its unique GUID.
pub struct Partition<'a> {
    pub disk: &'a mut dyn Disk,
    pub unique: [u8; 16],
}

/// What an install wrote, and how long ROOT took to stream.
#[derive(Debug)]
pub struct Installed {
    pub version: u64,
    pub slot: Which,
    pub root_bytes: u64,
    pub root_ms: u128,
}

/// The image on `input`, signed with `key`'s seed, into the slot `boot` and
/// `root` are, and that slot marked on `table`'s partition. `begun` hears the
/// line said once the image is known installable and before anything is
/// written; `now` is the Unix second the volume's files are stamped with.
pub fn install(
    key: &[u8; 32],
    table: &mut dyn Disk,
    boot: Partition<'_>,
    root: Partition<'_>,
    input: &mut impl Read,
    now: u64,
    begun: impl FnOnce(String),
) -> Result<Installed, String> {
    let (current_table, current) =
        slots::read(|copies| table.read(0, copies.as_flattened_mut())).map_err(|why| why.to_string())?;
    let idle = [Which::A, Which::B]
        .into_iter()
        .find(|&w| current_table.slot(w).is_some_and(|s| s.boot == boot.unique && s.root == root.unique))
        .ok_or("the partitions this process holds are no slot the table names")?;
    let running = current_table.slot(idle.other()).ok_or("the table carries no running slot")?;

    let mut signed = [0u8; SIGNED_BYTES];
    input.read_exact(&mut signed).map_err(|e| format!("the input ended before a signed header: {e}"))?;
    let header = Header::parse(&signed).map_err(|why| why.to_string())?;
    let header_bytes: &[u8; HEADER_BYTES] = signed[..HEADER_BYTES].try_into().expect("a signed header begins with its header");
    sig::verify(key, header_bytes, &toyos_update::image::signature_of(&signed)).map_err(|why| why.to_string())?;
    let idle_version = current_table.slot(idle).map(|s| s.version).filter(|&v| v != 0);
    policy::installable(header.version, running.version, idle_version).map_err(|why| why.to_string())?;
    if header.root().len > root.disk.blocks() * BLOCK as u64 {
        return Err(format!(
            "ROOT is {} bytes and slot {}'s ROOT partition holds {}",
            header.root().len,
            idle.letter(),
            root.disk.blocks() * BLOCK as u64
        ));
    }
    begun(format!(
        "update: version {} is signed by this machine's key; writing slot {} (running {} at version {})",
        header.version,
        idle.letter(),
        idle.other().letter(),
        running.version
    ));

    let kernel = take(input, header.kernel().len, header.kernel().sha256, "kernel")?;
    let cmdline = take(input, header.cmdline().len, header.cmdline().sha256, "cmdline")?;
    let streamed = Instant::now();
    stream_root(input, &mut *root.disk, header.root().len, header.root().sha256)?;
    let root_ms = streamed.elapsed().as_millis();
    let mut rest = [0u8; 1];
    match input.read(&mut rest) {
        Ok(0) => {}
        Ok(_) => return Err("the input carries more bytes than its signed header names".into()),
        Err(e) => return Err(format!("the input's end would not read: {e}")),
    }

    write_volume(&mut *boot.disk, &kernel, &cmdline, &signed, now)?;
    root.disk.flush().map_err(|e| format!("slot {}'s ROOT is not durable: {e:?}", idle.letter()))?;
    boot.disk.flush().map_err(|e| format!("slot {}'s volume is not durable: {e:?}", idle.letter()))?;

    let mut next = current_table;
    next.marked = idle;
    let mut slot = current_table.slot(idle).expect("the idle slot is in the table");
    slot.version = header.version;
    next.slots[idle.index()] = Some(slot);
    let (copy, block) = slots::next_write((current_table, current), next);
    table.write(copy as u64, &block).map_err(|e| format!("the slot table's copy {copy} would not write: {e:?}"))?;
    table.flush().map_err(|e| format!("the slot table is not durable: {e:?}"))?;
    Ok(Installed { version: header.version, slot: idle, root_bytes: header.root().len, root_ms })
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
    let mut hasher = toyos_sha2_hw::sha256();
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
    if hasher.finalize() != sha256 {
        return Err("ROOT is not the bytes its signed header names; the mark is not moved".into());
    }
    Ok(())
}

/// The kernel, its boot parameter and the signed header onto the idle slot's
/// FAT volume, replacing whatever was there.
fn write_volume(boot: &mut dyn Disk, kernel: &[u8], cmdline: &[u8], signed: &[u8], now: u64) -> Result<(), String> {
    let mut fs = toyos_fat32::Fat32::mount(volume::Cached::new(boot))
        .map_err(|e| format!("the idle slot's volume does not mount: {e:?}"))?;
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
    // Writes every block the volume holds (`Cached::flush`).
    fs.sync().map_err(|e| format!("the idle volume's blocks: {e:?}"))
}

