//! What a syscall does to the object a handle names.
//!
//! Every function dispatches on [`KObjectRef`] with no `_` arm, so a new
//! object type is a compile error here. Authorization is not here: the
//! caller has already resolved the handle with the rights the call needs.

use alloc::sync::Arc;
use alloc::vec::Vec;

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::{FileType, OpenFlags, SeekFrom, SyscallError};

use crate::drivers::serial;
use crate::file_cache;
use crate::time::Deadline;
use crate::pipe::{self, PipeId};
use crate::process::PipeMap;
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::watch::Watch;
use crate::{device as device_registry, keyboard, mouse};

use super::device::DeviceClaim;
use super::file::{FileObject, OpenFileState};
use super::handle::{HandleEntry, HandleTable};
use super::KObjectRef;

/// What a freshly created object's one handle carries.
pub fn initial_rights(object: &KObjectRef) -> Rights {
    const BASE: Rights = Rights::DUP.union(Rights::TRANSFER).union(Rights::WAIT);
    match object {
        // `MAP` is `SYS_PIPE_MAP`: either end may window the pipe's ring page.
        KObjectRef::PipeRead(_) => BASE.union(Rights::READ).union(Rights::MAP),
        KObjectRef::PipeWrite(_) => BASE.union(Rights::WRITE).union(Rights::MAP),
        KObjectRef::Connection(_) => {
            BASE.union(Rights::READ).union(Rights::WRITE).union(Rights::MAP)
        }
        KObjectRef::File(_) => BASE.union(Rights::READ).union(Rights::WRITE),
        // No `DUP`: a claim admits exactly one handle, exclusivity by type rather than a check in `dup`.
        KObjectRef::Device(_) => {
            Rights::TRANSFER.union(Rights::WAIT).union(Rights::READ).union(Rights::WRITE)
        }
        KObjectRef::Console(_) => BASE.union(Rights::READ).union(Rights::WRITE),
        KObjectRef::Acceptor(_) => BASE.union(Rights::READ),
        KObjectRef::Inbox(_) => {
            BASE.union(Rights::READ).union(Rights::WRITE).union(Rights::MAP)
        }
        // Every `SysCap` bit is authority init decides per program: no default, the creator states it.
        KObjectRef::SysCap(_) => Rights::NONE,
        // `MAP` is the whole of it: a region is examined through the memory, not the handle.
        KObjectRef::SharedMem(_) => {
            Rights::DUP.union(Rights::TRANSFER).union(Rights::MAP)
        }
        // A connector has no read/write path: put it in a namespace, or give the namespace away.
        KObjectRef::Connector(_) => Rights::DUP.union(Rights::TRANSFER),
        // `READ` is what resolving a name through it, and narrowing into a child's, both take.
        KObjectRef::Namespace(_) => Rights::DUP.union(Rights::TRANSFER).union(Rights::READ),
        // A spawner gets everything a child handle offers: exit code, kill, and accounting.
        KObjectRef::Process(_) => BASE.union(Rights::READ).union(Rights::MANAGE),
    }
}

/// Install a new object at the next free slot, with the rights its type gets.
pub fn install(table: &mut HandleTable, object: KObjectRef) -> Result<RawHandle, SyscallError> {
    let rights = initial_rights(&object);
    table
        .install(HandleEntry::new(object, rights))
        .map_err(|_| SyscallError::ResourceExhausted)
}

/// Every bit `OpenFlags` defines; `READ` is among them although nothing asks for
/// it, because the word is validated in both directions or in neither.
/// Hand-copied from `toyos-abi` and unchecked, for the reason
/// `arch/syscall/vm.rs`'s `MMAP_PROT_KNOWN` gives for all four of these masks.
const OPEN_FLAGS_KNOWN: u64 = OpenFlags::READ.0
    | OpenFlags::WRITE.0
    | OpenFlags::CREATE.0
    | OpenFlags::TRUNCATE.0
    | OpenFlags::APPEND.0;

/// A file opened at absolute `path`, installed in `table`.
pub fn open(table: &mut HandleTable, path: &str, flags: OpenFlags) -> u64 {
    // First, so the answer is the bit and not the path's own refusal.
    if flags.0 & !OPEN_FLAGS_KNOWN != 0 {
        return SyscallError::InvalidArgument.to_u64();
    }
    let writable = flags.contains(OpenFlags::WRITE);
    let create = flags.contains(OpenFlags::CREATE);
    let truncate = flags.contains(OpenFlags::TRUNCATE);
    let append = flags.contains(OpenFlags::APPEND);
    let modifies = writable || create || truncate || append;

    let opened = {
        let mut vfs = crate::vfs::lock();
        // Scoped to this block: dropped before the object exists, since `OpenFileState::Drop` re-takes it.

        let intent = if modifies {
            crate::vfs::ResolveIntent::UserModify
        } else {
            crate::vfs::ResolveIntent::KernelOrRead
        };
        let target = match vfs.resolve_for_open(path, intent) {
            Ok(target) => target,
            Err(e) => return e.to_u64(),
        };

        if create {
            let (_, file) = vfs.resolve_path("/", target.as_str());
            if file.is_empty() {
                return SyscallError::InvalidArgument.to_u64();
            }
        }

        let built = if truncate && create {
            let mtime = crate::clock::nanos_since_boot();
            // `NotFound` is not a failure: truncating past a name that was not there is fine.
            // Any `vfs.delete` error other than `NotFound` is propagated, not swallowed: truncating past it could silently create a file over one the mount could not confirm was missing.
            match vfs.delete(target.as_str()) {
                Ok(()) | Err(SyscallError::NotFound) => {}
                Err(e) => return e.to_u64(),
            }
            vfs.create_file(target.as_str(), mtime).map(|file_id| (file_id, mtime, 0))
        } else {
            // `mtime_target` takes no reference, so it runs first and the reference-taking
            // `open_target` runs last — a refusal cannot strand a reference. `CREATE` acts on `NotFound` only.
            match vfs.mtime_target(&target) {
                Ok(mtime) => vfs.open_target(&target).map(|file_id| {
                    let position =
                        if append { file_cache::size(file_id) as usize } else { 0 };
                    (file_id, mtime, position)
                }),
                Err(SyscallError::NotFound) if create => {
                    let mtime = crate::clock::nanos_since_boot();
                    vfs.create_file(target.as_str(), mtime).map(|file_id| (file_id, mtime, 0))
                }
                Err(e) => Err(e),
            }
        };
        built.map(|(file_id, mtime, position)| (target, file_id, mtime, position))
    };

    let (target, file_id, mtime, position) = match opened {
        Ok(v) => v,
        Err(e) => return e.to_u64(),
    };
    let object = KObjectRef::File(FileObject::new(OpenFileState {
        path: target.into_string(),
        file_id,
        position,
        mtime,
    }));
    // `writable` is a right, not a field: a read-only write fails for lacking `WRITE`.
    let mut rights = initial_rights(&object);
    if !writable {
        rights = rights.without(Rights::WRITE);
    }
    match table.install(HandleEntry::new(object, rights)) {
        Ok(h) => h.0 as u64,
        Err(_) => SyscallError::ResourceExhausted.to_u64(),
    }
}

/// Release one handle.
///
/// What the object holds is released by its own zero-handle hook; `close` releases only the two things that are the process's, not the object's (pipe-map windows, polls on a watch it ends).
pub fn close(
    table: &mut HandleTable,
    h: RawHandle,
    pipe_maps: &mut Vec<PipeMap>,
) -> Result<(), super::HandleError> {
    let entry = table.remove(h)?;
    let object = entry.object().clone();
    // The decrement, and any deferred hook it enqueues, run with the table's borrow already released.
    drop(entry);
    // A map's warrant is the handle: past the last one naming this pipe, revoke its windows.
    for id in [pipe_id_read(&object), pipe_id_write(&object)].into_iter().flatten() {
        let still_held = table.iter().any(|(_, e)| {
            pipe_id_read(e.object()) == Some(id) || pipe_id_write(e.object()) == Some(id)
        });
        if !still_held {
            if let Some(pt) = crate::scheduler::current_address_space() {
                crate::process::revoke_pipe_maps(pipe_maps, &pt, id);
            }
        }
    }
    // Only a watch this handle's object ends is answered: one it shares (every `Console` and a
    // `Device(Keyboard)` name the keyboard's) cannot cancel another's poll.
    if close_ends_polls(&object) {
        for watch in [read_watch(&object), write_watch(&object)].into_iter().flatten() {
            watch.cancel_polls();
        }
    }
    Ok(())
}


/// Release every handle a process holds; runs on the kill path too, which does not unwind.
///
/// `close_all` takes no `pipe_maps` argument: its only caller is process teardown, which destroys the whole address space the windows are in.
pub fn close_all(table: &mut HandleTable) {
    for entry in table.drain() {
        drop(entry);
    }
}

pub fn pipe_id_read(object: &KObjectRef) -> Option<PipeId> {
    match object {
        KObjectRef::PipeRead(r) => Some(r.id()),
        KObjectRef::Connection(c) => Some(c.rx()),
        KObjectRef::PipeWrite(_) | KObjectRef::File(_) | KObjectRef::Device(_)
        | KObjectRef::Console(_) | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) => None,
    }
}

pub fn pipe_id_write(object: &KObjectRef) -> Option<PipeId> {
    match object {
        KObjectRef::PipeWrite(w) => Some(w.id()),
        KObjectRef::Connection(c) => Some(c.tx()),
        KObjectRef::PipeRead(_) | KObjectRef::File(_) | KObjectRef::Device(_)
        | KObjectRef::Console(_) | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) => None,
    }
}

/// A watch as a poll registration holds it: a static one's reference, or a
/// share of one that goes with its object.
pub enum WatchRef {
    Static(&'static Watch),
    Shared(Arc<Watch>),
}

impl core::ops::Deref for WatchRef {
    type Target = Watch;
    fn deref(&self) -> &Watch {
        match self {
            Self::Static(watch) => watch,
            Self::Shared(watch) => watch,
        }
    }
}

/// The watch a readable poll on this object registers on, or `None` when no
/// readiness of that direction exists.
pub fn read_watch(object: &KObjectRef) -> Option<WatchRef> {
    match object {
        KObjectRef::PipeRead(r) => pipe::read_watch(r.id()).map(WatchRef::Shared),
        KObjectRef::Connection(c) => pipe::read_watch(c.rx()).map(WatchRef::Shared),
        KObjectRef::Acceptor(a) => Some(WatchRef::Shared(a.watch().clone())),
        KObjectRef::Console(_) => Some(WatchRef::Static(&keyboard::WATCH)),
        KObjectRef::Device(d) => match d.class() {
            device_registry::DeviceType::Keyboard => Some(WatchRef::Static(&keyboard::WATCH)),
            device_registry::DeviceType::Mouse => Some(WatchRef::Static(&mouse::WATCH)),
            device_registry::DeviceType::PciFunction => {
                d.pci_slot().map(|slot| WatchRef::Static(crate::pcidev::watch(slot)))
            }
            device_registry::DeviceType::HdaAudio | device_registry::DeviceType::VirtioSound => {
                Some(WatchRef::Static(&crate::drivers::AUDIO_WATCH))
            }
            device_registry::DeviceType::Framebuffer => None,
            // A partition answers its description and has nothing to wait for.
            device_registry::DeviceType::Partition => None,
        },
        // Named unconditionally: the watch alone cannot enforce rights.
        KObjectRef::SysCap(_) => Some(WatchRef::Static(&crate::log::user::WATCH)),
        KObjectRef::PipeWrite(_) | KObjectRef::File(_) | KObjectRef::Inbox(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) => None,
    }
}

/// The watch a writable poll on this object registers on.
pub fn write_watch(object: &KObjectRef) -> Option<WatchRef> {
    match object {
        KObjectRef::PipeWrite(w) => pipe::write_watch(w.id()).map(WatchRef::Shared),
        KObjectRef::Connection(c) => pipe::write_watch(c.tx()).map(WatchRef::Shared),
        KObjectRef::Console(_) => Some(WatchRef::Static(&crate::log::console::SPACE)),
        KObjectRef::PipeRead(_) | KObjectRef::File(_) | KObjectRef::Device(_)
        | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) => None,
    }
}

/// Whether closing one handle to this object ends what its watches watch, so
/// every poll on them — in any ring — is answered as gone. `false` for the log
/// and the keyboard, which the machine ends on its own and which other handles
/// share: a console closing is not every console's keyboard going away.
fn close_ends_polls(object: &KObjectRef) -> bool {
    match object {
        KObjectRef::SysCap(_) => crate::actuator::log_close_cancels_any_syscap(),
        // A keyboard *claim* closing is the stimulus, not a `SysCap`.
        KObjectRef::Console(_) => crate::actuator::keyboard_close_cancels_every_console(),
        KObjectRef::Device(d) => match d.class() {
            device_registry::DeviceType::Keyboard => {
                crate::actuator::keyboard_close_cancels_every_console()
            }
            device_registry::DeviceType::Mouse
            | device_registry::DeviceType::PciFunction
            | device_registry::DeviceType::HdaAudio
            | device_registry::DeviceType::VirtioSound
            | device_registry::DeviceType::Framebuffer
            | device_registry::DeviceType::Partition => true,
        },
        KObjectRef::PipeRead(_) | KObjectRef::PipeWrite(_) | KObjectRef::Connection(_)
        | KObjectRef::Acceptor(_) | KObjectRef::File(_) | KObjectRef::Inbox(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) => true,
    }
}

fn read_file(file: &FileObject, buf: &mut UserBytesMut) -> Option<u64> {
    file.with(|state| {
        let size = file_cache::size(state.file_id) as usize;
        let available = size.saturating_sub(state.position);
        let count = buf.len().min(available);
        if count == 0 {
            return Some(0);
        }
        let mut read = 0;
        let mut refused = false;
        while read < count {
            let abs_pos = state.position + read;
            let page_idx = (abs_pos / 4096) as u32;
            let offset_in_page = abs_pos % 4096;
            let remaining_in_page = 4096 - offset_in_page;
            let to_read = remaining_in_page.min(count - read);
            // A refused page is not a page of zeros: stop short rather than fake a hole under a success.
            if file_cache::read_page(
                state.file_id,
                page_idx,
                offset_in_page,
                &mut buf.sub(read, to_read),
            )
            .is_err()
            {
                refused = true;
                break;
            }
            read += to_read;
        }
        if read == 0 && refused {
            return Some(SyscallError::Io.to_u64());
        }
        state.position += read;
        Some(read as u64)
    })
}

/// Read a device claim; takes `table` because describing a device installs handles into it.
pub fn read_device(
    claim: &DeviceClaim,
    table: &mut HandleTable,
    buf: &mut UserBytesMut,
) -> Option<u64> {
    match claim.class() {
        // Reads the queue only and drives no hardware: polling the controller here can block on its recovery engine.
        // `drain_irqs` calls this same read at the top of every scheduler pass, so skipping the poll here still bounds staleness to one scheduler pass.
        device_registry::DeviceType::Keyboard | device_registry::DeviceType::Mouse => {
            match claim.class() {
            device_registry::DeviceType::Keyboard => {
                let event_size = core::mem::size_of::<keyboard::RawKeyEvent>();
                let mut count = 0;
                while count + event_size <= buf.len() {
                    let Some(event) = keyboard::try_read_event() else { break };
                    buf.write_at(count, event.as_bytes());
                    count += event_size;
                }
                if count > 0 { Some(count as u64) } else { None }
            }
            device_registry::DeviceType::Mouse => {
                let event_size = core::mem::size_of::<mouse::MouseEvent>();
                let mut count = 0;
                while count + event_size <= buf.len() {
                    let Some(event) = mouse::try_read_event() else { break };
                    buf.write_at(count, event.as_bytes());
                    count += event_size;
                }
                if count > 0 { Some(count as u64) } else { None }
            }
            other => panic!("a {other:?} claim answers with events"),
            }
        }
        device_registry::DeviceType::Framebuffer => Some(claim.describe(table, buf)),
        // Every read is the description: a partition's bytes move through
        // `SYS_PARTITION_READ`, never through a read of the claim.
        device_registry::DeviceType::Partition => Some(claim.describe(table, buf)),
        // The description first and interrupts after, the shape the HDA stub
        // has: a driver reads what it is driving once, and everything it reads
        // afterwards is what its device has been doing.
        device_registry::DeviceType::PciFunction => {
            if !claim.info_read() {
                return Some(claim.describe(table, buf));
            }
            if buf.len() < toyos_abi::pci::DeviceIrqRecord::SIZE {
                return Some(SyscallError::InvalidArgument.to_u64());
            }
            let slot = claim.pci_slot().expect("a PCI claim knows its slot");
            let record = match crate::pcidev::take_record(slot) {
                Ok(record) => record?,
                Err(refused) => return Some(refused.to_u64()),
            };
            buf.write_at(0, record_bytes(&record));
            Some(toyos_abi::pci::DeviceIrqRecord::SIZE as u64)
        }
        device_registry::DeviceType::HdaAudio => {
            if !claim.info_read() {
                return Some(claim.describe(table, buf));
            }
            if buf.len() < toyos_abi::audio::AudioCompletionRecord::SIZE {
                return Some(SyscallError::InvalidArgument.to_u64());
            }
            let n = crate::drivers::hda::drain_completed(buf);
            if n == 0 { None } else { Some(n as u64) }
        }
        device_registry::DeviceType::VirtioSound => {
            if !claim.info_read() {
                return Some(claim.describe(table, buf));
            }
            if buf.len() < toyos_abi::audio::AudioCompletionRecord::SIZE {
                return Some(SyscallError::InvalidArgument.to_u64());
            }
            // Completion records, oldest first; empty answers `None` so a blocking read parks.
            let n = crate::drivers::virtio_sound::drain_completed(buf);
            if n == 0 { None } else { Some(n as u64) }
        }
    }
}

/// One interrupt record as the bytes that cross the boundary.
///
/// Every byte belongs to a field — the record's own `const _` in `toyos-abi`
/// proves the layout has no gap — so nothing of this kernel's stack is
/// published with it.
fn record_bytes(record: &toyos_abi::pci::DeviceIrqRecord) -> &[u8] {
    // SAFETY: `record` is a live `&DeviceIrqRecord`, readable for its own size,
    // and the layout assertion beside its declaration proves every byte of that
    // width is an initialised field.
    unsafe {
        core::slice::from_raw_parts(
            record as *const _ as *const u8,
            toyos_abi::pci::DeviceIrqRecord::SIZE,
        )
    }
}

/// Read whatever a handle names except a device claim; [`read_device`] needs the table mutably.
pub fn try_read(object: &KObjectRef, buf: &mut UserBytesMut) -> Option<u64> {
    match object {
        KObjectRef::File(f) => read_file(f, buf),
        KObjectRef::PipeRead(r) => pipe::try_read(r.id(), buf).map(|n| n as u64),
        KObjectRef::Connection(c) => pipe::try_read(c.rx(), buf).map(|n| n as u64),
        KObjectRef::Device(_) => unreachable!("a device claim is read by `read_device`"),
        KObjectRef::Console(_) => {
            let mut count = 0usize;
            while count < buf.len() {
                if let Some(b) = serial::try_read_byte() {
                    buf.write_at(count, &[b]);
                    count += 1;
                    if b == b'\n' || b == b'\r' {
                        break;
                    }
                } else if count > 0 {
                    break;
                } else {
                    return None;
                }
            }
            Some(count as u64)
        }
        KObjectRef::PipeWrite(_) | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) => Some(SyscallError::PermissionDenied.to_u64()),
    }
}

fn write_pipe(id: PipeId, buf: &UserBytes) -> Option<u64> {
    match pipe::try_write(id, buf) {
        // The reader is gone, not absent: `NotFound` means the name is not there and nothing else may say so.
        Some(pipe::PipeWrite::BrokenPipe) => Some(SyscallError::Gone.to_u64()),
        Some(pipe::PipeWrite::NoMemory) => Some(SyscallError::ResourceExhausted.to_u64()),
        Some(pipe::PipeWrite::Wrote(n)) => Some(n as u64),
        None => None,
    }
}

pub fn try_write(object: &KObjectRef, buf: &UserBytes) -> Option<u64> {
    match object {
        KObjectRef::File(f) => f.with(|state| {
            // Refused, never wrapped: past this the `u32` page index would alias a low page.
            if (state.position as u64).saturating_add(buf.len() as u64) > file_cache::MAX_FILE_SIZE {
                return Some(SyscallError::InvalidArgument.to_u64());
            }
            let mut written = 0;
            let mut refused = false;
            while written < buf.len() {
                let abs_pos = state.position + written;
                let page_idx = (abs_pos / 4096) as u32;
                let offset_in_page = abs_pos % 4096;
                let remaining_in_page = 4096 - offset_in_page;
                let to_write = remaining_in_page.min(buf.len() - written);
                // A page that cannot be re-read off the device is refused, not merged into zeros.
                if file_cache::write_page(
                    state.file_id,
                    page_idx,
                    offset_in_page,
                    &buf.sub(written, to_write),
                )
                .is_err()
                {
                    refused = true;
                    break;
                }
                written += to_write;
            }
            if written == 0 && refused {
                return Some(SyscallError::Io.to_u64());
            }
            state.position += written;
            // Dirty state lives in the cache now, set by `write_page`; the handle keeps only the mtime.
            state.mtime = crate::clock::nanos_since_boot();
            Some(written as u64)
        }),
        KObjectRef::PipeWrite(w) => write_pipe(w.id(), buf),
        KObjectRef::Connection(c) => write_pipe(c.tx(), buf),
        // The whole lines `klogd`'s queue has room for, and a trailing partial one:
        // a short count is a writer ahead of the console, told so.
        KObjectRef::Console(c) => Some(c.write(buf) as u64),
        KObjectRef::PipeRead(_) | KObjectRef::Device(_) | KObjectRef::Acceptor(_)
        | KObjectRef::Inbox(_) | KObjectRef::SharedMem(_) | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::Process(_) => {
            Some(SyscallError::PermissionDenied.to_u64())
        }
    }
}

pub fn seek(object: &KObjectRef, pos: SeekFrom) -> u64 {
    let KObjectRef::File(file) = object else {
        return SyscallError::PermissionDenied.to_u64();
    };
    file.with(|state| {
        let size = file_cache::size(state.file_id) as usize;
        let new_pos = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => (state.position as i64).checked_add(n).unwrap_or(-1),
            SeekFrom::End(n) => (size as i64).checked_add(n).unwrap_or(-1),
        };
        if new_pos < 0 || new_pos as u64 > file_cache::MAX_FILE_SIZE {
            return SyscallError::InvalidArgument.to_u64();
        }
        // Past EOF is a position, not an error (POSIX lseek): a later write extends the file.
        state.position = new_pos as usize;
        state.position as u64
    })
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Stat {
    pub file_type: u64,
    pub size: u64,
    pub mtime: u64,
}

/// What kind of thing this is, and how big.
pub fn fstat(object: &KObjectRef) -> Stat {
    let plain = |t: FileType| Stat { file_type: t as u64, size: 0, mtime: 0 };
    match object {
        KObjectRef::File(f) => f.with(|state| Stat {
            file_type: FileType::File as u64,
            size: file_cache::size(state.file_id),
            mtime: state.mtime,
        }),
        KObjectRef::PipeRead(r) => {
            plain(if r.is_tty() { FileType::Tty } else { FileType::Pipe })
        }
        KObjectRef::PipeWrite(w) => {
            plain(if w.is_tty() { FileType::Tty } else { FileType::Pipe })
        }
        KObjectRef::Connection(_) => plain(FileType::Socket),
        KObjectRef::Console(_) => plain(FileType::Serial),
        KObjectRef::Acceptor(_) => plain(FileType::Pipe),
        KObjectRef::SharedMem(m) => Stat {
            file_type: FileType::SharedMemory as u64,
            size: m.size(),
            mtime: 0,
        },
        KObjectRef::Inbox(_) | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::Process(_) => plain(FileType::Unknown),
        KObjectRef::Device(d) => plain(match d.class() {
            device_registry::DeviceType::Keyboard => FileType::Keyboard,
            device_registry::DeviceType::Mouse => FileType::Mouse,
            device_registry::DeviceType::Framebuffer => FileType::Framebuffer,
            device_registry::DeviceType::PciFunction => FileType::Unknown,
            device_registry::DeviceType::HdaAudio
            | device_registry::DeviceType::VirtioSound => FileType::Unknown,
            device_registry::DeviceType::Partition => FileType::Unknown,
        }),
    }
}

/// `SYS_FSYNC`: the file's bytes on the device, and the device told to commit them.
///
/// The device-commit step is not optional: `/system/bin/logd` calls a line durable off `fsync`'s result, so a flush that stopped at the page cache would make that a claim about nothing.
pub fn fsync(object: &KObjectRef) -> u64 {
    let file = match object {
        KObjectRef::File(file) => file,
        KObjectRef::Device(claim) => return partition_fsync(claim),
        _ => return SyscallError::PermissionDenied.to_u64(),
    };
    let (path, file_id, mtime) =
        file.with(|state| (state.path.clone(), state.file_id, state.mtime));
    // The file's debt or its mount's, not the handle's: another handle's write, and a
    // device commit an earlier attempt failed to deliver, are both still owed here.
    if !crate::vfs::lock().durability_owed(&path, file_id) {
        return 0;
    }
    // A refused attempt can leave the two FATs split, and the park between two attempts is where the machine's stop would find this thread.
    let _update = crate::block::begin_update();
    // A refused attempt discards nothing — an unsettled debt needs no restoring.
    let run = until_answered(|| {
        // Outside `FileObject`'s lock: this and `OpenFileState::drop` take the VFS lock in the same order.
        // Flush and sync share one acquisition so this file cannot be unmounted between them.
        let mut vfs = crate::vfs::lock();
        // Tags the flush as `SYS_FSYNC`'s, for `quiesce-fsync-refuse` to stage on this path.
        #[cfg(feature = "boot-actuators")]
        crate::fat32_adapter::enter_fsync_flush(&path);
        let done = vfs
            .flush_file(&path, file_id, mtime)
            .and_then(|()| vfs.sync_for_path(&path));
        #[cfg(feature = "boot-actuators")]
        crate::fat32_adapter::leave_fsync_flush();
        drop(vfs);
        done
    });
    match run {
        Answered::Answer { answer: Ok(()), attempts, took } => {
            if attempts > 1 {
                crate::log!(
                    "fsync: {path} durable on attempt {attempts} after {took} — a refused \
                     attempt kept every page dirty and a later one delivered them",
                );
            }
            // `flush_file` settled the file's debt and `sync_for_path` the mount's; there is no per-handle flag to clear.
            0
        }
        // The device's own word (an error status, or a recovery that gave up) is passed through unchanged.
        Answered::Answer { answer: Err(e), .. } => e.to_u64(),
        Answered::Killed => SyscallError::WouldBlock.to_u64(),
        Answered::Deadman { attempts, took } => {
            crate::log!(
                "fsync: {path} is not durable after {attempts} attempt(s) in {took} — {}",
                crate::block::DEADMAN,
            );
            SyscallError::Io.to_u64()
        }
    }
}

/// What a run of [`until_answered`]'s attempts came to.
pub(crate) enum Answered {
    /// An attempt answered with a word of the device's own: `Ok`, or a failure
    /// it named.
    Answer { answer: Result<(), SyscallError>, attempts: u32, took: crate::time::Duration },
    /// The caller is being killed; the word dies with its task.
    Killed,
    /// Every attempt until [`crate::block::DEADMAN`] was refused on its budget:
    /// the caller answers `Io`, a device word, rather than another ask-again.
    Deadman { attempts: u32, took: crate::time::Duration },
}

/// `attempt` run until it answers anything but `WouldBlock` — a budget that
/// expired on a live device, never a device fact — each time on a fresh
/// budget, parked between two (`block::between_attempts`), and given up once
/// [`crate::block::DEADMAN`] is spent. The one loop in this kernel that asks a
/// block device again, for a caller holding no spinlock: nothing it holds can
/// be held across the wait, so no disk wait here is under one.
pub(crate) fn until_answered(mut attempt: impl FnMut() -> Result<(), SyscallError>) -> Answered {
    let began = crate::clock::now();
    // Bounds the run of attempts, never a single attempt's elapsed time.
    let deadman = Deadline::at(began + crate::block::DEADMAN.duration());
    #[cfg(feature = "boot-actuators")]
    let deadman = if crate::actuator::fsync_deadman_now() { Deadline::passed() } else { deadman };
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let answer = {
            // Stages a first attempt with its budget already spent, exercising the shipped refusal itself.
            #[cfg(feature = "boot-actuators")]
            let _spent = (attempts == 1 && crate::actuator::fsync_budget_spent())
                .then(|| crate::scheduler::Operation::begin(Deadline::passed()));
            attempt()
        };
        if answer != Err(SyscallError::WouldBlock) {
            return Answered::Answer { answer, attempts, took: crate::clock::now() - began };
        }
        // A killed caller stops retrying at the first safe point. The machine's stop is deliberately not read here: `quiesce` claims every filesystem is synced, and a sync it named may not be abandoned by the stop that is about to make that claim.
        if crate::sched::driver::current_kill_pending() {
            return Answered::Killed;
        }
        if deadman.reached(crate::clock::now()) {
            return Answered::Deadman { attempts, took: crate::clock::now() - began };
        }
        crate::block::between_attempts(attempts);
    }
}

/// A block-device failure as the word a syscall returns: a budget that expired
/// is asked again ([`until_answered`]), anything else is the device's.
pub(crate) fn block_word(e: crate::block::BlockError) -> SyscallError {
    match e {
        crate::block::BlockError::Device => SyscallError::Io,
        crate::block::BlockError::BudgetExpired => SyscallError::WouldBlock,
    }
}

/// `SYS_FSYNC` on a partition claim: every write the claim returned from
/// before this call durable, or the claim told they may not be. The flush is
/// the whole device's, and its answer is the claim's own
/// (`block::Partition::flush`).
fn partition_fsync(claim: &DeviceClaim) -> u64 {
    match claim.class() {
        device_registry::DeviceType::Partition => {}
        device_registry::DeviceType::Keyboard
        | device_registry::DeviceType::Mouse
        | device_registry::DeviceType::Framebuffer
        | device_registry::DeviceType::HdaAudio
        | device_registry::DeviceType::VirtioSound
        | device_registry::DeviceType::PciFunction => {
            return SyscallError::PermissionDenied.to_u64();
        }
    }
    let run = until_answered(|| match claim.partition_view() {
        Some(view) => view.flush().map_err(block_word),
        None => Err(SyscallError::Gone),
    });
    partition_word("a flush", run)
}

/// What a partition claim's run of attempts at `what` answers its caller.
pub(crate) fn partition_word(what: &str, run: Answered) -> u64 {
    match run {
        Answered::Answer { answer: Ok(()), .. } => 0,
        Answered::Answer { answer: Err(e), .. } => e.to_u64(),
        Answered::Killed => SyscallError::WouldBlock.to_u64(),
        Answered::Deadman { attempts, took } => {
            crate::log!(
                "partclaim: {what} still refused after {attempts} attempt(s) in {took} — {}",
                crate::block::DEADMAN,
            );
            SyscallError::Io.to_u64()
        }
    }
}

pub fn ftruncate(object: &KObjectRef, size: u64) -> u64 {
    let KObjectRef::File(file) = object else {
        return SyscallError::PermissionDenied.to_u64();
    };
    if size > file_cache::MAX_FILE_SIZE {
        return SyscallError::InvalidArgument.to_u64();
    }
    let file_id = file.with(|state| state.file_id);
    {
        // The VFS lock outside `FileObject`'s (fsync's order) is `resize`'s witness.
        let mut vfs = crate::vfs::lock();
        // A refused resize changed nothing, so the size stays as it was.
        // A budget expiry is the caller's own bound and not a fact about the device: retryable.
        if let Err(e) = file_cache::resize(&mut vfs, file_id, size) {
            return match e {
                crate::block::BlockError::BudgetExpired => SyscallError::WouldBlock,
                crate::block::BlockError::Device => SyscallError::Io,
            }
            .to_u64();
        }
    }
    // The seek pointer is not touched (POSIX ftruncate): a shrink leaves it past EOF.
    file.with(|state| {
        state.mtime = crate::clock::nanos_since_boot();
        0
    })
}

pub fn has_data(object: &KObjectRef) -> bool {
    match object {
        KObjectRef::PipeRead(r) => pipe::has_data(r.id()),
        KObjectRef::Connection(c) => pipe::has_data(c.rx()),
        KObjectRef::Console(_) => serial::has_data(),
        KObjectRef::Acceptor(a) => a.has_pending(),
        KObjectRef::File(_) => true,
        KObjectRef::Device(d) => match d.class() {
            device_registry::DeviceType::Keyboard => keyboard::has_data(),
            device_registry::DeviceType::Mouse => mouse::has_data(),
            device_registry::DeviceType::PciFunction => {
                !d.info_read() || d.pci_slot().is_some_and(crate::pcidev::has_irq)
            }
            device_registry::DeviceType::Framebuffer => true,
            device_registry::DeviceType::Partition => true,
            device_registry::DeviceType::HdaAudio => {
                !d.info_read() || crate::drivers::hda::has_pending()
            }
            device_registry::DeviceType::VirtioSound => {
                !d.info_read() || crate::drivers::virtio_sound::has_pending()
            }
        },
        KObjectRef::PipeWrite(_) | KObjectRef::Inbox(_) | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) => false,
    }
}

pub fn has_space(object: &KObjectRef) -> bool {
    match object {
        KObjectRef::PipeWrite(w) => pipe::has_space(w.id()),
        KObjectRef::Connection(c) => pipe::has_space(c.tx()),
        KObjectRef::File(_) => true,
        KObjectRef::Console(_) => crate::log::console::has_room(),
        KObjectRef::PipeRead(_) | KObjectRef::Device(_) | KObjectRef::Acceptor(_)
        | KObjectRef::Inbox(_) | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) => false,
    }
}

/// Mark one end of a pipe as a terminal — per end, not per pipe.
pub fn mark_tty(object: &KObjectRef) -> u64 {
    match object {
        KObjectRef::PipeRead(r) => {
            r.mark_tty();
            0
        }
        KObjectRef::PipeWrite(w) => {
            w.mark_tty();
            0
        }
        KObjectRef::Connection(_) | KObjectRef::File(_) | KObjectRef::Device(_)
        | KObjectRef::Console(_) | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_) | KObjectRef::SharedMem(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::Process(_) => SyscallError::InvalidArgument.to_u64(),
    }
}
