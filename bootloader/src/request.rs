//! What the running system asked of this pass through the slot table
//! (`toyos_update::slots::Request`), acted on once.
//!
//! **Consumed before it is acted on.** Each field is written away — the table
//! again, without it, flushed — before this pass does what it asks, so a pass
//! that dies after the write has lost the request rather than repeating it,
//! and a pass whose write fails acts on nothing and says so. A `BootNext` the
//! running system asked for is therefore set at most once, and the firmware
//! deletes it as it boots it (UEFI 2.10 §3.1.2): one boot of that entry, and
//! the order after.
//!
//! Two halves, because they are due at different passes: the firmware's
//! variables ([`firmware`]) in whichever pass comes next, the report pass
//! after a handover included; a slot to boot once ([`once`]) only in a pass
//! that goes on to boot a slot, so the report pass that ends a chain leaves it
//! for the pass after. The slot booted once is left on the table as a trial
//! (`Next::Trial`), which keeps the slot the table marks from any grant while
//! it runs, and the next pass that chooses a slot takes the trial away.

use toyos_update::slots::{Next, Request, Table, Which};
use uefi::prelude::*;

use crate::bootvars::{self, HEAD as ENTRIES};
use crate::rootimage::{Disk, TableAt};

/// The head of every line this module writes.
const HEAD: &str = "Request:";

/// What the firmware half did that decides how this pass ends.
pub enum Fired {
    /// `BootNext` names another entry: the firmware boots it at the reset
    /// this pass ends with, so this pass boots no kernel.
    Next,
    Nothing,
}

/// The boot disk's slot table, and where it is, or why there is none to read
/// a request out of.
fn table(handle: Handle, system_table: &SystemTable<Boot>) -> Result<(Disk<'_>, TableAt), alloc::string::String> {
    let bs = system_table.boot_services();
    let mut disk = Disk::open(bs, crate::rootimage::boot_disk(handle, bs)?)?;
    let at = disk.table_at()?;
    Ok((disk, at))
}

/// Write `next` over `at`'s table, and say so either way; whether it is on
/// the disk.
fn consume(disk: &mut Disk<'_>, at: &TableAt, next: Table, what: &str) -> bool {
    match disk.write_table(at, next) {
        Ok(()) => {
            println!("{HEAD} {what} is taken off the slot table");
            true
        }
        Err(why) => {
            println!("{HEAD} {what} stands, and is not acted on, because taking it off the slot table failed: {why}");
            false
        }
    }
}

/// Put this loader's own entry first in `BootOrder`, and set `BootNext` to an
/// ESP the running system named, where the slot table asks either.
pub fn firmware(handle: Handle, system_table: &SystemTable<Boot>, ours: Option<&[u8; 16]>) -> Fired {
    let (mut disk, at) = match table(handle, system_table) {
        Ok(read) => read,
        Err(why) => {
            println!("{HEAD} the slot table would not read ({why}), so nothing it asks is acted on");
            return Fired::Nothing;
        }
    };
    let asked = at.table.request;
    let esp = match asked.next {
        Some(Next::Esp(guid)) => Some(guid),
        _ => None,
    };
    if !asked.first && esp.is_none() {
        return Fired::Nothing;
    }
    let left = Request { first: false, next: asked.next.filter(|n| !matches!(n, Next::Esp(_))) };
    let what = match (asked.first, esp) {
        (true, Some(_)) => "the boot order and a boot of another ESP",
        (true, None) => "the boot order",
        (false, _) => "a boot of another ESP",
    };
    if !consume(&mut disk, &at, Table { request: left, ..at.table }, what) {
        return Fired::Nothing;
    }
    drop(disk);
    let bs = system_table.boot_services();
    let rt = system_table.runtime_services();
    if asked.first {
        match ours {
            None => println!("{ENTRIES} this loader came off no GPT partition, so it has no entry to put first"),
            Some(guid) => match bootvars::esp(bs, guid).and_then(|esp| bootvars::entry_for(rt, &esp)) {
                Err(why) => println!("{ENTRIES} this loader's own ESP has no entry to put first: {why}"),
                Ok((number, made)) => {
                    let made = if made { "written now" } else { "already there" };
                    match bootvars::put_first(rt, number) {
                        Ok((was, now)) => println!(
                            "{ENTRIES} Boot{number:04X}, this loader's ESP ({made}), is first: BootOrder was {} and is {}",
                            bootvars::spelled(&was),
                            bootvars::spelled(&now)
                        ),
                        Err(why) => println!("{ENTRIES} Boot{number:04X} is not first: {why}"),
                    }
                }
            },
        }
    }
    let Some(guid) = esp else { return Fired::Nothing };
    let named = toyos_gpt::Guid(guid);
    match bootvars::esp(bs, &guid).and_then(|esp| bootvars::entry_for(rt, &esp)) {
        Err(why) => {
            println!("{ENTRIES} no boot of ESP {named} is set, because {why}; this pass boots as it would have");
            Fired::Nothing
        }
        Ok((number, made)) => match bootvars::boot_next(rt, number) {
            Ok(()) => {
                let made = if made { "written now" } else { "already there" };
                println!(
                    "{ENTRIES} BootNext=Boot{number:04X}, ESP {named} ({made}): the firmware boots it once, at the reset this pass ends with"
                );
                Fired::Next
            }
            Err(why) => {
                println!("{ENTRIES} {why}; this pass boots as it would have");
                Fired::Nothing
            }
        },
    }
}

/// The slot the running system asked to boot once, left on the table as its
/// trial; or `None` where it asked none, or where the table would not take
/// the trial — a trial that could not be made a trial is not booted at all.
/// A trial the last boot ran is over, and is taken away here.
pub fn once(handle: Handle, system_table: &SystemTable<Boot>) -> Option<Which> {
    let (mut disk, at) = match table(handle, system_table) {
        Ok(read) => read,
        // `slot::choose` reads the same table next and refuses by name.
        Err(_) => return None,
    };
    let (left, what, boots) = match at.table.request.next {
        Some(Next::Slot(which)) => (Some(Next::Trial(which)), alloc::format!("a boot of slot {} once", which.letter()), Some(which)),
        Some(Next::Trial(which)) => (None, alloc::format!("slot {}'s trial, which is over,", which.letter()), None),
        _ => return None,
    };
    let left = Table { request: Request { next: left, ..at.table.request }, ..at.table };
    consume(&mut disk, &at, left, &what).then_some(boots).flatten()
}
