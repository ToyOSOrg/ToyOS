//! The loader's own log, written to the stick as it runs.
//!
//! Every line [`crate::println`] puts on the firmware's console is appended
//! here too, written and flushed before the loader goes on.
//!
//! The file is `loader.log` at the root of the partition
//! `KernelArgs::log_partition_guid` names, truncated at each boot. One file
//! under a fixed name and never one of `logd`'s timestamped ones, so a reader
//! looking for the kernel's log on this volume never picks this up.
//!
//! A partition this cannot open or write is refused by name on the console and
//! the boot continues: the loader's job is the kernel.

use core::cell::UnsafeCell;
use core::fmt;

use uefi::proto::media::file::{File, FileAttribute, FileMode, RegularFile};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::proto::media::partition::PartitionInfo;
use uefi::table::boot::{BootServices, OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::{prelude::*, CStr16, Handle};

/// The loader's first line, which is also the file's: [`open`] runs before it.
pub const BEGINS_AT: &str = "ToyOS Bootloader 1.0";

/// What `query_gop` prints once it has opened the protocol, and the word that
/// tells this line from the kernel's own `GOP:` line on a console with both.
pub const GOP_AT: &str = "GOP: mode";

/// The file's last line: [`close`] runs before the memory map is sized, and a
/// write after that could grow the map the kernel is about to be handed.
const ENDS_AT: &str = "Loader log: the kernel handoff begins, so this file ends here";

/// The last line of a pass that reads the black box and boots no kernel.
///
/// It says the reset and not a return, because the reset is what it does:
/// returning is what leaves this image's exit-boot-services callback registered
/// for the next operating system to call into, and `end_this_pass` exists to
/// not do it. A line describing the shape this code was written to avoid is a
/// line that will be read as evidence one day.
pub const ENDS_AT_CHAIN: &str =
    "Loader log: the last boot is accounted for, so this pass resets the machine";

/// Close the file on a pass that boots no kernel. Separate from [`close`]
/// because that one's last line is about a handoff this pass does not make.
pub fn close_without_a_kernel() {
    // SAFETY: [`Sink`]'s contract. Dropping the handle closes it.
    unsafe { *SINK.0.get() = None };
}

const NAME: &CStr16 = cstr16!("loader.log");

/// The open file, from [`open`] until [`close`].
///
/// A UEFI application owns the machine: one processor, no preemption, and
/// nothing here runs from a firmware callback, so the cell has one caller at a
/// time and no borrow of its contents outlives the call that took it.
struct Sink(UnsafeCell<Option<RegularFile>>);

// SAFETY: [`Sink`]'s own contract; nothing else in this crate names the type.
unsafe impl Sync for Sink {}

static SINK: Sink = Sink(UnsafeCell::new(None));

/// Open `loader.log` on the partition `guid` names, discarding what an earlier
/// boot left there.
/// What a pass that appends writes before its own first line, so the boot being
/// reported on and the pass reporting on it are never read as one.
pub const SEPARATOR: &str = "--- the pass after the reset, reading what the boot above left";

/// `truncate` replaces what the last boot left; a pass that appends is one that
/// has a *report about* that boot, and the boot's own account has to stay
/// readable under it. One file for now: per-pass names are their own change.
pub fn open(system_table: &SystemTable<Boot>, guid: &[u8; 16], truncate: bool) {
    let bs = system_table.boot_services();
    let Ok(handles) = bs.locate_handle_buffer(SearchType::from_proto::<SimpleFileSystem>()) else {
        return refused(format_args!("this machine publishes no filesystem at all"));
    };
    let mut on_gpt = 0usize;
    let found = handles.iter().find(|handle| match unique_guid(bs, **handle) {
        Some(unique) => {
            on_gpt += 1;
            unique == *guid
        }
        None => false,
    });
    let Some(&handle) = found else {
        return match on_gpt {
            0 => refused(format_args!("no filesystem here sits on a GPT partition")),
            n => refused(format_args!("none of this machine's {n} GPT filesystems is {guid:02x?}")),
        };
    };
    let mut fs = match bs.open_protocol_exclusive::<SimpleFileSystem>(handle) {
        Ok(fs) => fs,
        Err(e) => return refused(format_args!("the log partition would not open ({e})")),
    };
    let mut root = match fs.open_volume() {
        Ok(root) => root,
        Err(e) => return refused(format_args!("the log partition has no volume ({e})")),
    };
    // Deleted and not rewound: `CreateReadWrite` opens what is already there at
    // offset zero without truncating it, so a shorter boot than the last would
    // end in the last one's tail.
    if truncate {
        match root.open(NAME, FileMode::ReadWrite, FileAttribute::empty()) {
            Ok(stale) => {
                if let Err(e) = stale.delete() {
                    return refused(format_args!("the last boot's {NAME} would not delete ({e})"));
                }
            }
            Err(e) if e.status() == Status::NOT_FOUND => {}
            Err(e) => return refused(format_args!("the last boot's {NAME} would not open ({e})")),
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
    // written to until [`close`], and the loader never gives the machine back
    // before then.
    core::mem::forget(fs);
    let mut file = file;
    if !truncate {
        // `EFI_FILE_POSITION_END_OF_FILE` (UEFI 2.10 §13.5.13): the one seek
        // that does not need the length read first.
        if let Err(e) = file.set_position(u64::MAX) {
            return refused(format_args!("{NAME} would not seek to its end ({e})"));
        }
    }
    // SAFETY: [`Sink`]'s contract.
    unsafe { *SINK.0.get() = Some(file) };
    if !truncate {
        println!("{SEPARATOR}");
    }
}

pub fn line(args: fmt::Arguments) {
    // SAFETY: [`Sink`]'s contract.
    let sink = unsafe { &mut *SINK.0.get() };
    let Some(file) = sink.as_mut() else { return };
    let text = alloc::format!("{args}\n");
    // A short write is a truncated line, and the count firmware did write is
    // the error's payload rather than its status.
    let written = match file.write(text.as_bytes()) {
        Ok(()) => file.flush().map_err(|e| (e.status(), text.len())),
        Err(e) => Err((e.status(), *e.data())),
    };
    if let Err((status, wrote)) = written {
        // Taken out of the sink before `refused` runs: nothing may re-enter
        // `line` while this `&mut` is live.
        *sink = None;
        refused(format_args!("{NAME} took {wrote} of {} bytes and then {status}", text.len()));
    }
}

pub fn close() {
    println!("{ENDS_AT}");
    // SAFETY: [`Sink`]'s contract. Dropping the handle closes it.
    unsafe { *SINK.0.get() = None };
}

/// The unique GUID of the GPT partition `handle` sits on. `None` is a handle
/// that publishes no partition record, or one on a table that is not GPT.
fn unique_guid(bs: &BootServices, handle: Handle) -> Option<[u8; 16]> {
    // `GetProtocol`, never `Exclusive`: this runs over every filesystem on the
    // machine, and EXCLUSIVE would call `Stop` on whatever driver holds each.
    //
    // SAFETY: `open_protocol`'s obligation is that this handle and its protocol
    // stay installed until the `ScopedProtocol` drops. Nothing between the two
    // can uninstall either: the loader is the one image running, it registers
    // no event callback, and it calls no boot service that connects or
    // disconnects a controller.
    let info = unsafe {
        bs.open_protocol::<PartitionInfo>(
            OpenProtocolParams { handle, agent: bs.image_handle(), controller: None },
            OpenProtocolAttributes::GetProtocol,
        )
    }
    .ok()?;
    let entry = info.gpt_partition_entry()?;
    // A `repr(packed)` entry, where a reference into it would be unaligned.
    Some({ entry.unique_partition_guid }.to_bytes())
}

/// Why there is no log, and that the boot goes on without one.
fn refused(why: fmt::Arguments) {
    // The console directly: there is no file, and this says why.
    uefi_services::println!("Loader log: {why}. This boot's loader lines are on the screen only");
}
