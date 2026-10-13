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
//! slot this boot runs is never among them (`toyos_update::slots::idle`). What
//! it writes there, and in what order, is [`install`]'s.

mod install;

use std::time::Instant;

use diskserver::disk::{Claimed, Disk, Served};
use toyos::endow::{self, Endowments};
use toyos::PartitionDev;
use toyos_update::sig;
use toyos_update::slots;
use install::Partition;

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
    let (mut table, mut boot, mut root) = (held(slots::TABLE_LABEL)?, held(slots::BOOT_LABEL)?, held(slots::ROOT_LABEL)?);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs());
    let installed = install::install(
        &KEY,
        &mut *table.disk,
        Partition { disk: &mut *boot.disk, unique: boot.unique },
        Partition { disk: &mut *root.disk, unique: root.unique },
        &mut std::io::stdin().lock(),
        now,
        |line| println!("{line}"),
    )?;
    Ok(format!(
        "{INSTALLED} version {} in slot {} ({} bytes of ROOT in {} ms, {} ms in all); it boots at the next reboot",
        installed.version,
        installed.slot.letter(),
        installed.root_bytes,
        installed.root_ms,
        began.elapsed().as_millis()
    ))
}
