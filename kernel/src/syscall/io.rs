//! Blocking `read`/`write` register on the object's watch before re-checking the
//! condition — closing the check-then-block window — and never park while
//! holding a `with_process_data` guard.
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
use super::handles::with_object_ref;

/// What `sys_write` does when the object took nothing.
enum WriteBlock {
    Pipe(pipe::PipeId, WaitClass),
    Refused(u64),
    /// Carried out of the process's lock: `HandleError::refuse` may take the
    /// process down and cannot run under a guard.
    BadHandle(crate::object::HandleError),
}

/// What `sys_read` parks on when the handle has nothing to give.
enum ReadBlock {
    Pipe(alloc::sync::Arc<crate::watch::Watch>, pipe::PipeId, WaitClass),
    /// A console, which reads the serial line and nothing else: nothing posts
    /// a serial byte, so it parks on its own thread's watch until its re-poll.
    Console(Deadline),
    /// Nothing to wait for: the answer is this word.
    Refused(u64),
    /// Carried out of the process's lock: `HandleError::refuse` may take the
    /// process down and cannot run under a guard.
    BadHandle(crate::object::HandleError),
}

pub(super) fn sys_write(h: RawHandle, buf: &UserBytes) -> u64 {
    loop {
        let action = process::with_process_data(|data| {
            let object = match data.handles.get_ref(h, Rights::WRITE) {
                Ok(object) => object,
                Err(e) => return Err(WriteBlock::BadHandle(e)),
            };
            match ops::try_write(object, buf) {
                Some(n) => Ok((n, ops::pipe_id_write(object))),
                None => Err(match ops::pipe_write(object) {
                    Some((id, class)) => WriteBlock::Pipe(id, class),
                    None => WriteBlock::Refused(SyscallError::NotFound.to_u64()),
                }),
            }
        });
        match action {
            Ok((n, pipe_id)) => {
                if let Some(id) = pipe_id { process::wake_pipe_readers(id); }
                return n;
            }
            Err(WriteBlock::Pipe(id, class)) => match pipe::write_watch(id) {
                Some(end) => {
                    let parkable = crate::scheduler::Parkable::at_entry();
                    if watch::wait_until(
                        &parkable,
                        &end,
                        0,
                        class,
                        Deadline::never(),
                        || pipe::has_space(id, 1),
                    )
                    .is_err()
                    {
                        return cancelled();
                    }
                }
                None => return SyscallError::NotFound.to_u64(),
            },
            Err(WriteBlock::Refused(word)) => return word,
            Err(WriteBlock::BadHandle(e)) => return e.refuse(),
        }
    }
}

fn read_block(object: &KObjectRef) -> ReadBlock {
    match object {
        KObjectRef::Device(_) => unreachable!("a device claim blocks via `read_block_device`"),
        KObjectRef::Console(_) => {
            // Nothing posts a serial-console key, so the timer alone wakes it.
            const CONSOLE_REPOLL: Cadence = Cadence::every(
                Duration::from_millis(10),
                "nothing posts a serial-console key, so this rate is the whole of the wake",
            );
            ReadBlock::Console(Deadline::at(crate::clock::now() + CONSOLE_REPOLL.duration()))
        }
        _ => match ops::pipe_read(object).and_then(|(id, class)| {
            pipe::read_watch(id).map(|end| ReadBlock::Pipe(end, id, class))
        }) {
            Some(block) => block,
            None => ReadBlock::Refused(SyscallError::NotFound.to_u64()),
        },
    }
}

pub(super) fn sys_read(h: RawHandle, buf: &mut UserBytesMut) -> u64 {
    loop {
        let action = process::with_process_data(|data| {
            let object = match data.handles.get_ref(h, Rights::READ) {
                Ok(object) => object,
                Err(e) => return Err(ReadBlock::BadHandle(e)),
            };
            // Resolved twice rather than cached: this path runs once per
            // device per boot, but cloning the `Arc` would cost an atomic
            // RMW on the hottest syscall in the kernel.
            if matches!(object, KObjectRef::Device(_)) {
                let claim = data
                    .handles
                    .get::<crate::object::device::DeviceClaim>(h, Rights::READ)
                    .expect("a Device resolved a moment ago under this same hold");
                // No device class blocks: an empty read answers `NotFound`.
                return match ops::read_device(&claim, &mut data.handles, buf) {
                    Some(n) => Ok((n, None)),
                    None => Err(ReadBlock::Refused(SyscallError::NotFound.to_u64())),
                };
            }
            match ops::try_read(object, buf) {
                Some(n) => Ok((n, ops::pipe_id_read(object))),
                None => Err(read_block(object)),
            }
        });
        match action {
            Ok((n, pipe_id)) => {
                if let Some(id) = pipe_id { process::wake_pipe_writers(id); }
                return n;
            }
            Err(ReadBlock::Pipe(end, id, class)) => {
                let parkable = crate::scheduler::Parkable::at_entry();
                if watch::wait_until(
                    &parkable,
                    &end,
                    0,
                    class,
                    Deadline::never(),
                    || pipe::has_data(id),
                )
                .is_err()
                {
                    return cancelled();
                }
            }
            Err(ReadBlock::Console(deadline)) => {
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
            Err(ReadBlock::Refused(word)) => return word,
            Err(ReadBlock::BadHandle(e)) => return e.refuse(),
        }
    }
}

pub(super) fn sys_read_nonblock(h: RawHandle, buf: &mut UserBytesMut) -> u64 {
    let result = process::with_process_data(|data| {
        let object = match data.handles.get_ref(h, Rights::READ) {
            Ok(object) => object,
            Err(e) => return Err(e),
        };
        // Same two-step as `sys_read`, for the same reason.
        if matches!(object, KObjectRef::Device(_)) {
            let claim = data
                .handles
                .get::<crate::object::device::DeviceClaim>(h, Rights::READ)
                .expect("a Device resolved a moment ago under this same hold");
            return Ok((ops::read_device(&claim, &mut data.handles, buf), None));
        }
        Ok((ops::try_read(object, buf), ops::pipe_id_read(object)))
    });
    match result {
        Ok((Some(n), wake)) => {
            if let Some(id) = wake { process::wake_pipe_writers(id); }
            n
        }
        Ok((None, _)) => SyscallError::WouldBlock.to_u64(),
        Err(e) => e.refuse(),
    }
}

pub(super) fn sys_write_nonblock(h: RawHandle, buf: &UserBytes) -> u64 {
    let result = with_object_ref(h, Rights::WRITE, |object| {
        (ops::try_write(object, buf), ops::pipe_id_write(object))
    });
    match result {
        Ok((Some(n), wake)) => {
            if let Some(id) = wake { process::wake_pipe_readers(id); }
            n
        }
        Ok((None, _)) => SyscallError::WouldBlock.to_u64(),
        Err(e) => e.refuse(),
    }
}

/// A fresh null handle carrying `want`, a subset of [`NullObject::RIGHTS`].
pub(super) fn sys_null(want: u64) -> u64 {
    let rights = match u32::try_from(want).ok().and_then(Rights::from_bits) {
        Some(rights) if NullObject::RIGHTS.contains(rights) => rights,
        _ => return SyscallError::InvalidArgument.to_u64(),
    };
    let object = KObjectRef::Null(NullObject::new());
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
