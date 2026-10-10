//! The loader's own log, written to the stick as it runs.
//!
//! Every line [`crate::println`] puts on the firmware's console is appended
//! here too, written and flushed before the loader goes on.
//!
//! The file is `loader.log` at the root of the partition
//! `KernelArgs::log_partition_guid` names, truncated at each boot. One file
//! under a fixed name and never one of `logkeeper`'s timestamped ones, so a reader
//! looking for the kernel's log on this volume never picks this up.
//!
//! A partition this cannot open or write is refused by name on the console and
//! the boot continues: the loader's job is the kernel.

use core::cell::UnsafeCell;
use core::fmt;

use crate::efi::{cstr16, BootServices, CStr16, File, Handle, Mode, PartitionInfo, SimpleFileSystem, Status, SystemTable};

/// The loader's first line, which is also the file's: [`open`] runs before it.
pub const BEGINS_AT: &str = "ToyOS Bootloader 1.0";

/// What `query_gop` prints once it has opened the protocol, and the word that
/// tells this line from the kernel's own `GOP:` line on a console with both.
pub const GOP_AT: &str = "GOP: mode";

/// The file's last line: [`close`] runs before the memory map is sized, and a
/// write after that could grow the map the kernel is about to be handed.
const ENDS_AT: &str = "Loader log: the kernel handoff begins, so this file ends here";

/// The last line of a pass that reads the black box and boots no kernel.
pub const ENDS_AT_CHAIN: &str =
    "Loader log: the last boot is accounted for, so this pass resets the machine";

/// Close the file on a pass that boots no kernel. Separate from [`close`]
/// because that one's last line is about a handoff this pass does not make.
pub fn close_without_a_kernel() {
    // SAFETY: [`Sink`]'s contract. Dropping the handle closes it.
    unsafe { *SINK.0.get() = None };
    // SAFETY: [`Volume`]'s contract.
    unsafe { *VOLUME.0.get() = None };
}

const NAME: &CStr16 = cstr16!("loader.log");

/// The open file, from [`open`] until [`close`].
///
/// A UEFI application owns the machine: one processor, no preemption, and
/// nothing here runs from a firmware callback, so the cell has one caller at a
/// time and no borrow of its contents outlives the call that took it.
struct Sink(UnsafeCell<Option<File>>);

// SAFETY: [`Sink`]'s own contract; nothing else in this crate names the type.
unsafe impl Sync for Sink {}

static SINK: Sink = Sink(UnsafeCell::new(None));

/// The log partition's root, kept open beside [`SINK`] for the same span and
/// under the same contract: the one way to write another file on the volume
/// once [`open`] holds it exclusively ([`with_open_volume`]).
struct Volume(UnsafeCell<Option<File>>);

// SAFETY: [`Sink`]'s contract, which this shares.
unsafe impl Sync for Volume {}

static VOLUME: Volume = Volume(UnsafeCell::new(None));

/// What a pass that appends writes before its own first line, so the boot being
/// reported on and the pass reporting on it are never read as one.
pub const SEPARATOR: &str = "--- the pass after the reset, reading what the boot above left";

/// The handle of the filesystem on the partition `guid` names, or why this
/// machine has none.
///
/// **The one lookup.** A second reader of that volume finds it by this rule or
/// by none, so it cannot end up reading a different partition than the log does.
pub fn volume_handle(
    bs: &BootServices,
    guid: &[u8; 16],
) -> Result<Handle, alloc::string::String> {
    let Ok(handles) = bs.handles::<SimpleFileSystem>() else {
        return Err("this machine publishes no filesystem at all".into());
    };
    let mut on_gpt = 0usize;
    let found = handles.iter().find(|handle| match unique_guid(bs, **handle) {
        Some(unique) => {
            on_gpt += 1;
            unique == *guid
        }
        None => false,
    });
    match found {
        Some(&handle) => Ok(handle),
        None => Err(match on_gpt {
            0 => "no filesystem here sits on a GPT partition".into(),
            n => alloc::format!("none of this machine's {n} GPT filesystems is {guid:02x?}"),
        }),
    }
}

/// Open the log partition's root, hand it to `visit`, and **release the
/// protocol when it returns** — which is what makes this usable before [`open`]
/// takes the same handle exclusively and keeps it for the rest of the pass.
pub fn with_volume<T>(
    system_table: &SystemTable,
    guid: &[u8; 16],
    visit: impl FnOnce(&mut File) -> T,
) -> Result<T, alloc::string::String> {
    let bs = system_table.boot_services();
    let handle = volume_handle(bs, guid)?;
    let mut fs = bs
        .exclusive::<SimpleFileSystem>(handle)
        .map_err(|e| alloc::format!("the log partition would not open ({e})"))?;
    let mut root = fs
        .open_volume()
        .map_err(|e| alloc::format!("the log partition has no volume ({e})"))?;
    Ok(visit(&mut root))
}

/// Hand the log partition's root to `visit`, once [`open`] holds the volume;
/// `Err` where it does not, which is a pass whose log never opened.
pub fn with_open_volume<T>(visit: impl FnOnce(&mut File) -> T) -> Result<T, alloc::string::String> {
    // SAFETY: [`Volume`]'s contract: one caller at a time, and no borrow
    // outlives this call.
    let volume = unsafe { &mut *VOLUME.0.get() };
    match volume.as_mut() {
        Some(root) => Ok(visit(root)),
        None => Err("the log partition is not open".into()),
    }
}

pub fn open(system_table: &SystemTable, guid: &[u8; 16], truncate: bool) {
    let bs = system_table.boot_services();
    let handle = match volume_handle(bs, guid) {
        Ok(handle) => handle,
        Err(why) => return refused(format_args!("{why}")),
    };
    let mut fs = match bs.exclusive::<SimpleFileSystem>(handle) {
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
        match root.open(NAME, Mode::ReadWrite) {
            Ok(stale) => {
                if let Err(e) = stale.delete() {
                    return refused(format_args!("the last boot's {NAME} would not delete ({e})"));
                }
            }
            Err(Status::NOT_FOUND) => {}
            Err(e) => return refused(format_args!("the last boot's {NAME} would not open ({e})")),
        }
    }
    let file = match root.open(NAME, Mode::CreateReadWrite) {
        Ok(file) => file,
        Err(e) => return refused(format_args!("{NAME} would not open ({e})")),
    };
    let Some(file) = file.into_regular_file() else {
        return refused(format_args!("{NAME} on the log partition is a directory"));
    };
    #[expect(clippy::disallowed_methods, reason = "the exclusive open outlives this function, until `close`")]
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
    // SAFETY: [`Volume`]'s contract.
    unsafe { *VOLUME.0.get() = Some(root) };
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
    let written = file.write(text.as_bytes()).and_then(|()| file.flush().map_err(|status| (status, text.len())));
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
    // SAFETY: [`Volume`]'s contract.
    unsafe { *VOLUME.0.get() = None };
}

/// The unique GUID of the GPT partition `handle` sits on. `None` is a handle
/// that publishes no partition record, or one on a table that is not GPT.
fn unique_guid(bs: &BootServices, handle: Handle) -> Option<[u8; 16]> {
    bs.get::<PartitionInfo>(handle).ok()?.gpt_unique_guid()
}

/// Why there is no log, and that the boot goes on without one.
fn refused(why: fmt::Arguments) {
    // The console directly: there is no file, and this says why.
    crate::efi::print(format_args!("{} Loader log: {why}. This boot's loader lines are on the screen only\n", crate::stamp::now()));
}
