//! What libc's signals decide: what `signal` and `sigaction` keep, what a
//! signal raised does, and which thread takes it. It reads and sets nothing
//! but what it is handed, so the host tests it (`toyos-libc-copies`); what it
//! decides is performed by `signal.rs`.
//!
//! **A handler is kept only for a signal an event raises**: `SIGCHLD` a
//! child's end, `SIGALRM` an alarm come due, and `SIGHUP`, `SIGINT` and
//! `SIGTERM` the three reasons a quit asks. A handler for any other would
//! never be called, so it is refused `EINVAL`. `raise` raises any signal,
//! which its disposition decides.
//!
//! **A signal raised runs at once, beside the program**: in a blocking call of
//! a thread that leaves it unblocked, or else on libc's own thread while some
//! thread leaves it unblocked. While every thread blocks it, it is pending
//! until one unblocks it, and a signal pending that no blocking call takes
//! goes to libc's own thread once some thread leaves it unblocked. The
//! threads a signal is routed by are the first (tid 0) and each one libc
//! started: no other thread may set a mask, since nothing tells libc when it
//! ends, and libc's own thread is none of them.

use alloc::vec::Vec;

use crate::alarmreq::{SIG_DFL, SIG_IGN};
use crate::errcode::{EINVAL, ENOSYS};

/// The signal numbers `signal.h`'s `sigset_t` holds.
pub(crate) const SIGNALS: usize = 64;

pub(crate) const SIGHUP: i32 = 1;
pub(crate) const SIGINT: i32 = 2;
pub(crate) const SIGKILL: i32 = 9;
pub(crate) const SIGALRM: i32 = 14;
pub(crate) const SIGTERM: i32 = 15;
pub(crate) const SIGCHLD: i32 = 17;
const SIGCONT: i32 = 18;
const SIGSTOP: i32 = 19;
const SIGTSTP: i32 = 20;
const SIGTTIN: i32 = 21;
const SIGTTOU: i32 = 22;
const SIGURG: i32 = 23;
const SIGWINCH: i32 = 28;

const SA_SIGINFO: u64 = 4;
pub(crate) const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;

/// Signal `sig`'s bit in a `sigset_t`; `sig` is one of [`SIGNALS`].
pub(crate) const fn bit(sig: i32) -> u64 {
    1 << (sig - 1)
}

/// What a quit's three reasons are raised as.
pub(crate) const QUITS: u64 = bit(SIGHUP) | bit(SIGINT) | bit(SIGTERM);

/// What an event raises, and so what a handler may be kept for.
const RAISED_BY_EVENTS: u64 = QUITS | bit(SIGCHLD) | bit(SIGALRM);

/// What no disposition changes and no mask holds.
const UNCATCHABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

/// Whose default action leaves the process running.
const DEFAULT_IGNORES: u64 = bit(SIGCHLD) | bit(SIGCONT) | bit(SIGURG) | bit(SIGWINCH);

/// Whose default action stops the process, which ToyOS has no state for.
const DEFAULT_STOPS: u64 = bit(SIGSTOP) | bit(SIGTSTP) | bit(SIGTTIN) | bit(SIGTTOU);

/// `<signal.h>`'s `struct sigaction`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SigAction {
    pub(crate) handler: usize,
    pub(crate) flags: u64,
    pub(crate) restorer: usize,
    pub(crate) mask: u64,
}

impl SigAction {
    /// The action of `handler` alone, as `signal` makes one.
    pub(crate) const fn of(handler: usize) -> Self {
        Self { handler, flags: 0, restorer: 0, mask: 0 }
    }
}

/// A signal's disposition, as its kept action makes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    Default,
    Ignore,
    /// A function, with the flags and the mask it was installed with.
    Handle(SigAction),
}

/// The mask a call on libc's own thread runs under: every signal.
pub(crate) const KEEPER_MASK: u64 = u64::MAX;

/// The index of signal `signum` in a table of [`SIGNALS`], or `EINVAL` for a
/// number that names none.
pub(crate) fn index(signum: i32) -> Result<usize, i32> {
    usize::try_from(signum).ok().filter(|&n| (1..=SIGNALS).contains(&n)).map(|n| n - 1).ok_or(EINVAL)
}

/// The action setting `signum`'s to `action` keeps, whole as `sigaction`
/// answers it but for `SIGKILL` and `SIGSTOP` in its mask, or the `errno` it
/// is refused with: `EINVAL` for a number that names no signal, for
/// `SIGKILL`'s and `SIGSTOP`'s, whose dispositions POSIX lets nobody change,
/// and for a handler of a signal no event raises.
pub(crate) fn set(signum: i32, action: SigAction) -> Result<SigAction, i32> {
    index(signum)?;
    if bit(signum) & UNCATCHABLE != 0 {
        return Err(EINVAL);
    }
    let handles = !matches!(action.handler, SIG_DFL | SIG_IGN);
    if handles && bit(signum) & RAISED_BY_EVENTS == 0 {
        return Err(EINVAL);
    }
    Ok(SigAction { mask: action.mask & !UNCATCHABLE, ..action })
}

/// The disposition an action makes.
pub(crate) fn kept(action: SigAction) -> Kept {
    match action.handler {
        SIG_DFL => Kept::Default,
        SIG_IGN => Kept::Ignore,
        _ => Kept::Handle(action),
    }
}

/// What the thread that takes a signal does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// End the process with this code, the default action's: 128 plus the
    /// signal, as a shell reads one.
    End(i32),
    /// Run the handler at `handler` for `sig`, with `mask` added to the
    /// thread's for its run.
    Handle { sig: i32, handler: usize, siginfo: bool, mask: u64, restart: bool },
}

/// What a signal raised does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Raised {
    /// Ignored, by the program or by the signal's default action, or pending
    /// already.
    Nothing,
    /// It waits pending: for a thread in a blocking call to take it, or for
    /// one to unblock it.
    Pending,
    /// Delivered now, on the raising thread or libc's own.
    Deliver(Delivery),
    /// Refused with this `errno`: a number that names no signal, or a default
    /// action that stops the process.
    Refused(i32),
}

/// What `raise` of `signum` does under disposition `kept` in a thread whose
/// mask is `mask`.
pub(crate) fn raised(signum: i32, kept: Kept, mask: u64) -> Raised {
    if let Err(errno) = index(signum) {
        return Raised::Refused(errno);
    }
    match delivery(signum, kept) {
        Err(errno) => Raised::Refused(errno),
        Ok(None) => Raised::Nothing,
        Ok(Some(_)) if mask & bit(signum) & !UNCATCHABLE != 0 => Raised::Pending,
        Ok(Some(delivery)) => Raised::Deliver(delivery),
    }
}

/// What taking `signum` under `kept` does: `None` for one ignored, `ENOSYS`
/// for a default that stops.
fn delivery(signum: i32, kept: Kept) -> Result<Option<Delivery>, i32> {
    match kept {
        Kept::Ignore => Ok(None),
        Kept::Default if bit(signum) & DEFAULT_IGNORES != 0 => Ok(None),
        Kept::Default if bit(signum) & DEFAULT_STOPS != 0 => Err(ENOSYS),
        Kept::Default => Ok(Some(Delivery::End(128 + signum))),
        Kept::Handle(action) => {
            let own = if action.flags & SA_NODEFER != 0 { 0 } else { bit(signum) };
            Ok(Some(Delivery::Handle {
                sig: signum,
                handler: action.handler,
                siginfo: action.flags & SA_SIGINFO != 0,
                mask: (action.mask | own) & !UNCATCHABLE,
                restart: action.flags & SA_RESTART != 0,
            }))
        }
    }
}

/// A thread in a blocking call: its tid, what it blocks while there, and, for
/// a wait for children, which turn of them it last saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Waiter {
    tid: u32,
    mask: u64,
    children: Option<u64>,
}

/// A process's signals.
pub(crate) struct Signals {
    actions: [SigAction; SIGNALS],
    pending: u64,
    /// Each thread's mask but the first's, by tid.
    masks: Vec<(u32, u64)>,
    first_mask: u64,
    waiters: Vec<Waiter>,
    /// Moved by every start and end of a child, which a wait for children
    /// looks at again.
    children: u64,
}

/// The first thread's tid, which the kernel gives every process's first.
const FIRST: u32 = 0;

impl Signals {
    pub(crate) const fn new() -> Self {
        Self { actions: [SigAction::of(SIG_DFL); SIGNALS], pending: 0, masks: Vec::new(), first_mask: 0, waiters: Vec::new(), children: 0 }
    }

    pub(crate) fn kept(&self, sig: i32) -> Kept {
        kept(self.actions[sig as usize - 1])
    }

    /// `sigaction`: make `new`, if any, `sig`'s action, and answer the one it
    /// replaces. A pending signal the new action ignores is dropped.
    pub(crate) fn act(&mut self, sig: i32, new: Option<SigAction>) -> Result<SigAction, i32> {
        let at = index(sig)?;
        let old = self.actions[at];
        if let Some(new) = new {
            self.actions[at] = set(sig, new)?;
            if matches!(delivery(sig, self.kept(sig)), Ok(None)) {
                self.pending &= !bit(sig);
            }
        }
        Ok(old)
    }

    /// Whether any handler is kept, which a blocking call is interruptible for.
    pub(crate) fn any_handler(&self) -> bool {
        self.actions.iter().any(|&action| matches!(kept(action), Kept::Handle(_)))
    }

    /// Whether this process must listen to its notice: a quit kills a process
    /// that does not, which is right only while each of the three signals a
    /// quit raises is at its default and no thread blocks it.
    pub(crate) fn listens(&self) -> bool {
        let blocked = self.masks.iter().fold(self.first_mask, |all, &(_, mask)| all | mask);
        [SIGHUP, SIGINT, SIGTERM].into_iter().any(|sig| self.kept(sig) != Kept::Default) || blocked & QUITS != 0
    }

    /// What is pending, as `sigpending` answers it.
    pub(crate) fn pending(&self) -> u64 {
        self.pending
    }

    pub(crate) fn mask(&self, tid: u32) -> u64 {
        match tid {
            FIRST => self.first_mask,
            _ => self.masks.iter().find(|(t, _)| *t == tid).map_or(0, |&(_, mask)| mask),
        }
    }

    /// `tid`'s mask is now `mask`, without `SIGKILL` and `SIGSTOP`: `false`,
    /// and nothing changed, for a thread neither the first nor started by
    /// libc.
    pub(crate) fn set_mask(&mut self, tid: u32, mask: u64) -> bool {
        let mask = mask & !UNCATCHABLE;
        if tid == FIRST {
            self.first_mask = mask;
            return true;
        }
        match self.masks.iter_mut().find(|(t, _)| *t == tid) {
            Some((_, m)) => *m = mask,
            None => return false,
        }
        true
    }

    /// A thread libc started runs from now with `mask`.
    pub(crate) fn thread_started(&mut self, tid: u32, mask: u64) {
        self.masks.push((tid, mask & !UNCATCHABLE));
    }

    /// A thread libc started has ended.
    pub(crate) fn thread_ended(&mut self, tid: u32) {
        self.masks.retain(|&(t, _)| t != tid);
    }

    /// A signal an event raised, as libc's own thread takes it.
    pub(crate) fn arrive(&mut self, sig: i32) -> Raised {
        debug_assert!(bit(sig) & RAISED_BY_EVENTS != 0, "signal {sig} is raised by no event");
        let delivery = match delivery(sig, self.kept(sig)) {
            Ok(Some(delivery)) if self.pending & bit(sig) == 0 => delivery,
            Ok(_) | Err(_) => return Raised::Nothing,
        };
        if self.a_waiter_takes(sig) || !self.a_thread_free(sig) {
            self.pending |= bit(sig);
            return Raised::Pending;
        }
        self.reset(sig);
        Raised::Deliver(delivery)
    }

    fn a_waiter_takes(&self, sig: i32) -> bool {
        self.waiters.iter().any(|w| w.mask & bit(sig) == 0)
    }

    /// Whether a thread in no blocking call leaves `sig` unblocked.
    fn a_thread_free(&self, sig: i32) -> bool {
        let waiting = |tid: u32| self.waiters.iter().any(|w| w.tid == tid);
        let first_free = !waiting(FIRST) && self.first_mask & bit(sig) == 0;
        first_free || self.masks.iter().any(|&(t, m)| !waiting(t) && m & bit(sig) == 0)
    }

    /// The lowest pending signal no blocking call will take and some thread
    /// in none leaves unblocked, taken for libc's own thread: what a call
    /// that left without taking it, or a mask that unblocked it in another
    /// thread, left behind.
    pub(crate) fn orphan(&mut self) -> Option<Delivery> {
        let sig = (1..=SIGNALS as i32).find(|&sig| self.pending & bit(sig) != 0 && self.orphaned(sig))?;
        self.pending &= !bit(sig);
        let delivery = delivery(sig, self.kept(sig)).ok().flatten()?;
        self.reset(sig);
        Some(delivery)
    }

    pub(crate) fn has_orphan(&self) -> bool {
        (1..=SIGNALS as i32).any(|sig| self.pending & bit(sig) != 0 && self.orphaned(sig))
    }

    fn orphaned(&self, sig: i32) -> bool {
        !self.a_waiter_takes(sig) && self.a_thread_free(sig)
    }

    /// `raise(sig)` in thread `tid`.
    pub(crate) fn raise(&mut self, tid: u32, sig: i32) -> Raised {
        let Ok(at) = index(sig) else { return Raised::Refused(EINVAL) };
        let raised = raised(sig, kept(self.actions[at]), self.mask(tid));
        match raised {
            Raised::Pending => self.pending |= bit(sig),
            Raised::Deliver(_) => self.reset(sig),
            Raised::Nothing | Raised::Refused(_) => {}
        }
        raised
    }

    /// The lowest signal pending that `mask` leaves unblocked, taken.
    pub(crate) fn take(&mut self, mask: u64) -> Option<Delivery> {
        let open = self.pending & !mask;
        if open == 0 {
            return None;
        }
        let sig = open.trailing_zeros() as i32 + 1;
        self.pending &= !bit(sig);
        let delivery = delivery(sig, self.kept(sig)).ok().flatten()?;
        self.reset(sig);
        Some(delivery)
    }

    /// A handler kept with `SA_RESETHAND` is the default's once it is taken.
    fn reset(&mut self, sig: i32) {
        if let Kept::Handle(action) = self.kept(sig) {
            if action.flags & SA_RESETHAND != 0 {
                self.actions[sig as usize - 1] = SigAction::of(SIG_DFL);
            }
        }
    }

    /// `tid` enters a blocking call blocking `mask` while there, waiting for
    /// children where `children`.
    pub(crate) fn enter(&mut self, tid: u32, mask: u64, children: bool) {
        let children = children.then_some(self.children);
        self.waiters.push(Waiter { tid, mask, children });
    }

    /// `tid` leaves its blocking call.
    pub(crate) fn leave(&mut self, tid: u32) {
        let at = self.waiters.iter().position(|w| w.tid == tid).expect("leave: a thread left a call it never entered");
        self.waiters.swap_remove(at);
    }

    /// A child was started or has ended: every wait for children looks again.
    pub(crate) fn children_changed(&mut self) {
        self.children = self.children.wrapping_add(1);
    }

    /// `tid`, waiting for children, looks at them now.
    pub(crate) fn children_seen(&mut self, tid: u32) {
        let now = self.children;
        if let Some(waiter) = self.waiters.iter_mut().find(|w| w.tid == tid) {
            waiter.children = waiter.children.map(|_| now);
        }
    }

    /// Whether some thread in a blocking call has something to look at: a
    /// signal it leaves unblocked, or children moved since it last looked.
    /// libc's wake event is pending exactly while this holds.
    pub(crate) fn wake_wanted(&self) -> bool {
        self.waiters
            .iter()
            .any(|w| self.pending & !w.mask != 0 || w.children.is_some_and(|seen| seen != self.children))
    }
}
