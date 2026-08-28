//! What a syscall does to the object a handle names.
//!
//! Every function dispatches on [`KObjectRef`] with no `_` arm, so a new
//! object type is a compile error here. Authorization is not here: the
//! caller has already resolved the handle with the rights the call needs.

use alloc::vec::Vec;

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::{FileType, OpenFlags, SeekFrom, SyscallError};

use crate::drivers::serial;
use crate::file_cache;
use crate::time::Deadline;
use crate::inbox::Source;
use crate::pipe::{self, PipeId};
use crate::process::PipeMap;
use crate::user_ptr::{UserBytes, UserBytesMut};
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

/// A file opened at absolute `path`, installed in `table`.
pub fn open(table: &mut HandleTable, path: &str, flags: OpenFlags) -> u64 {
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
            // `CREATE` acts on `NotFound` and nothing else — not on every refusal a mount can return.
            match vfs.open_target(&target) {
                Ok(file_id) => vfs.mtime_target(&target).map(|mtime| {
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
/// What the object holds is released by its own zero-handle hook; `close` releases only the two things that are the process's, not the object's (pipe-map windows, ended sources).
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
    // Only `EndedSource` reaches `cancel_by_source`: a source this handle does not solely own
    // (e.g. `Console` and `Device(Keyboard)` both naming `Source::Keyboard`) cannot cancel another's poll.
    let sources = [read_source(&object), write_source(&object)]
        .map(|s| s.and_then(Source::ended_by_its_last_handle));
    if sources.iter().any(|s| s.is_some()) {
        crate::inbox::cancel_by_source(&sources);
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

pub fn read_source(object: &KObjectRef) -> Option<Source> {
    match object {
        KObjectRef::PipeRead(r) => Some(Source::PipeReadable(r.id())),
        KObjectRef::Connection(c) => Some(Source::PipeReadable(c.rx())),
        KObjectRef::Acceptor(a) => Some(Source::Port(a.port())),
        KObjectRef::Console(_) => Some(Source::Keyboard),
        KObjectRef::Device(d) => match d.class() {
            device_registry::DeviceType::Keyboard => Some(Source::Keyboard),
            device_registry::DeviceType::Mouse => Some(Source::Mouse),
            device_registry::DeviceType::Nic => Some(Source::Network),
            device_registry::DeviceType::HdaAudio => Some(Source::Hda),
            device_registry::DeviceType::VirtioSound => Some(Source::VirtioSound),
            device_registry::DeviceType::Framebuffer => None,
        },
        // Named unconditionally: the source alone cannot enforce rights.
        KObjectRef::SysCap(_) => Some(Source::Log),
        KObjectRef::PipeWrite(_) | KObjectRef::File(_) | KObjectRef::Inbox(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) => None,
    }
}

pub fn write_source(object: &KObjectRef) -> Option<Source> {
    match object {
        KObjectRef::PipeWrite(w) => Some(Source::PipeWritable(w.id())),
        KObjectRef::Connection(c) => Some(Source::PipeWritable(c.tx())),
        KObjectRef::PipeRead(_) | KObjectRef::File(_) | KObjectRef::Device(_)
        | KObjectRef::Console(_) | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) => None,
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
        device_registry::DeviceType::Framebuffer | device_registry::DeviceType::Nic => {
            Some(claim.describe(table, buf))
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
        Some(pipe::PipeWrite::BrokenPipe) => Some(SyscallError::NotFound.to_u64()),
        Some(pipe::PipeWrite::NoMemory) => Some(SyscallError::ResourceExhausted.to_u64()),
        Some(pipe::PipeWrite::Wrote(n)) => Some(n as u64),
        None => None,
    }
}

pub fn try_write(object: &KObjectRef, buf: &UserBytes) -> Option<u64> {
    match object {
        KObjectRef::File(f) => f.with(|state| {
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
        KObjectRef::Console(c) => {
            // The whole write is always accepted: a short count would make a caller re-send bytes.
            c.write(buf);
            Some(buf.len() as u64)
        }
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
        if new_pos < 0 {
            return SyscallError::InvalidArgument.to_u64();
        }
        state.position = (new_pos as usize).min(size);
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
            file_type: FileType::Unknown as u64,
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
            device_registry::DeviceType::Nic => FileType::Nic,
            device_registry::DeviceType::HdaAudio
            | device_registry::DeviceType::VirtioSound => FileType::Unknown,
        }),
    }
}

/// `SYS_FSYNC`: the file's bytes on the device, and the device told to commit them.
///
/// The device-commit step is not optional: `/bin/logd` publishes `LOG_DURABLE_NS` off `fsync`'s result, so a flush that stopped at the page cache would make that durability contract a claim about nothing.
pub fn fsync(object: &KObjectRef) -> u64 {
    let KObjectRef::File(file) = object else {
        return SyscallError::PermissionDenied.to_u64();
    };
    let (path, file_id, mtime) =
        file.with(|state| (state.path.clone(), state.file_id, state.mtime));
    // The file's dirty state, not the handle's: another handle's write still gets made durable here.
    if !file_cache::dirty_meta(file_id) {
        return 0;
    }
    let began = crate::clock::now();
    // Bounds the run of attempts, never a single attempt's elapsed time.
    let deadman = Deadline::at(began + crate::block::DEADMAN.duration());
    #[cfg(feature = "boot-actuators")]
    let deadman = if crate::actuator::fsync_deadman_now() { Deadline::passed() } else { deadman };
    let mut attempt = 0u32;
    // No spinlock is held at this depth, so waiting here (unlike everywhere below `vfs::lock()`) is safe.
    loop {
        attempt += 1;
        let refused = {
            // Stages a first attempt with its budget already spent, exercising the shipped refusal itself.
            #[cfg(feature = "boot-actuators")]
            let _spent = (attempt == 1 && crate::actuator::fsync_budget_spent())
                .then(|| crate::scheduler::Operation::begin(Deadline::passed()));
            // Outside `FileObject`'s lock: this and `OpenFileState::drop` take the VFS lock in the same order.
            // Flush and sync share one acquisition so this file cannot be unmounted between them.
            let mut vfs = crate::vfs::lock();
            let done = vfs
                .flush_file(&path, file_id, mtime)
                .and_then(|()| vfs.sync_for_path(&path));
            drop(vfs);
            done
        };
        match refused {
            Ok(()) => {
                if attempt > 1 {
                    crate::log!(
                        "fsync: {path} durable on attempt {attempt} after {} — a refused \
                         attempt kept every page dirty and a later one delivered them",
                        crate::clock::now() - began,
                    );
                }
                // `flush_file` already cleared the file's own `dirty_meta`; there is no per-handle flag to clear.
                return 0;
            }
            // A budget expired on a live device, never a device fact: retry on a fresh budget.
            // A refused attempt discards nothing — `Vfs::flush_file` restores `dirty_meta` before returning.
            Err(SyscallError::WouldBlock) => {
                // A killed caller stops retrying at the first safe point; the return value dies with the task.
                if crate::sched::driver::current_kill_pending() {
                    return SyscallError::WouldBlock.to_u64();
                }
                if deadman.reached(crate::clock::now()) {
                    crate::log!(
                        "fsync: {path} is not durable after {attempt} attempt(s) in {} — \
                         {}",
                        crate::clock::now() - began,
                        crate::block::DEADMAN,
                    );
                    return SyscallError::Io.to_u64();
                }
                crate::block::between_attempts(attempt);
            }
            // The device's own word (an error status, or a recovery that gave up) is passed through unchanged.
            Err(e) => return e.to_u64(),
        }
    }
}

pub fn ftruncate(object: &KObjectRef, size: u64) -> u64 {
    let KObjectRef::File(file) = object else {
        return SyscallError::PermissionDenied.to_u64();
    };
    file.with(|state| {
        // `resize`, not `set_size`: marks the file dirty even when no page changed, so flush is not skipped.
        file_cache::resize(state.file_id, size);
        if state.position > size as usize {
            state.position = size as usize;
        }
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
            device_registry::DeviceType::Nic => crate::net::has_packet(),
            device_registry::DeviceType::Framebuffer => true,
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
        KObjectRef::File(_) | KObjectRef::Console(_) => true,
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
