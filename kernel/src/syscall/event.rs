//! Events, and the wait on one handle that any waitable object answers.

use kernel::event::Taken;
use kernel::sched::task::WaitClass;
use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::inbox::{OTHER_END_GONE, READABLE, WRITABLE};
use toyos_abi::syscall::SyscallError;

use crate::object::event::{EventSignal, EventWait};
use crate::object::ops::{self, WatchRef};
use crate::object::KObjectRef;
use crate::process;
use crate::time::{Deadline, Duration};
use crate::watch;

use super::cancelled;
use super::ipc::install_pair;

/// The signal side's handle in the high word, the wait side's in the low.
pub(super) fn sys_event_create() -> u64 {
    match crate::object::event::create() {
        Ok((signal, wait)) => install_pair(KObjectRef::EventSignal(signal), KObjectRef::EventWait(wait)),
        Err(e) => SyscallError::from(e).to_u64(),
    }
}

pub(super) fn sys_event_signal(h: RawHandle) -> u64 {
    match process::with_process_data(|data| data.handles.get::<EventSignal>(h, Rights::WRITE)) {
        Ok(event) if event.signal() => 0,
        Ok(_) => SyscallError::Gone.to_u64(),
        Err(e) => e.refuse(),
    }
}

/// `1` for a signal taken, `WouldBlock` for none pending, `Gone` for none
/// pending and no signaler left. Never blocks: a wait is `SYS_HANDLE_WAIT`.
pub(super) fn sys_event_take(h: RawHandle) -> u64 {
    match process::with_process_data(|data| data.handles.get::<EventWait>(h, Rights::READ)) {
        Ok(event) => match event.take() {
            Taken::Signal => 1,
            Taken::Empty => SyscallError::WouldBlock.to_u64(),
            Taken::Gone => SyscallError::Gone.to_u64(),
        },
        Err(e) => e.refuse(),
    }
}

/// Park until `h`'s object answers the one condition `interest` names, or
/// `timeout_nanos` passes (`u64::MAX` never does); answers the condition's
/// bit, or `0` at the deadline. The answer is the one an inbox's watch looks
/// at, so a readable wait on a notice makes its process a listener as a
/// readable poll does.
pub(super) fn sys_handle_wait(h: RawHandle, interest: u64, timeout_nanos: u64) -> u64 {
    let interest = match u32::try_from(interest) {
        Ok(bit @ (READABLE | WRITABLE | OTHER_END_GONE)) => bit,
        _ => return SyscallError::InvalidArgument.to_u64(),
    };
    let object = match process::with_process_data(|data| data.handles.get_ref(h, Rights::WAIT).cloned()) {
        Ok(object) => object,
        Err(e) => return e.refuse(),
    };
    let on = match interest {
        READABLE if ops::read_posts_are_readiness(&object) => None,
        READABLE => ops::read_watch(&object),
        WRITABLE => ops::write_watch(&object),
        _ => ops::pipe_end_watch(&object),
    };
    // A claim lends its watch to polls alone: its release answers them, and
    // a parked thread has no such answer.
    let Some(on) = on.filter(|w| !matches!(w, WatchRef::Claim(_))) else {
        return SyscallError::NotSupported.to_u64();
    };
    if interest == READABLE {
        ops::watched_readable(&object);
    }
    let ready = || match interest {
        READABLE => ops::has_data(&object),
        WRITABLE => ops::has_space(&object),
        _ => ops::other_end_gone(&object),
    };
    let deadline = match timeout_nanos {
        u64::MAX => Deadline::never(),
        nanos => Deadline::at(crate::clock::now() + Duration::from_nanos(nanos)),
    };
    let parkable = crate::scheduler::Parkable::at_entry();
    let waited = match &on {
        WatchRef::Static(w) => watch::wait_until(&parkable, *w, 0, WaitClass::Other, deadline, ready),
        WatchRef::InPlace(w) => watch::wait_until(&parkable, *w, 0, WaitClass::Other, deadline, ready),
        WatchRef::Shared(w) => watch::wait_until(&parkable, &**w, 0, WaitClass::Other, deadline, ready),
        WatchRef::Claim(_) => unreachable!("refused above"),
    };
    match waited {
        Ok(()) if ready() => u64::from(interest),
        Ok(()) => 0,
        Err(_) => cancelled(),
    }
}
