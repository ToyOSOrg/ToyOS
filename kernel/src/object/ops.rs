//! What a syscall does to the object a handle names.
//!
//! Every function dispatches on [`KObjectRef`] with no `_` arm, so a new
//! object type is a compile error here. Authorization is not here: the
//! caller has already resolved the handle with the rights the call needs.

use alloc::sync::Arc;

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::{FileType, OpenFlags, SeekFrom, SyscallError, MAX_WHOLE_PIPE_WRITE};

use crate::drivers::serial;
use crate::file_cache;
use crate::pipe::{self, PipeRef};
use kernel::pipe::End;
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::inbox::PollEntry;
use crate::watch::Watch;
use crate::device as device_registry;

use super::device::DeviceClaim;
use super::file::{FileObject, OpenFileState};
use super::handle::{HandleEntry, HandleTable};
use super::{KObjectRef, KObjectVariant};

/// What a freshly created object's one handle carries.
pub fn initial_rights(object: &KObjectRef) -> Rights {
    const BASE: Rights = Rights::DUP.union(Rights::TRANSFER).union(Rights::WAIT);
    match object {
        KObjectRef::PipeRead(_) => BASE.union(Rights::READ),
        KObjectRef::PipeWrite(_) => BASE.union(Rights::WRITE),
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
        // Every `SysCap` bit is authority the supervisor decides per program: no default, the creator states it.
        KObjectRef::SysCap(_) => Rights::NONE,
        // A region is examined through the memory, not the handle: `MAP` maps it
        // readable and `WRITE` writable, which memory nobody writes never carries.
        KObjectRef::SharedMem(m) => {
            let read = Rights::DUP.union(Rights::TRANSFER).union(Rights::MAP);
            if m.immutable().is_some() { read } else { read.union(Rights::WRITE) }
        }
        // A connector has no read/write path: put it in a namespace, or give the namespace away.
        KObjectRef::Connector(_) => Rights::DUP.union(Rights::TRANSFER),
        // `READ` is what resolving a name through it, and narrowing into a child's, both take.
        KObjectRef::Namespace(_) => Rights::DUP.union(Rights::TRANSFER).union(Rights::READ),
        // A spawner gets everything a child handle offers: exit code, kill, and accounting.
        KObjectRef::Process(_) => BASE.union(Rights::READ).union(Rights::MANAGE),
        KObjectRef::Null(_) => super::null::NullObject::RIGHTS,
        KObjectRef::Job(_) => super::job::JobObject::RIGHTS,
        KObjectRef::Notice(_) => super::notice::NoticeObject::RIGHTS,
        KObjectRef::WakeLatency(_) => super::wake_latency::WakeLatencyObject::RIGHTS,
        KObjectRef::EventSignal(_) => Rights::DUP.union(Rights::TRANSFER).union(Rights::WRITE),
        KObjectRef::EventWait(_) => BASE.union(Rights::READ),
        KObjectRef::Pager(_) => super::paged::PagerObject::RIGHTS,
        KObjectRef::Paged(_) => super::paged::PagedHolderObject::RIGHTS,
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
    // First, so the answer is the bit and not the path's own refusal.
    if flags.0 & !OpenFlags::KNOWN.0 != 0 {
        return SyscallError::InvalidArgument.to_u64();
    }
    let writable = flags.contains(OpenFlags::WRITE);
    let create = flags.contains(OpenFlags::CREATE);
    let truncate = flags.contains(OpenFlags::TRUNCATE);
    let append = flags.contains(OpenFlags::APPEND);
    let exclusive = flags.contains(OpenFlags::EXCLUSIVE);
    if exclusive && !create {
        return SyscallError::InvalidArgument.to_u64();
    }
    let modifies = writable || create || truncate || append;
    // Charged before the file is opened or made, so a refusal makes nothing.
    let core = match FileObject::new_core(0) {
        Ok(core) => core,
        Err(e) => return SyscallError::from(e).to_u64(),
    };

    let opened = {
        let mut vfs = crate::vfs::lock();
        // Scoped to this block: dropped before the object exists, since `OpenFileState::Drop` re-takes it.

        let intent = if modifies {
            crate::vfs::ResolveIntent::UserModify
        } else {
            crate::vfs::ResolveIntent::KernelOrRead
        };
        // An exclusive create decides on the name itself, its link not
        // followed, so a link (dangling or not) or a directory there is
        // `AlreadyExists`, as POSIX's `O_EXCL` has it.
        if exclusive {
            match vfs.stat(path, false) {
                Ok(_) => return SyscallError::AlreadyExists.to_u64(),
                Err(SyscallError::NotFound) => {}
                Err(e) => return e.to_u64(),
            }
        }
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

        // Under the same VFS lock as the check above, so two exclusive
        // creates of one name cannot both find it missing; nothing is there,
        // so the target is the name itself.
        let built = if exclusive {
            let mtime = crate::clock::mtime_now();
            vfs.create_file(target.as_str(), mtime).map(|(file_id, volume)| (file_id, volume, mtime, 0))
        } else if truncate && create {
            let mtime = crate::clock::mtime_now();
            // `NotFound` is not a failure: truncating past a name that was not there is fine.
            // Any `vfs.delete_file` error other than `NotFound` is propagated, not swallowed: truncating past it could silently create a file over one the mount could not confirm was missing.
            match vfs.delete_file(target.as_str()) {
                Ok(()) | Err(SyscallError::NotFound) => {}
                Err(e) => return e.to_u64(),
            }
            vfs.create_file(target.as_str(), mtime).map(|(file_id, volume)| (file_id, volume, mtime, 0))
        } else {
            // `mtime_target` takes no reference, so it runs first and the reference-taking
            // `open_target` runs last — a refusal cannot strand a reference. `CREATE` acts on `NotFound` only.
            match vfs.mtime_target(&target) {
                Ok(mtime) => vfs.open_target(&target).map(|(file_id, volume)| {
                    let position =
                        if append { file_cache::size(file_id) as usize } else { 0 };
                    (file_id, volume, mtime, position)
                }),
                Err(SyscallError::NotFound) if create => {
                    let mtime = crate::clock::mtime_now();
                    vfs.create_file(target.as_str(), mtime).map(|(file_id, volume)| (file_id, volume, mtime, 0))
                }
                Err(e) => Err(e),
            }
        };
        built
    };

    let (file_id, volume, mtime, position) = match opened {
        Ok(v) => v,
        Err(e) => return e.to_u64(),
    };
    let object = KObjectRef::File(FileObject::new(core, OpenFileState { file_id, volume, position, mtime }));
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
/// What the object holds is released by its own zero-handle hook; `close` releases only what is the process's, not the object's: polls on a watch it ends.
pub fn close(table: &mut HandleTable, h: RawHandle) -> Result<(), super::HandleError> {
    let entry = table.remove(h)?;
    let object = entry.object().clone();
    // The decrement, and any deferred hook it enqueues, run with the table's borrow already released.
    drop(entry);
    // Only a watch this handle's object ends is answered: one it shares cannot cancel another's poll.
    if close_ends_polls(&object) {
        for watch in [read_watch(&object), write_watch(&object), pipe_end_watch(&object)].into_iter().flatten() {
            watch.cancel_polls();
        }
    }
    Ok(())
}


/// Release every handle a process holds; runs on the kill path too, which
/// does not unwind, and allocates nothing.
pub fn close_all(table: &mut HandleTable) {
    let job = Arc::clone(table.job());
    drop(core::mem::replace(table, HandleTable::new(&job)));
}

/// The pipe a read of `object` took from, held past the handle table's lock
/// to wake its writers.
pub fn read_pipe(object: &KObjectRef) -> Option<PipeRef> {
    pipe_read(object).cloned()
}

/// The pipe a write to `object` filled, held the same way to wake its readers.
pub fn written_pipe(object: &KObjectRef) -> Option<PipeRef> {
    pipe_write(object).cloned()
}

/// The pipe a blocking read of `object` parks on.
pub fn pipe_read(object: &KObjectRef) -> Option<&PipeRef> {
    match object {
        KObjectRef::PipeRead(r) => Some(r.pipe()),
        KObjectRef::Connection(_) | KObjectRef::PipeWrite(_) | KObjectRef::File(_) | KObjectRef::Device(_)
        | KObjectRef::Console(_) | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Null(_)
        | KObjectRef::Notice(_) => None,
    }
}

/// [`pipe_read`]'s answer for a blocking write.
pub fn pipe_write(object: &KObjectRef) -> Option<&PipeRef> {
    match object {
        KObjectRef::PipeWrite(w) => Some(w.pipe()),
        KObjectRef::Connection(_) | KObjectRef::PipeRead(_) | KObjectRef::File(_) | KObjectRef::Device(_)
        | KObjectRef::Console(_) | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Null(_)
        | KObjectRef::Notice(_) => None,
    }
}

/// A watch as a poll registration holds it: a static one's reference, or a
/// share of one that goes with its object.
pub enum WatchRef {
    Static(&'static Watch),
    /// A static watch posted where nothing may be freed, as `pmm::returned` is.
    InPlace(&'static crate::watch::IrqWatch),
    Shared(Arc<Watch>),
    /// The watch of the function or row a claim holds: named by the claim,
    /// and reached only with what the claim lends.
    Claim(Arc<DeviceClaim>),
}

impl WatchRef {
    pub(crate) fn add_poll(&self, entry: PollEntry) {
        match self {
            Self::Static(watch) => watch.add_poll(entry),
            Self::InPlace(watch) => watch.add_poll(entry),
            Self::Shared(watch) => watch.add_poll(entry),
            Self::Claim(claim) => claim.add_poll(entry),
        }
    }

    pub fn cancel_polls(&self) {
        match self {
            Self::Static(watch) => watch.cancel_polls(),
            Self::InPlace(watch) => watch.cancel_polls(),
            Self::Shared(watch) => watch.cancel_polls(),
            // Its release answers them, and its close ends none (`close_ends_polls`).
            Self::Claim(_) => unreachable!("a claim's polls are cancelled by its release alone"),
        }
    }
}

/// The watch a readable poll on this object registers on, or `None` when no
/// readiness of that direction exists.
pub fn read_watch(object: &KObjectRef) -> Option<WatchRef> {
    match object {
        KObjectRef::PipeRead(r) => Some(WatchRef::Shared(r.pipe().read_watch().clone())),
        KObjectRef::Connection(c) => Some(WatchRef::Shared(c.rx().read_watch().clone())),
        KObjectRef::Acceptor(a) => Some(WatchRef::Shared(a.watch().clone())),
        KObjectRef::Process(p) => Some(WatchRef::Shared(p.watch().clone())),
        KObjectRef::Notice(n) => Some(WatchRef::Shared(n.watch().clone())),
        KObjectRef::EventWait(e) => Some(WatchRef::Shared(e.watch().clone())),
        KObjectRef::Pager(p) => Some(WatchRef::Shared(p.watch().clone())),
        KObjectRef::Job(_) => Some(WatchRef::InPlace(crate::mm::pmm::returned())),
        // Nothing posts for a serial byte, so a console's readable poll has
        // no source (`issues/a-console-watch-waits-on-the-keyboard-not-the-serial-line.md`).
        KObjectRef::Console(_) => None,
        KObjectRef::Device(d) => match d.class() {
            device_registry::DeviceType::PciFunction
            | device_registry::DeviceType::Isa
            | device_registry::DeviceType::Acpi => Some(WatchRef::Claim(d.clone())),
            device_registry::DeviceType::Framebuffer => None,
        },
        // Named unconditionally: the watch alone cannot enforce rights.
        KObjectRef::SysCap(_) => Some(WatchRef::Static(&crate::log::user::WATCH)),
        KObjectRef::PipeWrite(_) | KObjectRef::File(_) | KObjectRef::Inbox(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Null(_) => None,
    }
}

/// A readable poll is registered on this object: its process listens to a
/// notice from now on (`toyos_proclife::quit`).
pub fn watched_readable(object: &KObjectRef) {
    match object {
        KObjectRef::Notice(n) => n.listen(),
        KObjectRef::PipeRead(_) | KObjectRef::PipeWrite(_) | KObjectRef::Connection(_) | KObjectRef::Device(_)
        | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_) | KObjectRef::SharedMem(_) | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::Namespace(_) | KObjectRef::File(_) | KObjectRef::Console(_) | KObjectRef::SysCap(_)
        | KObjectRef::Process(_) | KObjectRef::Null(_) | KObjectRef::Job(_) => {}
    }
}

/// The watch a writable poll on this object registers on.
pub fn write_watch(object: &KObjectRef) -> Option<WatchRef> {
    match object {
        KObjectRef::PipeWrite(w) => Some(WatchRef::Shared(w.pipe().write_watch().clone())),
        KObjectRef::Connection(c) => Some(WatchRef::Shared(c.tx().write_watch().clone())),
        KObjectRef::Console(_) => Some(WatchRef::Static(&crate::log::console::SPACE)),
        KObjectRef::PipeRead(_) | KObjectRef::File(_) | KObjectRef::Device(_)
        | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Null(_)
        | KObjectRef::Notice(_) => None,
    }
}

/// A pipe end's leaving watch, which only its other end's last holder posts as
/// it lets go; `None` for an object that is no pipe end.
pub fn pipe_end_watch(object: &KObjectRef) -> Option<WatchRef> {
    match object {
        KObjectRef::PipeRead(r) => Some(WatchRef::Shared(r.pipe().left_watch(End::Read).clone())),
        KObjectRef::PipeWrite(w) => Some(WatchRef::Shared(w.pipe().left_watch(End::Write).clone())),
        KObjectRef::Connection(_) | KObjectRef::Acceptor(_) | KObjectRef::Process(_) | KObjectRef::Job(_)
        | KObjectRef::Notice(_)
        | KObjectRef::Console(_) | KObjectRef::Device(_) | KObjectRef::SysCap(_)
        | KObjectRef::File(_) | KObjectRef::Inbox(_) | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::Namespace(_) | KObjectRef::SharedMem(_) | KObjectRef::Null(_) => None,
    }
}

/// Whether this pipe end's other end has no holder left; `false` for an
/// object that is no pipe end.
pub fn other_end_gone(object: &KObjectRef) -> bool {
    match object {
        KObjectRef::PipeRead(r) => r.pipe().other_end_gone(End::Read),
        KObjectRef::PipeWrite(w) => w.pipe().other_end_gone(End::Write),
        KObjectRef::Connection(_) | KObjectRef::Acceptor(_) | KObjectRef::Process(_) | KObjectRef::Job(_)
        | KObjectRef::Notice(_)
        | KObjectRef::Console(_) | KObjectRef::Device(_) | KObjectRef::SysCap(_)
        | KObjectRef::File(_) | KObjectRef::Inbox(_) | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::Namespace(_) | KObjectRef::SharedMem(_) | KObjectRef::Null(_) => false,
    }
}

/// Whether closing one handle to this object ends what its watches watch, so
/// every poll on them — in any ring — is answered as gone. `false` for the log
/// and a console, which the machine ends on its own and which other handles
/// share. `false` for a process, which only its own end ends: closing one handle to it ends no
/// other's watch. `false` for a claim on a function or a row: its one handle's
/// close is its release, which answers every poll on the watch before the slot
/// or the row can be held again (`pcidev`'s and `isa::Row`'s release).
fn close_ends_polls(object: &KObjectRef) -> bool {
    match object {
        KObjectRef::SysCap(_) => false,
        KObjectRef::Console(_) => false,
        KObjectRef::Process(_) | KObjectRef::Job(_) => false,
        KObjectRef::Device(d) => match d.class() {
            device_registry::DeviceType::PciFunction
            | device_registry::DeviceType::Isa
            | device_registry::DeviceType::Acpi => false,
            device_registry::DeviceType::Framebuffer => true,
        },
        KObjectRef::PipeRead(_) | KObjectRef::PipeWrite(_) | KObjectRef::Connection(_)
        | KObjectRef::Acceptor(_) | KObjectRef::File(_) | KObjectRef::Inbox(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Null(_) | KObjectRef::Notice(_) => true,
    }
}

/// What a file's page that could not be read or kept answers its call.
fn block_refusal(e: crate::file_backing::BlockError) -> SyscallError {
    match e {
        crate::file_backing::BlockError::Device => SyscallError::Io,
        crate::file_backing::BlockError::Exhausted => SyscallError::ResourceExhausted,
    }
}

fn read_file(file: &FileObject, buf: &mut UserBytesMut) -> Option<u64> {
    file.with(|state| {
        let (answer, read) = read_file_at(state.file_id, state.position, buf);
        state.position += read;
        Some(answer)
    })
}

/// Read file `file_id` at `position` into `buf`: the syscall's answer, and how
/// many bytes it read.
fn read_file_at(file_id: file_cache::FileId, position: usize, buf: &mut UserBytesMut) -> (u64, usize) {
    let size = file_cache::size(file_id) as usize;
    let count = buf.len().min(size.saturating_sub(position));
    let mut read = 0;
    let mut refused = None;
    while read < count {
        let abs_pos = position + read;
        let page_idx = (abs_pos / 4096) as u32;
        let offset_in_page = abs_pos % 4096;
        let to_read = (4096 - offset_in_page).min(count - read);
        // A refused page is not a page of zeros: stop short rather than fake a hole under a success.
        if let Err(e) = file_cache::read_page(file_id, page_idx, offset_in_page, &mut buf.sub(read, to_read)) {
            refused = Some(e);
            break;
        }
        read += to_read;
    }
    if let (0, Some(e)) = (read, refused) {
        return (block_refusal(e).to_u64(), 0);
    }
    (read as u64, read)
}

/// Write `buf` to file `file_id` at `position`, stamping it: the syscall's
/// answer, and how many bytes it wrote.
fn write_file_at(state: &mut OpenFileState, position: usize, buf: &UserBytes) -> (u64, usize) {
    // Refused, never wrapped: past this the `u32` page index would alias a low page.
    if (position as u64).saturating_add(buf.len() as u64) > file_cache::MAX_FILE_SIZE {
        return (SyscallError::InvalidArgument.to_u64(), 0);
    }
    let mut written = 0;
    let mut refused = None;
    while written < buf.len() {
        let abs_pos = position + written;
        let page_idx = (abs_pos / 4096) as u32;
        let offset_in_page = abs_pos % 4096;
        let to_write = (4096 - offset_in_page).min(buf.len() - written);
        // A page that cannot be re-read off the device is refused, not merged into zeros.
        if let Err(e) = file_cache::write_page(state.file_id, page_idx, offset_in_page, &buf.sub(written, to_write)) {
            refused = Some(e);
            break;
        }
        written += to_write;
    }
    if let (0, Some(e)) = (written, refused) {
        return (block_refusal(e).to_u64(), 0);
    }
    state.mtime = crate::clock::mtime_now();
    file_cache::touch(state.file_id, state.mtime);
    (written as u64, written)
}

/// Read a file at `offset`, leaving its position: any other object is
/// refused `NotSupported`, as a pipe has no offset to read at.
pub fn pread(object: &KObjectRef, offset: u64, buf: &mut UserBytesMut) -> u64 {
    let KObjectRef::File(file) = object else { return SyscallError::NotSupported.to_u64() };
    let Ok(position) = usize::try_from(offset) else { return 0 };
    file.with(|state| read_file_at(state.file_id, position, buf).0)
}

/// Write a file at `offset`, leaving its position: any other object is
/// refused `NotSupported`.
pub fn pwrite(object: &KObjectRef, offset: u64, buf: &UserBytes) -> u64 {
    let KObjectRef::File(file) = object else { return SyscallError::NotSupported.to_u64() };
    let Ok(position) = usize::try_from(offset) else { return SyscallError::InvalidArgument.to_u64() };
    file.with(|state| write_file_at(state, position, buf).0)
}

/// Read a device claim; takes `table` because describing a device installs handles into it.
pub fn read_device(
    claim: &DeviceClaim,
    table: &mut HandleTable,
    buf: &mut UserBytesMut,
) -> Option<u64> {
    match claim.class() {
        device_registry::DeviceType::Framebuffer => Some(claim.describe(table, buf)),
        // The description first and interrupts after: a driver reads what it
        // is driving once, and everything it reads afterwards is what its
        // device has been doing.
        device_registry::DeviceType::PciFunction => {
            if !claim.info_read() {
                return Some(claim.describe(table, buf));
            }
            if buf.len() < toyos_abi::pci::DeviceIrqRecord::SIZE {
                return Some(SyscallError::InvalidArgument.to_u64());
            }
            let record = match claim.pci(crate::pcidev::take_record) {
                Some(Ok(record)) => record?,
                Some(Err(refused)) => return Some(refused.to_u64()),
                None => return Some(SyscallError::Gone.to_u64()),
            };
            buf.write_at(0, record_bytes(&record));
            Some(toyos_abi::pci::DeviceIrqRecord::SIZE as u64)
        }
        // The PCI shape, but the description is what binds the ports to the
        // reader, and nothing after it answers any other process.
        device_registry::DeviceType::Isa | device_registry::DeviceType::Acpi => {
            let pid = crate::process::current_process();
            if !claim.info_read() {
                if claim.isa(|row| crate::isa::bind(row, pid)).is_none() {
                    return Some(SyscallError::Gone.to_u64());
                }
                return Some(claim.describe(table, buf));
            }
            let room = buf.len() >= toyos_abi::pci::DeviceIrqRecord::SIZE;
            // The holder's check and the take are one borrow of the row.
            let taken = claim.isa(|row| {
                if !crate::isa::held_by(row, pid) {
                    Err(SyscallError::PermissionDenied)
                } else if !room {
                    Err(SyscallError::InvalidArgument)
                } else {
                    Ok(crate::isa::take_record(row))
                }
            });
            let record = match taken {
                Some(Ok(record)) => record?,
                Some(Err(refused)) => return Some(refused.to_u64()),
                None => return Some(SyscallError::Gone.to_u64()),
            };
            buf.write_at(0, record_bytes(&record));
            Some(toyos_abi::pci::DeviceIrqRecord::SIZE as u64)
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
        KObjectRef::PipeRead(r) => r.pipe().try_read(buf).map(|n| n as u64),
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
        KObjectRef::Null(_) => Some(0),
        // An empty read listens and takes nothing: the reason would have nowhere to go.
        KObjectRef::Notice(n) if buf.is_empty() => {
            n.listen();
            Some(0)
        }
        KObjectRef::Notice(n) => n.read().map(|reason| {
            buf.write_at(0, &[reason.to_byte()]);
            1
        }),
        // A connection carries whole messages, which `SYS_MESSAGE_RECV` takes.
        KObjectRef::Connection(_) | KObjectRef::PipeWrite(_) | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) => Some(SyscallError::PermissionDenied.to_u64()),
    }
}

fn write_pipe(object: &KObjectRef, buf: &UserBytes) -> Option<u64> {
    let pipe = pipe_write(object).expect("a pipe's write end writes its pipe");
    match pipe.write(buf) {
        // The reader is gone, not absent: `NotFound` means the name is not there and nothing else may say so.
        pipe::PipeWrite::BrokenPipe => Some(SyscallError::Gone.to_u64()),
        pipe::PipeWrite::NoMemory => Some(SyscallError::ResourceExhausted.to_u64()),
        pipe::PipeWrite::Wrote(n) => Some(n as u64),
        pipe::PipeWrite::NoRoom => None,
    }
}

pub fn try_write(object: &KObjectRef, buf: &UserBytes) -> Option<u64> {
    match object {
        KObjectRef::File(f) => f.with(|state| {
            let (answer, written) = write_file_at(state, state.position, buf);
            state.position += written;
            Some(answer)
        }),
        KObjectRef::PipeWrite(_) => write_pipe(object, buf),
        // The whole lines `klogd`'s queue has room for, and a trailing partial one:
        // a short count is a writer ahead of the console, told so.
        KObjectRef::Console(c) => Some(c.write(buf) as u64),
        KObjectRef::Device(d) => Some(write_device(d, buf)),
        KObjectRef::Null(_) => Some(buf.len() as u64),
        // A connection carries whole messages, which `SYS_MESSAGE_SEND` sends.
        KObjectRef::Connection(_) | KObjectRef::PipeRead(_) | KObjectRef::Acceptor(_)
        | KObjectRef::Inbox(_) | KObjectRef::SharedMem(_) | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Notice(_) => {
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

toyos_abi::user_safe! {
    #[derive(Clone, Copy)]
    pub struct Stat {
        pub file_type: u64,
        pub size: u64,
        pub mtime: u64,
        pub volume: u64,
        pub node: u64,
    }
}

/// What kind of thing this is, how big, and which file: a file's node is its
/// `FileId`, one per file for as long as it exists (`vfs::FileSystem::open_file`).
pub fn fstat(object: &KObjectRef) -> Stat {
    let plain = |t: FileType| Stat { file_type: t as u64, size: 0, mtime: 0, volume: 0, node: 0 };
    match object {
        KObjectRef::File(f) => f.with(|state| Stat {
            file_type: FileType::File as u64,
            size: file_cache::size(state.file_id),
            mtime: state.mtime,
            volume: state.volume,
            node: state.file_id,
        }),
        KObjectRef::PipeRead(r) => {
            plain(if r.is_tty() { FileType::Tty } else { FileType::Pipe })
        }
        KObjectRef::PipeWrite(w) => {
            plain(if w.is_tty() { FileType::Tty } else { FileType::Pipe })
        }
        KObjectRef::Connection(_) => plain(FileType::Socket),
        KObjectRef::Console(_) => plain(FileType::Serial),
        KObjectRef::Null(_) => plain(FileType::Null),
        KObjectRef::Acceptor(_) => plain(FileType::Pipe),
        KObjectRef::SharedMem(m) => Stat {
            // The verdict `pcidev::dma_map` lends a device by: a receiver judges a peer's region on it before mapping.
            file_type: if m.is_ram() {
                FileType::SharedMemory
            } else if m.immutable().is_some() {
                FileType::ImmutableMemory
            } else {
                FileType::DeviceMemory
            } as u64,
            size: m.immutable().unwrap_or(m.size()),
            mtime: 0,
            volume: 0,
            node: 0,
        },
        KObjectRef::Inbox(_) | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Notice(_) => plain(FileType::Unknown),
        KObjectRef::Device(d) => plain(match d.class() {
            device_registry::DeviceType::Framebuffer => FileType::Framebuffer,
            device_registry::DeviceType::PciFunction
            | device_registry::DeviceType::Isa
            | device_registry::DeviceType::Acpi => FileType::Unknown,
        }),
    }
}

/// `SYS_FSYNC`: a kernel file's pages are the file (`/tmp`) or never written
/// (ROOT), so it owes nothing; nothing else here holds writes.
pub fn fsync(object: &KObjectRef) -> u64 {
    match object {
        KObjectRef::File(_) => 0,
        _ => SyscallError::PermissionDenied.to_u64(),
    }
}

pub fn ftruncate(object: &KObjectRef, size: u64) -> u64 {
    let KObjectRef::File(file) = object else {
        return SyscallError::PermissionDenied.to_u64();
    };
    if size > file_cache::MAX_FILE_SIZE {
        return SyscallError::InvalidArgument.to_u64();
    }
    // The seek pointer is not touched (POSIX ftruncate): a shrink leaves it past EOF.
    file.with(|state| {
        if let Err(e) = file_cache::set_size(state.file_id, size) {
            return SyscallError::from(e).to_u64();
        }
        state.mtime = crate::clock::mtime_now();
        file_cache::touch(state.file_id, state.mtime);
        0
    })
}

pub fn has_data(object: &KObjectRef) -> bool {
    match object {
        KObjectRef::PipeRead(r) => r.pipe().has_data(),
        KObjectRef::Connection(c) => c.rx().readable(),
        KObjectRef::Console(_) => serial::has_data(),
        KObjectRef::Acceptor(a) => a.has_pending(),
        KObjectRef::Process(p) => p.finished(),
        KObjectRef::Job(j) => j.ready(),
        KObjectRef::Notice(n) => n.readable(),
        KObjectRef::EventWait(e) => e.ready(),
        KObjectRef::Pager(p) => p.has_notices(),
        KObjectRef::File(_) | KObjectRef::Null(_) => true,
        KObjectRef::Device(d) => match d.class() {
            // A claim its last handle has let go has nothing to read: a poll
            // on it is ended where it registers (`DeviceClaim::add_poll`).
            device_registry::DeviceType::PciFunction => {
                !d.info_read() || d.pci(crate::pcidev::has_irq).unwrap_or(false)
            }
            device_registry::DeviceType::Isa | device_registry::DeviceType::Acpi => {
                !d.info_read() || d.isa(crate::isa::has_irq).unwrap_or(false)
            }
            device_registry::DeviceType::Framebuffer => true,
        },
        KObjectRef::PipeWrite(_) | KObjectRef::Inbox(_) | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) => false,
    }
}

/// Whether a post on this object's read watch is its readability, which
/// [`has_data`] cannot be asked for: the log's, whose unread records are a
/// property of the reader's cursor, which the kernel does not hold.
pub fn read_posts_are_readiness(object: &KObjectRef) -> bool {
    match object {
        KObjectRef::SysCap(_) => true,
        KObjectRef::Console(_) | KObjectRef::PipeRead(_) | KObjectRef::PipeWrite(_) | KObjectRef::Connection(_)
        | KObjectRef::Acceptor(_) | KObjectRef::File(_) | KObjectRef::Device(_)
        | KObjectRef::Inbox(_) | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Null(_)
        | KObjectRef::Notice(_) => false,
    }
}

pub fn has_space(object: &KObjectRef) -> bool {
    match object {
        // Writable is room for any write that lands whole, so a non-blocking
        // one refused, retried on readiness, lands.
        KObjectRef::PipeWrite(w) => w.pipe().has_space(MAX_WHOLE_PIPE_WRITE),
        KObjectRef::Connection(c) => c.tx().writable(),
        KObjectRef::File(_) | KObjectRef::Null(_) => true,
        KObjectRef::Console(_) => crate::log::console::has_room(),
        KObjectRef::PipeRead(_) | KObjectRef::Device(_) | KObjectRef::Acceptor(_)
        | KObjectRef::Inbox(_) | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Notice(_) => false,
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
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_) | KObjectRef::Namespace(_)
        | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Null(_) | KObjectRef::Notice(_) => {
            SyscallError::InvalidArgument.to_u64()
        }
    }
}

/// A write to a claim: [`toyos_abi::acpi::ACK`] from the process its row's
/// ports are bound to, which unmasks the row's level lines. Nothing else
/// writes to a claim.
fn write_device(claim: &DeviceClaim, buf: &UserBytes) -> u64 {
    match claim.class() {
        device_registry::DeviceType::Isa | device_registry::DeviceType::Acpi => {}
        device_registry::DeviceType::Framebuffer | device_registry::DeviceType::PciFunction => {
            return SyscallError::PermissionDenied.to_u64();
        }
    }
    if !claim.info_read() {
        return SyscallError::PermissionDenied.to_u64();
    }
    let mut word = [0u8; 4];
    let acknowledges = buf.len() == word.len() && {
        buf.read_at(0, &mut word);
        u32::from_ne_bytes(word) == toyos_abi::acpi::ACK
    };
    let pid = crate::process::current_process();
    // The holder's check and the unmask are one borrow of the row.
    let acked = claim.isa(|row| {
        if !crate::isa::held_by(row, pid) {
            Err(SyscallError::PermissionDenied)
        } else if !acknowledges {
            Err(SyscallError::InvalidArgument)
        } else {
            crate::isa::ack(row).map_err(|()| SyscallError::InvalidArgument)
        }
    });
    match acked {
        Some(Ok(())) => word.len() as u64,
        Some(Err(refused)) => refused.to_u64(),
        None => SyscallError::Gone.to_u64(),
    }
}
