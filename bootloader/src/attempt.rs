//! How many times this image has been handed the machine without reporting
//! back, and which slot's image it handed it to, kept on the stick it boots
//! from.
//!
//! **The bound on a hang, and the machine has no other way out of one.**
//! `bootnext::point_at_us` aims `BootNext` at this loader before every kernel
//! handoff, so a kernel that hangs and an owner who cuts power get: firmware,
//! this loader, the same kernel, the same hang — for ever. The black box cannot
//! break it, because a power cut is exactly what empties the black box: the next
//! pass finds nothing to report, arms a fresh record and boots the same kernel
//! again. The only ways out of that are the firmware's boot menu and pulling the
//! stick, and neither of those is the loop.
//!
//! What survives a power cut is the stick. So the count is a file on the log
//! partition, beside `loader.log`: **one flash writes a fresh log partition, so
//! the count is per image and per flash without anything having to say so.** It
//! carries the partition's own signature anyway, because a file that says what
//! it counts for is one a reader can be handed on its own.
//!
//! One hand per hang, never two: the second attempt of an image whose first
//! never reported boots no kernel at all. **The same file is the slots'
//! record** (`toyos_update::record`): the image a pass handed the machine to,
//! and every image that died on a boot of its own, so the pass after a hang or
//! a death boots the other slot rather than the one that died.
//!
//! Written twice a pass: before the log opens, with the count and whatever the
//! last boot's end taught, so a pass that dies before its handoff has still
//! counted; and after the slot is chosen, through the log's own open volume,
//! with the image it chose.

use alloc::string::String;
use toyos_update::record::{self, Record};
use uefi::proto::media::file::{Directory, File, FileAttribute, FileMode};
use uefi::prelude::*;
use uefi::{cstr16, CStr16};

use crate::loaderlog;

/// The file, at the root of the log partition beside `loader.log`.
///
/// Not one of `logd`'s names and not the loader's log: a reader of the volume
/// that walks it for either finds this and passes over it
/// (`toyos_build::bootlog::split_listing`).
const NAME: &CStr16 = cstr16!("attempts");

/// What the stick says about this image's attempts, or why it could not say.
///
/// **`Err` is not zero.** A volume this cannot read is one the bound is off on,
/// and the caller says so rather than treating an unreadable stick as a first
/// attempt — which would be a bound that silently never fires.
pub fn read(system_table: &SystemTable<Boot>, guid: &[u8; 16]) -> Result<Record, String> {
    loaderlog::with_volume(system_table, guid, |root| {
        let file = match root.open(NAME, FileMode::Read, FileAttribute::empty()) {
            Ok(file) => file,
            // No file is a first attempt, which is every freshly flashed image.
            Err(e) if e.status() == Status::NOT_FOUND => return Ok(Record::default()),
            Err(e) => return Err(alloc::format!("{NAME} would not open ({e})")),
        };
        let Some(mut file) = file.into_regular_file() else {
            return Err(alloc::format!("{NAME} on the log partition is a directory"));
        };
        // One byte more than a record, so a longer file is told from a whole one.
        let mut buffer = [0u8; record::BYTES + 1];
        let n = file.read(&mut buffer).map_err(|e| alloc::format!("{NAME} would not read ({e})"))?;
        // Short, longer, or another partition's: not this file, and a count
        // guessed out of it would be a bound firing on nothing.
        Record::decode(&buffer[..n], guid).map_err(|why| alloc::format!("{NAME} {why}"))
    })
    .and_then(|inner| inner)
}

/// Write `record` down before the log opens, replacing whatever was there.
///
/// Written **before the kernel is handed the machine**, because a count written
/// after it is a count a hang never gets to.
pub fn write(system_table: &SystemTable<Boot>, guid: &[u8; 16], record: &Record) -> Result<(), String> {
    loaderlog::with_volume(system_table, guid, |root| put(root, guid, record)).and_then(|inner| inner)
}

/// Write `record` down through the log's own open volume, once the slot this
/// pass boots is chosen.
pub fn write_chosen(guid: &[u8; 16], record: &Record) -> Result<(), String> {
    loaderlog::with_open_volume(|root| put(root, guid, record)).and_then(|inner| inner)
}

fn put(root: &mut Directory, guid: &[u8; 16], record: &Record) -> Result<(), String> {
    // Deleted and recreated rather than rewound: a fixed-width record is
    // still a record a shorter write would leave the tail of.
    match root.open(NAME, FileMode::ReadWrite, FileAttribute::empty()) {
        Ok(stale) => {
            if let Err(e) = stale.delete() {
                return Err(alloc::format!("{NAME} would not delete ({e})"));
            }
        }
        Err(e) if e.status() == Status::NOT_FOUND => {}
        Err(e) => return Err(alloc::format!("{NAME} would not open ({e})")),
    }
    let file = root
        .open(NAME, FileMode::CreateReadWrite, FileAttribute::empty())
        .map_err(|e| alloc::format!("{NAME} would not be created ({e})"))?;
    let Some(mut file) = file.into_regular_file() else {
        return Err(alloc::format!("{NAME} on the log partition is a directory"));
    };
    file.write(&record.encode(guid)).map_err(|e| alloc::format!("{NAME} would not write ({e})"))?;
    // **Flushed here and not at the handoff.** What this file exists to
    // survive is a power cut, and a byte in a cache survives nothing.
    file.flush().map_err(|e| alloc::format!("{NAME} would not flush ({e})"))?;
    Ok(())
}

/// The next count to write down, given what the stick said.
///
/// Saturating rather than wrapping: a count that got that far fired the bound
/// long ago, and a wrap to zero would arm the loop again.
pub const fn next(previous: u8) -> u8 {
    previous.saturating_add(1)
}

/// Whether this pass is the one that must not boot a kernel.
///
/// **Three things, and all three.** The machine has a black box, so "the last
/// boot reported nothing" is a question that has an answer at all; this pass
/// harvested no record, so the last boot did not report; and the stick says this
/// image has already had the machine once. A first attempt is not bounded — the
/// bound is on the *retry* — and a machine with no page is one where the first
/// two cannot be asked, which is said rather than guessed at.
pub const fn is_the_retry(has_a_page: bool, harvested: bool, previous: u8) -> bool {
    has_a_page && !harvested && previous >= 1
}
