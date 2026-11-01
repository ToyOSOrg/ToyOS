//! Blocking `read`/`write` register on the object's watch before re-checking the
//! condition — closing the check-then-block window. Each works on its object
//! cloned out of the process's lock, which no copy and no park runs under.
//!
//! Buffers arrive already bounded: `super::dispatch` converts the caller's
//! pointer and length into a [`UserBytes`]/[`UserBytesMut`] window before
//! this module runs.

use crate::watch;
use crate::object::null::NullObject;
use crate::object::{ops, KObjectRef};
use crate::time::{Cadence, Deadline, Duration};
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::{pipe, process};

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;
use kernel::sched::task::WaitClass;

use super::cancelled;
use super::handles::object;

/// What `sys_write` does when the object took nothing.
enum WriteBlock {
    /// The pipe, and the room the write waits for.
    Pipe(pipe::PipeRef, usize),
    Refused(u64),
}

/// What `sys_read` parks on when the handle has nothing to give.
enum ReadBlock {
    Pipe(pipe::PipeRef),
    /// A notice with no reason in it, until a quit puts one there.
    Notice(alloc::sync::Arc<crate::object::notice::NoticeObject>),
    /// A console, which reads the serial line and nothing else: nothing posts
    /// a serial byte, so it parks on its own thread's watch until its re-poll.
    Console(Deadline),
    /// Nothing to wait for: the answer is this word.
    Refused(u64),
}

pub(super) fn sys_write(h: RawHandle, buf: &UserBytes) -> u64 {
    let object = match object(h, Rights::WRITE) {
        Ok(object) => object,
        Err(e) => return e.refuse(),
    };
    loop {
        let blocked = match ops::try_write(&object, buf) {
            Some(n) => {
                if let Some(pipe) = ops::written_pipe(&object) { process::wake_pipe_readers(&pipe); }
                return n;
            }
            None => match ops::pipe_write(&object) {
                Some(pipe) => WriteBlock::Pipe(pipe.clone(), kernel::pipe::room(buf.len())),
                None => WriteBlock::Refused(SyscallError::NotFound.to_u64()),
            },
        };
        match blocked {
            WriteBlock::Pipe(pipe, room) => {
                let parkable = crate::scheduler::Parkable::at_entry();
                if watch::wait_until(&parkable, pipe.write_watch(), 0, WaitClass::Pipe, Deadline::never(), || pipe.has_space(room))
                    .is_err()
                {
                    return cancelled();
                }
            }
            WriteBlock::Refused(word) => return word,
        }
    }
}

fn read_block(object: &KObjectRef) -> ReadBlock {
    match object {
        KObjectRef::Device(_) => unreachable!("a device claim never blocks"),
        KObjectRef::Notice(n) => ReadBlock::Notice(n.clone()),
        KObjectRef::Console(_) => {
            // Nothing posts a serial-console key, so the timer alone wakes it.
            const CONSOLE_REPOLL: Cadence = Cadence::every(
                Duration::from_millis(10),
                "nothing posts a serial-console key, so this rate is the whole of the wake",
            );
            ReadBlock::Console(Deadline::at(crate::clock::now() + CONSOLE_REPOLL.duration()))
        }
        _ => match ops::pipe_read(object).map(|pipe| ReadBlock::Pipe(pipe.clone())) {
            Some(block) => block,
            None => ReadBlock::Refused(SyscallError::NotFound.to_u64()),
        },
    }
}

/// What a read's handle names, out of the process's lock: a device claim is
/// read under it, since describing one installs handles, and its answer copied
/// out after.
enum Readable {
    Object(KObjectRef),
    Device(Option<ops::DeviceRead>),
}

fn readable(h: RawHandle, room: usize) -> Result<Readable, crate::object::HandleError> {
    process::with_process_data(|data| {
        let object = data.handles.get_ref(h, Rights::READ)?;
        let KObjectRef::Device(claim) = object else { return Ok(Readable::Object(object.clone())) };
        let claim = alloc::sync::Arc::clone(claim);
        Ok(Readable::Device(ops::read_device(&claim, &mut data.handles, room)))
    })
}

pub(super) fn sys_read(h: RawHandle, buf: &mut UserBytesMut) -> u64 {
    let object = match readable(h, buf.len()) {
        Ok(Readable::Object(object)) => object,
        // No device class blocks: an empty read answers `NotFound`.
        Ok(Readable::Device(read)) => return read.map_or(SyscallError::NotFound.to_u64(), |read| read.deliver(buf)),
        Err(e) => return e.refuse(),
    };
    loop {
        if let Some(n) = ops::try_read(&object, buf) {
            if let Some(pipe) = ops::read_pipe(&object) { process::wake_pipe_writers(&pipe); }
            return n;
        }
        match read_block(&object) {
            ReadBlock::Pipe(pipe) => {
                let parkable = crate::scheduler::Parkable::at_entry();
                if watch::wait_until(&parkable, pipe.read_watch(), 0, WaitClass::Pipe, Deadline::never(), || pipe.has_data())
                    .is_err()
                {
                    return cancelled();
                }
            }
            ReadBlock::Notice(notice) => {
                let parkable = crate::scheduler::Parkable::at_entry();
                if watch::wait_until(
                    &parkable,
                    notice.watch(),
                    0,
                    WaitClass::Other,
                    Deadline::never(),
                    || notice.readable(),
                )
                .is_err()
                {
                    return cancelled();
                }
            }
            ReadBlock::Console(deadline) => {
                let parkable = crate::scheduler::Parkable::at_entry();
                let Some(handle) = crate::sched::driver::current_handle() else { return cancelled() };
                if watch::wait_until(
                    &parkable,
                    handle.watch(),
                    0,
                    WaitClass::Io,
                    deadline,
                    crate::drivers::serial::has_data,
                )
                .is_err()
                {
                    return cancelled();
                }
            }
            ReadBlock::Refused(word) => return word,
        }
    }
}

pub(super) fn sys_read_nonblock(h: RawHandle, buf: &mut UserBytesMut) -> u64 {
    let object = match readable(h, buf.len()) {
        Ok(Readable::Object(object)) => object,
        Ok(Readable::Device(read)) => return read.map_or(SyscallError::WouldBlock.to_u64(), |read| read.deliver(buf)),
        Err(e) => return e.refuse(),
    };
    match ops::try_read(&object, buf) {
        Some(n) => {
            if let Some(pipe) = ops::read_pipe(&object) { process::wake_pipe_writers(&pipe); }
            n
        }
        None => SyscallError::WouldBlock.to_u64(),
    }
}

pub(super) fn sys_write_nonblock(h: RawHandle, buf: &UserBytes) -> u64 {
    let object = match object(h, Rights::WRITE) {
        Ok(object) => object,
        Err(e) => return e.refuse(),
    };
    match ops::try_write(&object, buf) {
        Some(n) => {
            if let Some(pipe) = ops::written_pipe(&object) { process::wake_pipe_readers(&pipe); }
            n
        }
        None => SyscallError::WouldBlock.to_u64(),
    }
}

/// A fresh null handle carrying `want`, a subset of [`NullObject::RIGHTS`].
pub(super) fn sys_null(want: u64) -> u64 {
    let rights = match u32::try_from(want).ok().and_then(Rights::from_bits) {
        Some(rights) if NullObject::RIGHTS.contains(rights) => rights,
        _ => return SyscallError::InvalidArgument.to_u64(),
    };
    let object = match NullObject::new() {
        Ok(object) => KObjectRef::Null(object),
        Err(e) => return SyscallError::from(e).to_u64(),
    };
    let entry = crate::object::HandleEntry::new(object, rights);
    match process::with_process_data(|data| data.handles.install(entry)) {
        Ok(h) => h.0 as u64,
        Err(_) => SyscallError::ResourceExhausted.to_u64(),
    }
}

pub(super) fn sys_random(out: &mut UserBytesMut) -> u64 {
    crate::random::fill_user(out);
    0
}

/// `SYS_CONSOLE_CARRIED`: whether the holder of the console `h` carries the
/// log off the machine ([`crate::object::device::ConsoleObject::carry`]).
pub(super) fn sys_console_carried(h: RawHandle, elsewhere: u64) -> u64 {
    let elsewhere = match elsewhere {
        0 => false,
        1 => true,
        _ => return SyscallError::InvalidArgument.to_u64(),
    };
    let console = match process::with_process_data(|data| {
        data.handles.get::<crate::object::device::ConsoleObject>(h, Rights::WRITE)
    }) {
        Ok(console) => console,
        Err(e) => return e.refuse(),
    };
    match console.carry(elsewhere) {
        Ok(()) => 0,
        Err(kernel::carrier::Refused::Another) => SyscallError::AlreadyExists.to_u64(),
        Err(kernel::carrier::Refused::ForGood) => SyscallError::NotSupported.to_u64(),
    }
}
