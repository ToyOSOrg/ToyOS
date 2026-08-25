//! `read` and `write` — the hot path of the whole ABI — and the four places a
//! read that came back empty parks.
//!
//! Both blocking calls are one loop: resolve the handle under the process's own
//! lock, ask the object, and if it gave nothing decide *where* to wait from the
//! object itself ([`ReadBlock`], [`WriteBlock`]) and park with the guard already
//! gone. Each variant carries what its own re-check needs, because the queue is
//! registered on *before* the condition is re-read — which is what closes the
//! check-then-block window — and nothing may hold a `with_process_data` guard
//! across the park, which the baseline tripwire refuses by construction.
//!
//! The buffers arrive already bounded: `super::dispatch` turned the caller's
//! pointer and length into a [`UserBytes`] or [`UserBytesMut`] window before
//! anything here ran, and neither type hands out a reference to a page userland
//! can still write.

use crate::completion;
use crate::object::{ops, KObjectRef};
use crate::time::{Cadence, Deadline, Duration};
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::{device, pipe, process};

use crate::arch::cpu;
use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;
use toyos_sched::task::WaitClass;

use super::cancelled;
use super::handles::with_object_ref;

/// What `sys_write` does when the object took nothing.
enum WriteBlock {
    Pipe(pipe::PipeId),
    Refused(u64),
    /// Carried out of the process's lock rather than answered inside it:
    /// `HandleError::refuse` may take the process down and cannot run under a
    /// guard.
    BadHandle(crate::object::HandleError),
}

/// What `sys_read` parks on when the handle has nothing to give. Each variant
/// carries what its own re-check needs — the queue is registered on *before*
/// the condition is re-read, which is what closes the check-then-block window.
enum ReadBlock {
    Pipe(pipe::PipeEnd, pipe::PipeId),
    VirtioSound,
    Hda,
    /// A console read re-polls, because nothing posts a serial key; a claimed
    /// keyboard is woken by its own IRQ and waits with [`Deadline::never`].
    Keyboard(Deadline),
    /// Nothing to wait for: the answer is this word.
    Refused(u64),
    /// Carried out of the process's lock rather than answered inside it:
    /// `HandleError::refuse` may take the process down and cannot run under a
    /// guard.
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
                None => Err(match ops::pipe_id_write(object) {
                    Some(id) => WriteBlock::Pipe(id),
                    None => WriteBlock::Refused(SyscallError::NotFound.to_u64()),
                }),
            }
        });
        match action {
            Ok((n, pipe_id)) => {
                if let Some(id) = pipe_id { process::wake_pipe_readers(id); }
                return n;
            }
            Err(WriteBlock::Pipe(id)) => match pipe::writers_queue(id) {
                Some(end) => {
                    let parkable = crate::scheduler::Parkable::at_entry();
                    if completion::wait_until(
                        &parkable,
                        completion::Subject::of(&end.watch),
                        completion::Token::new(0),
                        WaitClass::Pipe,
                        Deadline::never(),
                        || pipe::has_space(id),
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

/// Where a read that came back empty parks, decided from the object itself.
///
/// Only these four wait. A mouse, a NIC and a framebuffer answer `NotFound` to
/// a blocking read that has nothing — which is what they did as descriptors,
/// and what their holders already build around.
fn read_block_device(claim: &crate::object::device::DeviceClaim) -> ReadBlock {
    match claim.class() {
        device::DeviceType::Keyboard => ReadBlock::Keyboard(Deadline::never()),
        device::DeviceType::VirtioSound if claim.info_read() => ReadBlock::VirtioSound,
        device::DeviceType::HdaAudio if claim.info_read() => ReadBlock::Hda,
        _ => ReadBlock::Refused(SyscallError::NotFound.to_u64()),
    }
}

fn read_block(object: &KObjectRef) -> ReadBlock {
    match object {
        KObjectRef::Device(_) => unreachable!("a device claim blocks via `read_block_device`"),
        KObjectRef::Console(_) => {
            /// **The only reason a serial-console read ever returns.**
            /// The park is on `waitqs::KEYBOARD`, whose only waker is the
            /// i8042/USB keyboard — a *different* device from the one this
            /// read is about. The 16550's IER is written to zero and
            /// `virtio_console` has no handler at all, so nothing posts a
            /// serial key and what ends this wait is the re-poll and nothing
            /// else. It is the [`Cadence`] of a poll on
            /// `serial::has_data`, which C10 makes explicit.
            const CONSOLE_REPOLL: Cadence = Cadence::every(
                Duration::from_millis(10),
                "nothing posts a serial-console key, so this rate is the whole of the wake",
            );
            ReadBlock::Keyboard(Deadline::at(crate::clock::now() + CONSOLE_REPOLL.duration()))
        }
        _ => match ops::pipe_id_read(object).and_then(|id| {
            pipe::readers_queue(id).map(|end| ReadBlock::Pipe(end, id))
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
            // A device description installs handles for the buffers it names,
            // so that one arm needs the table mutably and the borrow above has
            // to end first. Resolving twice costs a slot lookup on a path that
            // runs once per device per boot; cloning the `Arc` on the common
            // path would cost an atomic read-modify-write on the hottest
            // syscall in the kernel.
            if matches!(object, KObjectRef::Device(_)) {
                let claim = data
                    .handles
                    .get::<crate::object::device::DeviceClaim>(h, Rights::READ)
                    .expect("a Device resolved a moment ago under this same hold");
                let blocked = read_block_device(&claim);
                return match ops::read_device(&claim, &mut data.handles, buf) {
                    Some(n) => Ok((n, None)),
                    None => Err(blocked),
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
            Err(ReadBlock::Pipe(end, id)) => {
                let parkable = crate::scheduler::Parkable::at_entry();
                if completion::wait_until(
                    &parkable,
                    completion::Subject::of(&end.watch),
                    completion::Token::new(0),
                    WaitClass::Pipe,
                    Deadline::never(),
                    || pipe::has_data(id),
                )
                .is_err()
                {
                    return cancelled();
                }
            }
            Err(ReadBlock::VirtioSound) => {
                let parkable = crate::scheduler::Parkable::at_entry();
                if completion::wait_until(
                    &parkable,
                    completion::Subject::of(&crate::sched::waitqs::AUDIO_WATCH),
                    completion::Token::new(0),
                    WaitClass::Io,
                    Deadline::never(),
                    crate::drivers::virtio_sound::has_pending,
                )
                .is_err()
                {
                    return cancelled();
                }
            }
            Err(ReadBlock::Hda) => {
                let parkable = crate::scheduler::Parkable::at_entry();
                if completion::wait_until(
                    &parkable,
                    completion::Subject::of(&crate::sched::waitqs::AUDIO_WATCH),
                    completion::Token::new(0),
                    WaitClass::Io,
                    Deadline::never(),
                    crate::drivers::hda::has_pending,
                )
                .is_err()
                {
                    return cancelled();
                }
            }
            Err(ReadBlock::Keyboard(deadline)) => {
                let parkable = crate::scheduler::Parkable::at_entry();
                if completion::wait_until(
                    &parkable,
                    completion::Subject::of(&crate::sched::waitqs::KEYBOARD_WATCH),
                    completion::Token::new(0),
                    WaitClass::Io,
                    deadline,
                    crate::keyboard::has_data,
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
        // The two-step [`sys_read`] makes, for the same reason: a device
        // description installs handles for the buffers it names, so that arm
        // needs the table mutably and the borrow above has to end first. This
        // is the path the compositor reads its keyboard and its mouse on.
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

pub(super) fn sys_random(out: &mut UserBytesMut) -> u64 {
    let mut i = 0;
    while i + 8 <= out.len() {
        out.write_at(i, &cpu::rdrand().to_ne_bytes());
        i += 8;
    }
    let remaining = out.len() - i;
    if remaining > 0 {
        let bytes = cpu::rdrand().to_ne_bytes();
        out.write_at(i, &bytes[..remaining]);
    }
    0
}
