//! What `signal` and `sigaction` keep and refuse, what a signal raised does
//! and which thread takes it (`sigreq.rs`), against POSIX's words (XSH 2.4)
//! and by the numbers `signal.h` gives each signal: a handler kept for a
//! signal an event raises and refused for another, a blocked signal pending
//! until a thread unblocks it, and a handler run in a blocking call before on
//! libc's own thread.

use crate::alarmreq::{SIG_DFL, SIG_IGN};
use crate::errcode::{EINVAL, ENOSYS};
use crate::header::{self, SIGNAL_H};
use crate::sigreq::{bit, index, kept, raised, set, Delivery, Kept, Raised, SigAction, Signals, SIGNALS};

fn sig(name: &str) -> i32 {
    header::int(SIGNAL_H, name)
}

fn flag(name: &str) -> u64 {
    header::flags(SIGNAL_H, name)
}

/// A handler's address, as a program's function would be.
const HANDLER: usize = 0x4000;

fn handler(flags: u64) -> SigAction {
    SigAction { flags, ..SigAction::of(HANDLER) }
}

#[test]
fn a_handler_is_kept_for_a_signal_an_event_raises_and_refused_for_another() {
    for name in ["SIGHUP", "SIGINT", "SIGTERM", "SIGCHLD", "SIGALRM"] {
        assert_eq!(set(sig(name), handler(0)).map(kept), Ok(Kept::Handle(handler(0))), "{name}");
    }
    for name in ["SIGPIPE", "SIGUSR1", "SIGSEGV"] {
        assert_eq!(set(sig(name), handler(0)), Err(EINVAL), "{name}: a handler that is never called");
    }
    for name in ["SIGHUP", "SIGPIPE", "SIGCHLD"] {
        assert_eq!(set(sig(name), SigAction::of(SIG_DFL)).map(kept), Ok(Kept::Default), "{name}");
        assert_eq!(set(sig(name), SigAction::of(SIG_IGN)).map(kept), Ok(Kept::Ignore), "{name}");
    }
    let masked = SigAction { mask: bit(sig("SIGKILL")) | bit(sig("SIGUSR1")), ..handler(0) };
    assert_eq!(set(sig("SIGINT"), masked), Ok(SigAction { mask: bit(sig("SIGUSR1")), ..masked }), "SIGKILL in a mask");
    let ignored = SigAction { flags: flag("SA_RESTART"), mask: bit(sig("SIGUSR1")), ..SigAction::of(SIG_IGN) };
    assert_eq!(set(sig("SIGPIPE"), ignored), Ok(ignored), "an ignore is kept whole, as sigaction reads it back");
}

#[test]
fn kill_stop_and_no_signal_are_refused() {
    for action in [SigAction::of(SIG_DFL), SigAction::of(SIG_IGN), handler(0)] {
        assert_eq!(set(sig("SIGKILL"), action), Err(EINVAL));
        assert_eq!(set(sig("SIGSTOP"), action), Err(EINVAL));
    }
    for signum in [0, -1, SIGNALS as i32 + 1] {
        assert_eq!(index(signum), Err(EINVAL), "{signum}");
        assert_eq!(set(signum, SigAction::of(SIG_DFL)), Err(EINVAL), "{signum}");
    }
    assert_eq!(index(1), Ok(0));
    assert_eq!(index(SIGNALS as i32), Ok(SIGNALS - 1));
}

/// An ignored signal and one whose default ignores do nothing, a terminating
/// default ends the process with 128 plus the number, a stop or no signal is
/// refused, and a blocked one waits pending.
#[test]
fn raise_does_what_the_disposition_and_the_mask_say() {
    let term = sig("SIGTERM");
    assert_eq!(raised(term, Kept::Default, 0), Raised::Deliver(Delivery::End(128 + term)));
    assert_eq!(raised(term, Kept::Ignore, 0), Raised::Nothing);
    assert_eq!(raised(sig("SIGABRT"), Kept::Default, 0), Raised::Deliver(Delivery::End(134)), "abort's own code");
    assert_eq!(raised(sig("SIGCHLD"), Kept::Default, 0), Raised::Nothing, "a default that ignores");
    assert_eq!(raised(sig("SIGKILL"), Kept::Default, u64::MAX), Raised::Deliver(Delivery::End(128 + 9)), "no mask holds it");
    assert_eq!(raised(sig("SIGSTOP"), Kept::Default, 0), Raised::Refused(ENOSYS));
    assert_eq!(raised(0, Kept::Default, 0), Raised::Refused(EINVAL));
    assert_eq!(raised(term, Kept::Default, bit(term)), Raised::Pending);
    assert_eq!(raised(term, Kept::Ignore, bit(term)), Raised::Nothing);
    assert_eq!(raised(term, Kept::Default, !bit(term)), Raised::Deliver(Delivery::End(128 + term)), "another signal's bit");
}

/// A handler's run blocks its own signal unless `SA_NODEFER`, adds its
/// `sa_mask`, restarts with `SA_RESTART`, and is the default's after with
/// `SA_RESETHAND`.
#[test]
fn a_delivery_carries_what_its_flags_say() {
    let (alrm, usr1) = (sig("SIGALRM"), sig("SIGUSR1"));
    let mut signals = Signals::new();
    signals.act(alrm, Some(SigAction { mask: bit(usr1), ..handler(flag("SA_RESTART")) })).unwrap();
    assert_eq!(
        signals.arrive(alrm),
        Raised::Deliver(Delivery::Handle { sig: alrm, handler: HANDLER, siginfo: false, mask: bit(usr1) | bit(alrm), restart: true }),
    );
    signals.act(alrm, Some(handler(flag("SA_NODEFER") | flag("SA_RESETHAND") | flag("SA_SIGINFO")))).unwrap();
    assert_eq!(
        signals.arrive(alrm),
        Raised::Deliver(Delivery::Handle { sig: alrm, handler: HANDLER, siginfo: true, mask: 0, restart: false }),
    );
    assert_eq!(signals.kept(alrm), Kept::Default, "SA_RESETHAND left the handler");
}

/// An arrival runs on libc's own thread while no thread is in a blocking
/// call, goes pending for a call that leaves it unblocked first, waits while
/// every thread blocks it, and once pending is not raised twice.
#[test]
fn an_arrival_goes_to_a_blocking_call_then_to_libcs_own_thread_and_waits_while_every_thread_blocks_it() {
    let chld = sig("SIGCHLD");
    let mut signals = Signals::new();
    signals.act(chld, Some(handler(0))).unwrap();
    assert!(matches!(signals.arrive(chld), Raised::Deliver(Delivery::Handle { .. })), "no thread in a call");

    signals.enter(0, 0, false);
    assert_eq!(signals.arrive(chld), Raised::Pending, "a thread in a call leaves it unblocked");
    assert!(signals.wake_wanted());
    assert_eq!(signals.arrive(chld), Raised::Nothing, "pending already");
    assert!(matches!(signals.take(0), Some(Delivery::Handle { sig, .. }) if sig == chld));
    assert!(!signals.wake_wanted());
    signals.leave(0);

    signals.set_mask(0, bit(chld));
    signals.set_mask(7, bit(chld));
    assert_eq!(signals.arrive(chld), Raised::Pending, "every thread blocks it");
    assert_eq!(signals.pending(), bit(chld));
    assert_eq!(signals.take(bit(chld)), None, "a blocking mask takes nothing");
    signals.set_mask(7, 0);
    assert!(signals.take(signals.mask(7)).is_some(), "the thread that unblocks it takes it");
    assert_eq!(signals.pending(), 0);
    signals.thread_ended(7);
    assert_eq!(signals.arrive(chld), Raised::Pending, "the first thread alone blocks it");
}

/// An ignored signal pending is dropped by its ignore, and a quit's three
/// signals ask libc to listen once any is handled, ignored or blocked.
#[test]
fn an_ignore_drops_what_is_pending_and_a_quit_signal_off_its_default_listens() {
    let (term, hup, alrm) = (sig("SIGTERM"), sig("SIGHUP"), sig("SIGALRM"));
    let mut signals = Signals::new();
    assert!(!signals.listens());
    signals.set_mask(0, bit(alrm));
    assert_eq!(signals.arrive(alrm), Raised::Pending);
    signals.act(alrm, Some(SigAction::of(SIG_IGN))).unwrap();
    assert_eq!(signals.pending(), 0, "ignoring a pending signal drops it");
    assert!(!signals.listens(), "SIGALRM is no quit's");
    signals.set_mask(3, bit(term));
    assert!(signals.listens(), "a thread blocks SIGTERM");
    signals.thread_ended(3);
    assert!(!signals.listens());
    signals.act(hup, Some(SigAction::of(SIG_IGN))).unwrap();
    assert!(signals.listens(), "SIGHUP is ignored");
    assert_eq!(signals.arrive(hup), Raised::Nothing);
}

/// A wait for children looks again at every turn of them, and only once.
#[test]
fn a_wait_for_children_is_woken_by_each_turn_once() {
    let mut signals = Signals::new();
    signals.enter(0, 0, true);
    assert!(!signals.wake_wanted());
    signals.children_changed();
    assert!(signals.wake_wanted(), "a child started under a wait");
    signals.children_seen(0);
    assert!(!signals.wake_wanted());
    signals.leave(0);
    signals.children_changed();
    assert!(!signals.wake_wanted(), "no wait is there to look");
}

/// `raise` in a thread that blocks the signal leaves it pending for that
/// thread to take on unblocking; a handler makes blocking calls interruptible.
#[test]
fn a_raise_a_thread_blocks_waits_for_it_to_unblock() {
    let (int, usr1) = (sig("SIGINT"), sig("SIGUSR1"));
    let mut signals = Signals::new();
    assert!(!signals.any_handler());
    signals.act(int, Some(handler(0))).unwrap();
    assert!(signals.any_handler());
    signals.set_mask(4, bit(int));
    assert_eq!(signals.raise(4, int), Raised::Pending);
    assert!(matches!(signals.raise(0, int), Raised::Deliver(Delivery::Handle { .. })), "another thread takes it at once");
    assert_eq!(signals.raise(4, usr1), Raised::Deliver(Delivery::End(128 + usr1)));
    assert_eq!(signals.raise(4, 0), Raised::Refused(EINVAL));
    assert!(matches!(signals.take(0), Some(Delivery::Handle { sig, .. }) if sig == int));
}
