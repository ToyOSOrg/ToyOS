//! `/system/bin/update`: a signed image on standard input, installed into the
//! slot this machine is not running, and marked to boot next — or booted once
//! and not marked; and the requests the running system makes of the loader's
//! next pass, which reach the firmware's boot variables.
//!
//! ```text
//! update                 < image   install into the idle slot and mark it
//! update --once          < image   install into the idle slot and boot it once
//! update --boot-first               put this loader's entry first in BootOrder
//! update --boot-next <esp guid>     boot that EFI system partition once
//! ```
//!
//! **It does not care how the image arrived**: `ssh <machine> update < image`
//! today, a pull from a release server tomorrow, a file on a local shell — the
//! bytes on standard input are the whole of its input, and the owner's
//! signature over them is the whole of its authority to install anything.
//!
//! **What it holds is the whole of what it can write** (`slots` in its
//! `system.toml` row): init claims the slot table's partition and the idle
//! slot's FAT volume and ROOT and endows them, and the slot this boot runs is
//! never among them (`toyos_update::slots::idle`). So it writes, in order:
//!
//! 1. nothing, until the signed header's signature is this machine's key's and
//!    its version is newer than the running image and no older than the idle
//!    slot's (`toyos_update::policy::installable`);
//! 2. ROOT, streamed onto the idle ROOT partition as it arrives and held to
//!    the header's hash once whole;
//! 3. the kernel, its boot parameter and the signed header onto the idle FAT
//!    volume, each held to its hash before it is written;
//! 4. an fsync of each claim, which answers for these writes and no other
//!    process's;
//! 5. the slot table, marking the idle slot — or, `--once`, asking the loader
//!    to boot it once and leaving the mark where it is — the copy that is not
//!    current, so a torn write leaves the old table, and its fsync.
//!
//! A refusal at any step leaves the table as it was, so the machine boots
//! what it booted before. The loader checks every byte again at the next boot
//! and refuses the slot by name if anything here was wrong.
//!
//! **A request is the table's too** (`toyos_update::slots::Request`): the
//! loader acts on it once at its next pass, and writes it away as it does.
//! It asks for no more than an entry for an EFI system partition the loader
//! finds itself, first in the order or next once.

use std::io::Read;
use std::time::Instant;

use toyos::endow::Endowments;
use toyos::PartitionDev;
use toyos_abi::part::{Block, BLOCK_BYTES, MAX_BLOCKS_PER_CALL};
use toyos_update::image::{Header, HEADER_BYTES, SIGNED_BYTES};
use toyos_update::slots::{self, Next, Table, Which};
use toyos_fat32::BlockAccess as _;
use toyos_update::{policy, sig};

/// The key an image must be signed with: the same the loader embeds.
const KEY: [u8; 32] = sig::key_from_hex(env!("TOYOS_IMAGE_KEY"));

/// The line an install ends on, which the host reads.
const INSTALLED: &str = "update: installed";

/// What this run was asked for.
enum Asked {
    /// An image on standard input, marked, or booted `once`.
    Install { once: bool },
    BootFirst,
    BootNext([u8; 16]),
}

fn asked() -> Result<Asked, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words[..] {
        [] => Ok(Asked::Install { once: false }),
        ["--once"] => Ok(Asked::Install { once: true }),
        ["--boot-first"] => Ok(Asked::BootFirst),
        ["--boot-next", guid] => toyos_update::entry::parse_guid(guid)
            .map(Asked::BootNext)
            .ok_or_else(|| format!("{guid:?} is no partition GUID; --boot-next wants one as the GPT tools print it")),
        _ => Err(format!(
            "{words:?} is no ask this takes: `update`, `update --once` (each with an image on its input), \
             `update --boot-first` or `update --boot-next <esp guid>`"
        )),
    }
}

fn main() {
    let began = Instant::now();
    match asked().and_then(|asked| run(asked, began)) {
        Ok(line) => println!("{line}"),
        Err(why) => {
            println!("update: refused: {why}");
            std::process::exit(1);
        }
    }
}
/// The three claims init endowed, or why this process holds none.
fn grant() -> Result<(PartitionDev, PartitionDev, PartitionDev), String> {
    let take = |label: &str| {
        Endowments::get()
            .take::<PartitionDev>(label)
            .ok_or_else(|| format!("this process holds no `{label}`: init grants the idle slot to one update at a time, and says why where it grants none"))
    };
    Ok((take(slots::TABLE_LABEL)?, take(slots::BOOT_LABEL)?, take(slots::ROOT_LABEL)?))
}

fn run(asked: Asked, began: Instant) -> Result<String, String> {
    let (table_claim, boot, root) = grant()?;
    let (table, current) = read_table(&table_claim)?;
    let once = match asked {
        Asked::Install { once } => once,
        Asked::BootFirst => {
            let request = slots::Request { first: true, ..table.request };
            write_table(&table_claim, (table, current), Table { request, ..table })?;
            return Ok(String::from(
                "update: the loader puts its own entry first in the firmware's BootOrder at its next pass",
            ));
        }
        Asked::BootNext(guid) => {
            let request = slots::Request { next: Some(Next::Esp(guid)), ..table.request };
            write_table(&table_claim, (table, current), Table { request, ..table })?;
            return Ok(format!(
                "update: the loader boots EFI system partition {} once, at its next pass, and the order after it",
                toyos_update::entry::GuidText(guid)
            ));
        }
    };
    let boot_guid = boot.describe().map_err(|e| format!("the idle volume's claim: {e:?}"))?.unique_guid;
    let root_info = root.describe().map_err(|e| format!("the idle ROOT's claim: {e:?}"))?;
    let idle = [Which::A, Which::B]
        .into_iter()
        .find(|&w| table.slot(w).is_some_and(|s| s.boot == boot_guid && s.root == root_info.unique_guid))
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
    if header.root().len > root_info.blocks * BLOCK_BYTES as u64 {
        return Err(format!(
            "ROOT is {} bytes and slot {}'s ROOT partition holds {}",
            header.root().len,
            idle.letter(),
            root_info.blocks * BLOCK_BYTES as u64
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
    stream_root(&mut input, &root, header.root().len, header.root().sha256)?;
    let root_ms = streamed.elapsed().as_millis();
    let mut rest = [0u8; 1];
    match input.read(&mut rest) {
        Ok(0) => {}
        Ok(_) => return Err("the input carries more bytes than its signed header names".into()),
        Err(e) => return Err(format!("the input's end would not read: {e}")),
    }

    write_volume(&boot, &kernel, &cmdline, &signed)?;
    root.sync().map_err(|e| format!("slot {}'s ROOT is not durable: {e:?}", idle.letter()))?;
    boot.sync().map_err(|e| format!("slot {}'s volume is not durable: {e:?}", idle.letter()))?;

    let mut next = table;
    let mut slot = table.slot(idle).expect("the idle slot is in the table");
    slot.version = header.version;
    next.slots[idle.index()] = Some(slot);
    // A slot asked for once before this install is answered by it: the idle
    // slot is marked now, or is the one asked for.
    next.request.next = next.request.next.filter(|n| matches!(n, Next::Esp(_)));
    let when = if once {
        next.request.next = Some(Next::Slot(idle));
        format!("it boots once at the next reboot, and slot {} at every boot after", table.marked.letter())
    } else {
        next.marked = idle;
        String::from("it boots at the next reboot")
    };
    write_table(&table_claim, (table, current), next)?;

    Ok(format!(
        "{INSTALLED} version {} in slot {} ({} bytes of ROOT in {root_ms} ms, {} ms in all); {when}",
        header.version,
        idle.letter(),
        header.root().len,
        began.elapsed().as_millis()
    ))
}

/// Make `next` the slot table, as every writer does (`slots::next_write`), and
/// make it durable before this answers.
fn write_table(claim: &PartitionDev, current: (Table, usize), next: Table) -> Result<(), String> {
    let (copy, block) = slots::next_write(current, next);
    claim.write(copy as u64, &[block]).map_err(|e| format!("the slot table's copy {copy} would not write: {e:?}"))?;
    claim.sync().map_err(|e| format!("the slot table is not durable: {e:?}"))
}

/// The slot table and which copy of it is current.
fn read_table(claim: &PartitionDev) -> Result<(Table, usize), String> {
    let mut copies: [Block; 2] = [[0; BLOCK_BYTES]; 2];
    claim.read(0, &mut copies).map_err(|e| format!("the slot table would not read: {e:?}"))?;
    slots::current([&copies[0], &copies[1]]).map_err(|why| format!("the slot table's partition holds {why}"))
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

/// ROOT onto the idle ROOT partition as it arrives, a call's worth of blocks
/// at a time, and held to `sha256` once whole.
fn stream_root(input: &mut impl Read, root: &PartitionDev, len: u64, sha256: toyos_update::Digest) -> Result<(), String> {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    let mut run: Vec<Block> = vec![[0; BLOCK_BYTES]; MAX_BLOCKS_PER_CALL];
    let blocks = len / BLOCK_BYTES as u64;
    let mut at = 0u64;
    while at < blocks {
        let n = (blocks - at).min(MAX_BLOCKS_PER_CALL as u64) as usize;
        for block in &mut run[..n] {
            input.read_exact(block).map_err(|e| format!("the input ended inside ROOT, at block {at}: {e}"))?;
            hasher.update(&block[..]);
        }
        root.write(at, &run[..n]).map_err(|e| format!("ROOT's blocks from {at} would not write: {e:?}"))?;
        at += n as u64;
    }
    if <[u8; 32]>::from(hasher.finalize()) != sha256 {
        return Err("ROOT is not the bytes its signed header names; the mark is not moved".into());
    }
    Ok(())
}

/// The kernel, its boot parameter and the signed header onto the idle slot's
/// FAT volume, replacing whatever was there.
fn write_volume(boot: &PartitionDev, kernel: &[u8], cmdline: &[u8], signed: &[u8]) -> Result<(), String> {
    let blocks = boot.describe().map_err(|e| format!("the idle volume's claim: {e:?}"))?.blocks;
    let mut fs = toyos_fat32::Fat32::mount(volume::Cached::new(boot, blocks))
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
