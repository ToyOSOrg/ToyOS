//! The loader's own log, written to the stick as it runs.
//!
//! Every line [`crate::println`] puts on the firmware's console is appended
//! here too, written and flushed before the loader goes on, so the last line in
//! the file is the last thing the loader did before a machine stopped doing
//! anything — which is the whole point of it on a laptop whose only other
//! channel is a screen nobody was watching.
//!
//! The file is `loader.log` at the root of the partition
//! `KernelArgs::log_partition_guid` names, truncated at each boot. One file
//! under a fixed name and never one of `logd`'s timestamped ones, so a reader
//! looking for the kernel's log on this volume never picks this up:
//! `toyos_build::bootlog::LOADER_LOG` is the host's half of that name and
//! `the_loader_writes_the_file_the_host_reads` holds the two equal.
//!
//! A partition this cannot open or write is refused by name on the console and
//! the boot continues: the loader's job is the kernel, so a missing log is
//! reported rather than fatal. There is no second place to try.

use core::cell::UnsafeCell;
use core::fmt;

use uefi::proto::media::file::{File, FileAttribute, FileMode, RegularFile};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::proto::media::partition::PartitionInfo;
use uefi::table::boot::{BootServices, SearchType};
use uefi::{prelude::*, CStr16, Handle};

/// The loader's first line, which is also the file's: [`open`] runs before it.
pub const BEGINS_AT: &str = "ToyOS Bootloader 1.0";

/// The file's last line: `ExitBootServices` takes the firmware's filesystem
/// with it, so nothing the loader does afterwards can be written down.
const ENDS_AT: &str = "Loader log: ExitBootServices, so this file ends here";

const NAME: &CStr16 = cstr16!("loader.log");

/// The open file, from [`open`] until [`close`].
///
/// A UEFI application owns the machine — one processor, no preemption, and
/// nothing here runs from a firmware callback — so the cell has one caller at a
/// time and no borrow of its contents outlives the call that took it.
struct Sink(UnsafeCell<Option<RegularFile>>);

// SAFETY: [`Sink`]'s own contract; nothing else in this crate names the type.
unsafe impl Sync for Sink {}

static SINK: Sink = Sink(UnsafeCell::new(None));

/// Open `loader.log` on the partition `guid` names, discarding what an earlier
/// boot left there.
pub fn open(system_table: &SystemTable<Boot>, guid: &[u8; 16]) {
    let bs = system_table.boot_services();
    let Ok(handles) = bs.locate_handle_buffer(SearchType::from_proto::<SimpleFileSystem>()) else {
        return refused(format_args!("this machine publishes no filesystem at all"));
    };
    let Some(&handle) = handles.iter().find(|h| unique_guid(bs, **h).as_ref() == Some(guid)) else {
        return refused(format_args!("no filesystem here sits on partition {guid:02x?}"));
    };
    let mut fs = match bs.open_protocol_exclusive::<SimpleFileSystem>(handle) {
        Ok(fs) => fs,
        Err(e) => return refused(format_args!("the log partition would not open ({e})")),
    };
    let mut root = match fs.open_volume() {
        Ok(root) => root,
        Err(e) => return refused(format_args!("the log partition has no volume ({e})")),
    };
    // Deleted rather than rewound: one boot's account per boot, and a shorter
    // one than the last would otherwise end in the last one's tail.
    if let Ok(stale) = root.open(NAME, FileMode::ReadWrite, FileAttribute::empty()) {
        if let Err(e) = stale.delete() {
            return refused(format_args!("the last boot's {NAME} would not delete ({e})"));
        }
    }
    let file = match root.open(NAME, FileMode::CreateReadWrite, FileAttribute::empty()) {
        Ok(file) => file,
        Err(e) => return refused(format_args!("{NAME} would not open ({e})")),
    };
    let Some(file) = file.into_regular_file() else {
        return refused(format_args!("{NAME} on the log partition is a directory"));
    };
    // The exclusive open outlives this function: the file handle it produced is
    // written to until ExitBootServices, and the loader never gives the machine
    // back before then.
    core::mem::forget(fs);
    // SAFETY: [`Sink`]'s contract.
    unsafe { *SINK.0.get() = Some(file) };
}

/// One line, on the disk before the caller goes on.
pub fn line(args: fmt::Arguments) {
    // SAFETY: [`Sink`]'s contract.
    let sink = unsafe { &mut *SINK.0.get() };
    let Some(file) = sink.as_mut() else { return };
    let text = alloc::format!("{args}\n");
    let written = file
        .write(text.as_bytes())
        .map_err(|e| e.status())
        .and_then(|()| file.flush().map_err(|e| e.status()));
    if let Err(status) = written {
        // A device that refused a line is not asked for the next one.
        *sink = None;
        refused(format_args!("{NAME} refused a line ({status})"));
    }
}

/// The last line, and then the file: called where boot services end.
pub fn close() {
    println!("{ENDS_AT}");
    // SAFETY: [`Sink`]'s contract. Dropping the handle closes it.
    unsafe { *SINK.0.get() = None };
}

/// The unique GUID of the GPT partition `handle` sits on, as the sixteen bytes
/// the entry itself carries. `None` is a filesystem that is not on one.
fn unique_guid(bs: &BootServices, handle: Handle) -> Option<[u8; 16]> {
    let info = bs.open_protocol_exclusive::<PartitionInfo>(handle).ok()?;
    let entry = info.gpt_partition_entry()?;
    // Copied out of a `repr(packed)` entry, where a reference would be unaligned.
    Some({ entry.unique_partition_guid }.to_bytes())
}

/// Why there is no log, and that the boot goes on without one.
fn refused(why: fmt::Arguments) {
    // The console directly: there is no file, and this says why.
    uefi_services::println!("Loader log: {why} — this boot's loader lines are on the screen only");
}
