//! What a syscall does to the object a handle names.
//!
//! Every function dispatches on [`KObjectRef`] with no `_` arm, so a new
//! object type is a compile error here. Authorization is not here: the
//! caller has already resolved the handle with the rights the call needs.

use alloc::sync::Arc;

use kernel::sched::poll::Via;

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::{FileType, SyscallError, MAX_WHOLE_PIPE_WRITE};

use crate::pipe::{self, PipeRef};
use kernel::pipe::End;
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::inbox::PollEntry;
use crate::watch::Watch;
use crate::device as device_registry;

use super::device::DeviceClaim;
use super::handle::{HandleEntry, HandleTable};
use super::KObjectRef;

/// What a freshly created object's one handle carries.
pub fn initial_rights(object: &KObjectRef) -> Rights {
    const BASE: Rights = Rights::DUP.union(Rights::TRANSFER).union(Rights::WAIT);
    match object {
        KObjectRef::PipeRead(_) => BASE.union(Rights::READ),
        KObjectRef::PipeWrite(_) => BASE.union(Rights::WRITE),
        KObjectRef::Connection(_) => {
            BASE.union(Rights::READ).union(Rights::WRITE).union(Rights::MAP)
        }
        // No `DUP`: a claim admits exactly one handle, exclusivity by type rather than a check in `dup`.
        KObjectRef::Device(_) => {
            Rights::TRANSFER.union(Rights::WAIT).union(Rights::READ).union(Rights::WRITE)
        }
        KObjectRef::Acceptor(_) => BASE.union(Rights::READ),
        // No `TRANSFER`: a ring's page is its creator's alone, and its
        // watches name handles in the table of whoever submits.
        KObjectRef::Inbox(_) => {
            Rights::DUP.union(Rights::WAIT).union(Rights::READ).union(Rights::WRITE).union(Rights::MAP)
        }
        // Every `SysCap` bit is authority the supervisor decides per program: no default, the creator states it.
        KObjectRef::SysCap(_) => Rights::NONE,
        // A region is examined through the memory, not the handle: `MAP` maps it
        // readable and `WRITE` writable, which memory nobody writes never carries.
        KObjectRef::SharedMem(m) => {
            let read = Rights::DUP.union(Rights::TRANSFER).union(Rights::MAP);
            if m.immutable().is_some() { read } else { read.union(Rights::WRITE) }
        }
        // `WRITE` is what connecting through it takes: a holder given it without may only pass it on.
        KObjectRef::Connector(_) => Rights::DUP.union(Rights::TRANSFER).union(Rights::WRITE),
        // A spawner gets everything a child handle offers: exit code, kill, and accounting.
        KObjectRef::Process(_) => BASE.union(Rights::READ).union(Rights::MANAGE),
        KObjectRef::Null(_) => super::null::NullObject::RIGHTS,
        KObjectRef::Job(_) => super::job::JobObject::RIGHTS,
        KObjectRef::Notice(_) => super::notice::NoticeObject::RIGHTS,
        KObjectRef::WakeLatency(_) => super::wake_latency::WakeLatencyObject::RIGHTS,
        KObjectRef::EventSignal(_) => Rights::DUP.union(Rights::TRANSFER).union(Rights::WRITE),
        KObjectRef::EventWait(_) => BASE.union(Rights::READ),
        KObjectRef::Pager(_) => super::paged::PagerObject::RIGHTS,
        KObjectRef::Paged(h) => h.rights(),
        // Neither `DUP` nor `TRANSFER`: the set is its minter's alone.
        KObjectRef::DeviceSet(_) => Rights::READ.union(Rights::WAIT),
    }
}

/// Install a new object at the next free slot, with the rights its type gets.
pub fn install(table: &mut HandleTable, object: KObjectRef) -> Result<RawHandle, SyscallError> {
    let rights = initial_rights(&object);
    table
        .install(HandleEntry::new(object, rights))
        .map_err(|_| SyscallError::ResourceExhausted)
}

/// Release one handle, and the polls made through it ([`HandleTable::remove`]).
///
/// What the object holds is released by its own zero-handle hook.
pub fn close(table: &mut HandleTable, h: RawHandle) -> Result<(), super::HandleError> {
    drop(table.remove(h)?);
    Ok(())
}

/// End the polls made through `via` on `object`'s watches: that handle left
/// its table.
pub fn end_polls(object: &KObjectRef, via: Via) {
    for watch in [read_watch(object), write_watch(object), pipe_end_watch(object)].into_iter().flatten() {
        watch.end_polls(via);
    }
}

/// Release every handle a process holds; runs on the kill path too, which
/// does not unwind, and allocates nothing.
pub fn close_all(table: &mut HandleTable) {
    let job = Arc::clone(table.job());
    drop(core::mem::replace(table, HandleTable::new(&job)));
}

/// The pipe a blocking read of `object` parks on.
pub fn pipe_read(object: &KObjectRef) -> Option<&PipeRef> {
    match object {
        KObjectRef::PipeRead(r) => Some(r.pipe()),
        KObjectRef::Connection(_) | KObjectRef::PipeWrite(_) | KObjectRef::Device(_)
        | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Null(_)
        | KObjectRef::Notice(_) | KObjectRef::DeviceSet(_) => None,
    }
}

/// [`pipe_read`]'s answer for a blocking write.
pub fn pipe_write(object: &KObjectRef) -> Option<&PipeRef> {
    match object {
        KObjectRef::PipeWrite(w) => Some(w.pipe()),
        KObjectRef::Connection(_) | KObjectRef::PipeRead(_) | KObjectRef::Device(_)
        | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Null(_)
        | KObjectRef::Notice(_) | KObjectRef::DeviceSet(_) => None,
    }
}

/// A watch as a poll registration holds it: a static one's reference, or a
/// share of one that goes with its object.
pub enum WatchRef {
    Static(&'static Watch),
    /// A static watch posted where nothing may be freed, as `pmm::returned`
    /// and the device set, which a hot-plug slot's handler posts, are.
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

    fn end_polls(&self, via: Via) {
        match self {
            Self::Static(watch) => watch.end_polls(via),
            Self::InPlace(watch) => watch.end_polls(via),
            Self::Shared(watch) => watch.end_polls(via),
            Self::Claim(claim) => claim.end_polls(via),
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
        KObjectRef::DeviceSet(_) => Some(WatchRef::InPlace(&crate::pcidev::SET_WATCH)),
        KObjectRef::Notice(n) => Some(WatchRef::Shared(n.watch().clone())),
        KObjectRef::EventWait(e) => Some(WatchRef::Shared(e.watch().clone())),
        KObjectRef::Pager(p) => Some(WatchRef::Shared(p.watch().clone())),
        KObjectRef::Job(_) => Some(WatchRef::InPlace(crate::mm::pmm::returned())),
        KObjectRef::Device(d) => match d.class() {
            device_registry::DeviceType::PciFunction
            | device_registry::DeviceType::Isa
            | device_registry::DeviceType::Acpi
            | device_registry::DeviceType::Uart => Some(WatchRef::Claim(d.clone())),
            device_registry::DeviceType::Framebuffer => None,
        },
        // Named unconditionally: the watch alone cannot enforce rights.
        KObjectRef::SysCap(_) => Some(WatchRef::Static(&crate::log::user::WATCH)),
        KObjectRef::PipeWrite(_) | KObjectRef::Inbox(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::Paged(_)
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
        | KObjectRef::SysCap(_)
        | KObjectRef::Process(_) | KObjectRef::Null(_) | KObjectRef::Job(_) | KObjectRef::DeviceSet(_) => {}
    }
}

/// The watch a writable poll on this object registers on.
pub fn write_watch(object: &KObjectRef) -> Option<WatchRef> {
    match object {
        KObjectRef::PipeWrite(w) => Some(WatchRef::Shared(w.pipe().write_watch().clone())),
        KObjectRef::Connection(c) => Some(WatchRef::Shared(c.tx().write_watch().clone())),
        KObjectRef::PipeRead(_) | KObjectRef::Device(_)
        | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Null(_)
        | KObjectRef::Notice(_) | KObjectRef::DeviceSet(_) => None,
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
        | KObjectRef::Device(_) | KObjectRef::SysCap(_)
        | KObjectRef::Inbox(_) | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Null(_) | KObjectRef::DeviceSet(_) => None,
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
        | KObjectRef::Device(_) | KObjectRef::SysCap(_)
        | KObjectRef::Inbox(_) | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Null(_) | KObjectRef::DeviceSet(_) => false,
    }
}

/// What a device read answers, to copy out once no lock is held.
pub enum DeviceRead {
    /// The claim's description, which the reader takes as much of as fits.
    Description(alloc::boxed::Box<[u8]>),
    Record(toyos_abi::pci::DeviceIrqRecord),
    /// The answer, with nothing to copy.
    Word(u64),
}

impl DeviceRead {
    /// Copy what it answers into `buf`, answering the syscall's word.
    pub fn deliver(self, buf: &mut UserBytesMut) -> u64 {
        match self {
            Self::Description(bytes) => {
                let count = buf.len().min(bytes.len());
                buf.write_at(0, &bytes[..count]).map_or_else(|faulted| SyscallError::from(faulted).to_u64(), |()| count as u64)
            }
            Self::Record(record) => buf
                .write_at(0, record_bytes(&record))
                .map_or_else(|faulted| SyscallError::from(faulted).to_u64(), |()| toyos_abi::pci::DeviceIrqRecord::SIZE as u64),
            Self::Word(word) => word,
        }
    }
}

/// Read a device claim into a reader with `room` bytes; takes `table`
/// because describing a device installs handles into it.
pub fn read_device(claim: &DeviceClaim, table: &mut HandleTable, room: usize) -> Option<DeviceRead> {
    let refused = |e: SyscallError| Some(DeviceRead::Word(e.to_u64()));
    let described = |table: &mut HandleTable| Some(claim.describe(table).map_or_else(|e| DeviceRead::Word(e.to_u64()), DeviceRead::Description));
    match claim.class() {
        device_registry::DeviceType::Framebuffer => described(table),
        // The description first and interrupts after: a driver reads what it
        // is driving once, and everything it reads afterwards is what its
        // device has been doing.
        device_registry::DeviceType::PciFunction => {
            if !claim.info_read() {
                return described(table);
            }
            if room < toyos_abi::pci::DeviceIrqRecord::SIZE {
                return refused(SyscallError::InvalidArgument);
            }
            match claim.pci(crate::pcidev::take_record) {
                Some(Ok(record)) => record.map(DeviceRead::Record),
                Some(Err(e)) => refused(e),
                None => refused(SyscallError::Gone),
            }
        }
        // The PCI shape, but the description is what binds the ports to the
        // reader, and nothing after it answers any other process.
        device_registry::DeviceType::Isa | device_registry::DeviceType::Acpi | device_registry::DeviceType::Uart => {
            let pid = crate::process::current_process();
            if !claim.info_read() {
                if claim.isa(|row| crate::isa::bind(row, pid)).is_none() {
                    return refused(SyscallError::Gone);
                }
                return described(table);
            }
            let room = room >= toyos_abi::pci::DeviceIrqRecord::SIZE;
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
            match taken {
                Some(Ok(record)) => record.map(DeviceRead::Record),
                Some(Err(e)) => refused(e),
                None => refused(SyscallError::Gone),
            }
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
        // A kill that ends the wait for another read's claim answers what
        // a cancelled wait does; no answer reaches its Ring 3.
        KObjectRef::PipeRead(r) => match r.pipe().read(&crate::scheduler::Parkable::at_entry(), buf) {
            Ok(read) => read.map(|n| n as u64),
            Err(pipe::ReadStopped::Cancelled) => Some(SyscallError::Gone.to_u64()),
            Err(pipe::ReadStopped::Faulted(faulted)) => Some(SyscallError::from(faulted).to_u64()),
        },
        KObjectRef::Device(_) => unreachable!("a device claim is read by `read_device`"),
        KObjectRef::Null(_) => Some(0),
        // An empty read listens and takes nothing: the reason would have nowhere to go.
        KObjectRef::Notice(n) if buf.is_empty() => {
            n.listen();
            Some(0)
        }
        KObjectRef::Notice(n) => n.read().map(|reason| {
            buf.write_at(0, &[reason.to_byte()]).map_or_else(|faulted| SyscallError::from(faulted).to_u64(), |()| 1)
        }),
        // A connection carries whole messages, which `SYS_MESSAGE_RECV` takes.
        KObjectRef::Connection(_) | KObjectRef::PipeWrite(_) | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::DeviceSet(_) => Some(SyscallError::PermissionDenied.to_u64()),
    }
}

fn write_pipe(object: &KObjectRef, buf: &UserBytes) -> Option<u64> {
    let pipe = pipe_write(object).expect("a pipe's write end writes its pipe");
    let Ok(wrote) = pipe.write(&crate::scheduler::Parkable::at_entry(), buf) else {
        // As a read's cancelled wait for its turn answers.
        return Some(SyscallError::Gone.to_u64());
    };
    pipe_written(wrote)
}

/// What a pipe write answers its caller: `None` to wait for room.
pub fn pipe_written(wrote: pipe::PipeWrite) -> Option<u64> {
    match wrote {
        // The reader is gone, not absent: `NotFound` means the name is not there and nothing else may say so.
        pipe::PipeWrite::BrokenPipe => Some(SyscallError::Gone.to_u64()),
        pipe::PipeWrite::NoMemory => Some(SyscallError::ResourceExhausted.to_u64()),
        pipe::PipeWrite::Faulted(faulted) => Some(SyscallError::from(faulted).to_u64()),
        pipe::PipeWrite::Wrote(n) => Some(n as u64),
        pipe::PipeWrite::NoRoom => None,
    }
}

pub fn try_write(object: &KObjectRef, buf: &UserBytes) -> Option<u64> {
    match object {
        KObjectRef::PipeWrite(_) => write_pipe(object, buf),
        KObjectRef::Device(d) => Some(write_device(d, buf)),
        KObjectRef::Null(_) => Some(buf.len() as u64),
        // A connection carries whole messages, which `SYS_MESSAGE_SEND` sends.
        KObjectRef::Connection(_) | KObjectRef::PipeRead(_) | KObjectRef::Acceptor(_)
        | KObjectRef::Inbox(_) | KObjectRef::SharedMem(_) | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Notice(_) | KObjectRef::DeviceSet(_) => {
            Some(SyscallError::PermissionDenied.to_u64())
        }
    }
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

/// The kind and size of a kernel object.
pub fn fstat(object: &KObjectRef) -> Stat {
    let plain = |t: FileType| Stat { file_type: t as u64, size: 0, mtime: 0, volume: 0, node: 0 };
    match object {
        KObjectRef::PipeRead(r) => {
            plain(if r.is_tty() { FileType::Tty } else { FileType::Pipe })
        }
        KObjectRef::PipeWrite(w) => {
            plain(if w.is_tty() { FileType::Tty } else { FileType::Pipe })
        }
        KObjectRef::Connection(_) => plain(FileType::Socket),
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
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Notice(_) | KObjectRef::DeviceSet(_) => plain(FileType::Unknown),
        KObjectRef::Device(d) => plain(match d.class() {
            device_registry::DeviceType::Framebuffer => FileType::Framebuffer,
            device_registry::DeviceType::PciFunction
            | device_registry::DeviceType::Isa
            | device_registry::DeviceType::Acpi
            | device_registry::DeviceType::Uart => FileType::Unknown,
        }),
    }
}

pub fn has_data(object: &KObjectRef) -> bool {
    match object {
        KObjectRef::PipeRead(r) => r.pipe().has_data(),
        KObjectRef::Connection(c) => c.rx().readable(),
        KObjectRef::Acceptor(a) => a.has_pending(),
        KObjectRef::Process(p) => p.finished(),
        KObjectRef::DeviceSet(set) => set.changed(),
        KObjectRef::Job(j) => j.ready(),
        KObjectRef::Notice(n) => n.readable(),
        KObjectRef::EventWait(e) => e.ready(),
        KObjectRef::Pager(p) => p.has_notices(),
        KObjectRef::Null(_) => true,
        KObjectRef::Device(d) => match d.class() {
            // A claim its last handle has let go has nothing to read: a poll
            // on it is ended where it registers (`DeviceClaim::add_poll`).
            device_registry::DeviceType::PciFunction => {
                !d.info_read() || d.pci(crate::pcidev::has_irq).unwrap_or(false)
            }
            device_registry::DeviceType::Isa
            | device_registry::DeviceType::Acpi
            | device_registry::DeviceType::Uart => !d.info_read() || d.isa(crate::isa::has_irq).unwrap_or(false),
            device_registry::DeviceType::Framebuffer => true,
        },
        KObjectRef::PipeWrite(_) | KObjectRef::Inbox(_) | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::Paged(_)
        | KObjectRef::SharedMem(_) => false,
    }
}

/// Whether a post on this object's read watch is its readability, which
/// [`has_data`] cannot be asked for: the log's, whose unread records are a
/// property of the reader's cursor, which the kernel does not hold.
pub fn read_posts_are_readiness(object: &KObjectRef) -> bool {
    match object {
        KObjectRef::SysCap(_) => true,
        KObjectRef::PipeRead(_) | KObjectRef::PipeWrite(_) | KObjectRef::Connection(_)
        | KObjectRef::Acceptor(_) | KObjectRef::Device(_)
        | KObjectRef::Inbox(_) | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Null(_)
        | KObjectRef::Notice(_) | KObjectRef::DeviceSet(_) => false,
    }
}

pub fn has_space(object: &KObjectRef) -> bool {
    match object {
        // Writable is room for any write that lands whole, so a non-blocking
        // one refused, retried on readiness, lands.
        KObjectRef::PipeWrite(w) => w.pipe().has_space(MAX_WHOLE_PIPE_WRITE),
        KObjectRef::Connection(c) => c.tx().writable(),
        KObjectRef::Null(_) => true,
        KObjectRef::PipeRead(_) | KObjectRef::Device(_) | KObjectRef::Acceptor(_)
        | KObjectRef::Inbox(_) | KObjectRef::SysCap(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::SharedMem(_) | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Notice(_) | KObjectRef::DeviceSet(_) => false,
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
        KObjectRef::Connection(_) | KObjectRef::Device(_)
        | KObjectRef::Acceptor(_) | KObjectRef::Inbox(_)
        | KObjectRef::SysCap(_) | KObjectRef::SharedMem(_)
        | KObjectRef::Connector(_) | KObjectRef::WakeLatency(_) | KObjectRef::EventSignal(_) | KObjectRef::EventWait(_) | KObjectRef::Pager(_) | KObjectRef::Paged(_)
        | KObjectRef::Process(_) | KObjectRef::Job(_) | KObjectRef::Null(_) | KObjectRef::Notice(_) | KObjectRef::DeviceSet(_) => {
            SyscallError::InvalidArgument.to_u64()
        }
    }
}

/// A write to a claim: [`toyos_abi::acpi::ACK`] from the process its row's
/// ports are bound to, which unmasks the row's level lines. Nothing else
/// writes to a claim.
fn write_device(claim: &DeviceClaim, buf: &UserBytes) -> u64 {
    let ack = match claim.class() {
        device_registry::DeviceType::Isa | device_registry::DeviceType::Acpi => toyos_abi::acpi::ACK,
        device_registry::DeviceType::Uart => toyos_abi::uart::ACK,
        device_registry::DeviceType::Framebuffer | device_registry::DeviceType::PciFunction => {
            return SyscallError::PermissionDenied.to_u64();
        }
    };
    if !claim.info_read() {
        return SyscallError::PermissionDenied.to_u64();
    }
    let mut word = [0u8; 4];
    if buf.len() == word.len() {
        if let Err(faulted) = buf.read_at(0, &mut word) {
            return SyscallError::from(faulted).to_u64();
        }
    }
    let acknowledges = buf.len() == word.len() && u32::from_ne_bytes(word) == ack;
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
