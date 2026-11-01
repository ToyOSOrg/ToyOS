//! The signals libc imitates, delivered: libc's own thread, which raises them
//! from the events they stand for, the wake event a blocking call watches
//! beside what it waits for, and the handlers' runs. Where each goes is
//! `sigreq`'s.
//!
//! **libc's own thread starts with the first thing it watches**: an alarm, a
//! `SIGCHLD` handler, or the process's notice. It watches every child whose
//! end has not raised `SIGCHLD`, at most [`crate::spawn::MAX_CHILDREN`],
//! beside its control event and the notice, which fills one poller.
//!
//! **libc listens to the notice once a quit's signal is off its default or
//! blocked**, before the call that put it there returns: until then a quit
//! kills the process, which is what the default would do. A reason read is
//! raised as `SIGINT`, `SIGHUP` or `SIGTERM`.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use toyos::poller::{Poller, READABLE};
use toyos::quit::Reason;
use toyos_abi::syscall::{self, SyscallError};

use crate::errcode::ENOSYS;
use crate::pthread::Lock;
use crate::sigreq::{Delivery, Kept, Raised, SigAction, Signals, SIGALRM, SIGCHLD, SIGHUP, SIGINT, SIGTERM};

/// The sides of an event this library made and never closes: `wait` is
/// readable while it is knocked.
#[derive(Clone, Copy)]
struct Event {
    wait: toyos::BorrowedHandle<'static>,
    signal: toyos::BorrowedHandle<'static>,
}

impl Event {
    fn new() -> Self {
        let ends = syscall::event_create().expect("signals: no event for libc's own use");
        // SAFETY: made here and never closed: libc's own for the process's life.
        unsafe { Self { wait: toyos::BorrowedHandle::borrow_raw(ends.wait), signal: toyos::BorrowedHandle::borrow_raw(ends.signal) } }
    }

    fn knock(&self) {
        if let Err(e) = syscall::event_signal(self.signal.raw()) {
            panic!("signals: an event of libc's own refused a signal: {e:?}");
        }
    }

    fn drain(&self) {
        match syscall::event_take(self.wait.raw()) {
            Ok(()) | Err(SyscallError::WouldBlock) => {}
            Err(e) => panic!("signals: an event of libc's own refused a take: {e:?}"),
        }
    }
}

struct State {
    signals: Signals,
    /// The event each interruptible blocking call watches, made by the first.
    wake: Option<Event>,
    /// Whether `wake` is knocked.
    woken: bool,
    /// libc's own thread's control event, once it runs.
    keeper: Option<Event>,
    /// When the alarm is due, in the monotonic clock's nanoseconds.
    alarm: Option<u64>,
    /// Whether this process listens to its notice, which it does for good.
    listening: bool,
    /// Children a wait took before libc's own thread saw them end, each owed
    /// its `SIGCHLD`.
    unseen_ends: u32,
    /// The mask a handler running on libc's own thread set, which routes
    /// nothing and lasts that run.
    keeper_mask: u64,
}

static STATE: Lock<State> = Lock::new(State {
    signals: Signals::new(),
    wake: None,
    woken: false,
    keeper: None,
    alarm: None,
    listening: false,
    unseen_ends: 0,
    keeper_mask: 0,
});

/// libc's own thread's tid, `u32::MAX` before it runs.
static KEEPER: AtomicU32 = AtomicU32::new(u32::MAX);

/// The signals whose handlers run on libc's own thread now, decided under
/// the lock: a mask that newly blocks one waits until its run ends, so a
/// block a program makes excludes the handler it blocks.
static RUNNING: AtomicU64 = AtomicU64::new(0);

/// Moved at every run's end on libc's own thread, which such a mask waits on.
static RAN: AtomicU32 = AtomicU32::new(0);

fn on_keeper(tid: u32) -> bool {
    KEEPER.load(Ordering::Acquire) == tid
}

/// Whether any handler is kept, read without the lock by a blocking call
/// deciding whether it can be interrupted, and written under it.
static HANDLERS: AtomicBool = AtomicBool::new(false);

impl State {
    /// The wake event made to agree with `Signals::wake_wanted`.
    fn settle(&mut self) {
        let Some(wake) = self.wake else { return };
        match (self.signals.wake_wanted(), self.woken) {
            (true, false) => wake.knock(),
            (false, true) => wake.drain(),
            _ => return,
        }
        self.woken = !self.woken;
    }

    /// libc's own thread, started if it does not run, looks at what it
    /// watches again.
    fn kick(&mut self) {
        let keeper = *self.keeper.get_or_insert_with(|| {
            crate::pthread::spawn_own(keep);
            Event::new()
        });
        keeper.knock();
    }

    /// Listen to the notice if the signals now ask it: `ENOSYS` when this
    /// process holds no notice to listen to, so a quit would kill it.
    fn listen(&mut self) -> Result<(), i32> {
        if self.listening || !self.signals.listens() {
            return Ok(());
        }
        let notice = toyos::quit::notice().ok_or(ENOSYS)?;
        notice.listen().map_err(|_| ENOSYS)?;
        self.listening = true;
        self.kick();
        Ok(())
    }
}

fn tid() -> u32 {
    toyos_abi::current_tid().raw()
}

/// `sigaction`'s work: `sig`'s action made `new`, if any, and the old one
/// answered. Refused, the action unchanged, where the process would have to
/// listen to a notice it does not hold.
pub(crate) fn act(sig: i32, new: Option<SigAction>) -> Result<SigAction, i32> {
    let mut state = STATE.lock();
    let old = state.signals.act(sig, new)?;
    if let Err(errno) = state.listen() {
        state.signals.act(sig, Some(old)).expect("an action just kept is kept again");
        return Err(errno);
    }
    HANDLERS.store(state.signals.any_handler(), Ordering::Relaxed);
    let children = (sig == SIGCHLD).then(|| state.signals.kept(SIGCHLD));
    if let Some(Kept::Handle(_)) = children {
        // A child that ended before the handler was kept raises nothing.
        crate::spawn::mark_ended_raised();
        state.kick();
    }
    state.settle();
    drop(state);
    if children == Some(Kept::Ignore) {
        crate::spawn::forget_ended();
    }
    Ok(old)
}

/// Whether `SIGCHLD` is ignored, so an ended child is kept for no wait.
pub(crate) fn children_ignored() -> bool {
    STATE.lock().signals.kept(SIGCHLD) == Kept::Ignore
}

/// A child was started, or a wait took one libc's own thread had not seen
/// end, which is owed its `SIGCHLD`.
pub(crate) fn children_changed(unseen_end: bool) {
    let mut state = STATE.lock();
    state.signals.children_changed();
    if let Kept::Handle(_) = state.signals.kept(SIGCHLD) {
        state.unseen_ends += u32::from(unseen_end);
        state.kick();
    }
    state.settle();
}

/// Arm the alarm for `seconds` from now, none for 0, and answer the seconds
/// the one it replaces had left.
pub(crate) fn alarm(seconds: u32) -> u32 {
    let now = toyos_abi::clock::nanos_since_boot();
    let mut state = STATE.lock();
    let left = crate::alarmreq::left(state.alarm, now);
    state.alarm = crate::alarmreq::due(now, seconds);
    if state.alarm.is_some() || state.keeper.is_some() {
        state.kick();
    }
    left
}

/// What is pending, as `sigpending` answers it.
pub(crate) fn pending() -> u64 {
    STATE.lock().signals.pending()
}

/// The calling thread's mask.
pub(crate) fn mask() -> u64 {
    let state = STATE.lock();
    match on_keeper(tid()) {
        true => state.keeper_mask,
        false => state.signals.mask(tid()),
    }
}

/// `raise(sig)` in the calling thread: `Err` with an `errno` for a signal it
/// cannot raise.
pub(crate) fn raise(sig: i32) -> Result<(), i32> {
    let raised = {
        let mut state = STATE.lock();
        let raised = match on_keeper(tid()) {
            true => {
                let mask = state.keeper_mask;
                crate::sigreq::raised(sig, state.signals.kept(sig), mask)
            }
            false => state.signals.raise(tid(), sig),
        };
        state.settle();
        raised
    };
    match raised {
        Raised::Deliver(delivery) => {
            run(delivery, Some(tid()));
            Ok(())
        }
        Raised::Nothing | Raised::Pending => Ok(()),
        Raised::Refused(errno) => Err(errno),
    }
}

/// The calling thread's mask is now `mask`: once a handler running on
/// libc's own thread for a signal it newly blocks has ended, and with
/// whatever was pending that it leaves unblocked delivered before this
/// returns. Refused `ENOSYS`, the mask unchanged, on a thread neither the
/// first nor libc's, and where the process would have to listen to a notice
/// it does not hold.
pub(crate) fn set_mask(mask: u64) -> Result<(), i32> {
    let tid = tid();
    if on_keeper(tid) {
        // A handler's own: it routes nothing.
        STATE.lock().keeper_mask = mask;
        return Ok(());
    }
    let newly = {
        let mut state = STATE.lock();
        let old = state.signals.mask(tid);
        if !state.signals.set_mask(tid, mask) {
            return Err(ENOSYS);
        }
        if let Err(errno) = state.listen() {
            state.signals.set_mask(tid, old);
            return Err(errno);
        }
        mask & !old
    };
    loop {
        let ran = RAN.load(Ordering::Acquire);
        if RUNNING.load(Ordering::Acquire) & newly == 0 {
            break;
        }
        // SAFETY: `RAN` is a live, aligned u32.
        unsafe { syscall::futex_wait(RAN.as_ptr(), ran, None) };
    }
    deliver_pending(tid);
    Ok(())
}

/// Deliver, on thread `tid`, every signal pending its mask leaves unblocked.
fn deliver_pending(tid: u32) {
    while let Some(delivery) = {
        let mut state = STATE.lock();
        let mask = state.signals.mask(tid);
        let taken = state.signals.take(mask);
        state.settle();
        taken
    } {
        run(delivery, Some(tid));
    }
}

/// A thread libc started runs from now with `mask`, its creator's.
pub(crate) fn thread_started(tid: u32, mask: u64) {
    STATE.lock().signals.thread_started(tid, mask);
}

pub(crate) fn thread_ended(tid: u32) {
    STATE.lock().signals.thread_ended(tid);
}

/// Run what a thread took: the default action's end, or the handler, on
/// thread `tid` with its mask widened for the run, or on libc's own thread
/// for `None`. Answers whether the call it interrupted restarts.
pub(crate) fn run(delivery: Delivery, tid: Option<u32>) -> bool {
    let (sig, handler, siginfo, mask, restart) = match delivery {
        Delivery::End(code) => crate::misc::end(code),
        Delivery::Handle { sig, handler, siginfo, mask, restart } => (sig, handler, siginfo, mask, restart),
    };
    let before = tid.map(|tid| {
        let mut state = STATE.lock();
        let before = state.signals.mask(tid);
        state.signals.set_mask(tid, before | mask);
        before
    });
    if siginfo {
        let mut info = SigInfo::of(sig);
        // SAFETY: an `SA_SIGINFO` handler the program kept, called as
        // `<signal.h>`'s `sa_sigaction` is typed.
        let handler: extern "C" fn(i32, *mut SigInfo, *mut u8) = unsafe { core::mem::transmute(handler) };
        handler(sig, &mut info, core::ptr::null_mut());
    } else {
        // SAFETY: a handler the program kept, called as `<signal.h>`'s
        // `sa_handler` is typed.
        let handler: extern "C" fn(i32) = unsafe { core::mem::transmute(handler) };
        handler(sig);
    }
    if let (Some(tid), Some(before)) = (tid, before) {
        STATE.lock().signals.set_mask(tid, before);
        deliver_pending(tid);
    }
    restart
}

/// `<signal.h>`'s `siginfo_t`: the signal, and nothing a ToyOS event says more.
#[repr(C)]
struct SigInfo {
    signo: i32,
    errno: i32,
    code: i32,
    rest: [u64; 13],
}

impl SigInfo {
    fn of(sig: i32) -> Self {
        Self { signo: sig, errno: 0, code: 0, rest: [0; 13] }
    }
}

/// A thread inside a blocking call a handler can interrupt: it watches
/// [`Call::wake`] beside what it waits for.
pub(crate) struct Call {
    tid: u32,
    mask: u64,
    children: bool,
    wake: toyos::BorrowedHandle<'static>,
    inside: bool,
}

impl Call {
    /// Enter a blocking call that blocks `mask` while inside, the thread's own
    /// for `None`, waiting for children where `children`. `None` when nothing
    /// could interrupt it: no handler is kept, it waits for no child, and it
    /// was not asked to wait `for_signals`.
    pub(crate) fn enter(mask: Option<u64>, children: bool, for_signals: bool) -> Option<Self> {
        if !children && !for_signals && !HANDLERS.load(Ordering::Relaxed) {
            return None;
        }
        let tid = tid();
        let mut state = STATE.lock();
        // libc's own thread takes no signal in a call a handler makes there:
        // what it would take stays pending for its next turn or for a thread
        // whose mask leaves it unblocked.
        let mask = match on_keeper(tid) {
            true => crate::sigreq::KEEPER_MASK,
            false => mask.unwrap_or_else(|| state.signals.mask(tid)),
        };
        let wake = state.wake.get_or_insert_with(Event::new).wait;
        state.signals.enter(tid, mask, children);
        state.settle();
        Some(Self { tid, mask, children, wake, inside: true })
    }

    /// What a blocking call watches, readable when it has something to look at.
    pub(crate) fn wake(&self) -> toyos::BorrowedHandle<'static> {
        self.wake
    }

    /// A signal the call takes, which it leaves the call to run.
    pub(crate) fn take(&mut self) -> Option<Delivery> {
        let mut state = STATE.lock();
        let taken = state.signals.take(self.mask)?;
        state.signals.leave(self.tid);
        self.inside = false;
        state.settle();
        Some(taken)
    }

    /// Run what [`Call::take`] took, answering whether the call restarts, and
    /// enter it again if it does.
    pub(crate) fn interrupted(&mut self, delivery: Delivery) -> bool {
        let restart = run(delivery, Some(self.tid));
        if restart {
            let mut state = STATE.lock();
            state.signals.enter(self.tid, self.mask, self.children);
            self.inside = true;
            state.settle();
        }
        restart
    }

    /// The children are looked at now.
    pub(crate) fn children_seen(&self) {
        let mut state = STATE.lock();
        state.signals.children_seen(self.tid);
        state.settle();
    }
}

impl Drop for Call {
    /// A signal the call would have taken and did not is delivered here,
    /// before the call returns, where the thread's own mask leaves it
    /// unblocked, and else goes to libc's own thread if another thread does.
    fn drop(&mut self) {
        if self.inside {
            let mut state = STATE.lock();
            state.signals.leave(self.tid);
            state.settle();
        }
        if !on_keeper(self.tid) {
            deliver_pending(self.tid);
        }
        let mut state = STATE.lock();
        if state.signals.has_orphan() {
            state.kick();
        }
    }
}

/// Sleep `nanos`, or until a handler interrupts it: `Err` with the
/// nanoseconds left then.
pub(crate) fn sleep(nanos: u64) -> Result<(), u64> {
    let Some(mut call) = Call::enter(None, false, false) else {
        syscall::nanosleep(nanos);
        return Ok(());
    };
    let due = toyos_abi::clock::nanos_since_boot().saturating_add(nanos);
    loop {
        if let Some(delivery) = call.take() {
            call.interrupted(delivery);
            return Err(due.saturating_sub(toyos_abi::clock::nanos_since_boot()));
        }
        let left = due.saturating_sub(toyos_abi::clock::nanos_since_boot());
        if left == 0 {
            return Ok(());
        }
        // Ready or the deadline: the take at the loop's top says which.
        toyos::poller::wait_one(&call.wake(), READABLE, left).expect("signals: libc's own wake event refused a wait");
    }
}

/// The signal a quit's reason is raised as.
fn signal_of(reason: Reason) -> i32 {
    match reason {
        Reason::Interrupt => SIGINT,
        Reason::HangUp => SIGHUP,
        Reason::Terminate => SIGTERM,
    }
}

/// libc's own thread: it raises `SIGCHLD` once per child that ends while a
/// handler is kept for it, `SIGALRM` when the alarm is due, and a quit's
/// signal for each reason it reads, and delivers what no thread in a
/// blocking call takes.
unsafe extern "C" fn keep(_: *mut u8) -> *mut u8 {
    const CONTROL: u64 = u64::MAX;
    const NOTICE: u64 = u64::MAX - 1;
    KEEPER.store(tid(), Ordering::Release);
    let poller = Poller::new(Poller::MAX_HANDLES);
    // A copy of each watched child's handle, kept until it raises or is
    // reaped, so no watch is armed on a number another handle takes.
    let mut children: alloc::vec::Vec<(i32, toyos::OwnedHandle)> = alloc::vec::Vec::new();
    loop {
        let (control, listening, due) = {
            let state = STATE.lock();
            let control = state.keeper.expect("libc's own thread runs only once its event is made").wait;
            let watched = match state.signals.kept(SIGCHLD) {
                Kept::Handle(_) => crate::spawn::unraised(),
                _ => alloc::vec::Vec::new(),
            };
            children.retain(|(pid, _)| watched.contains(pid));
            for pid in watched {
                if !children.iter().any(|(p, _)| *p == pid) {
                    children.extend(crate::spawn::copy_of(pid).map(|copy| (pid, copy)));
                }
            }
            (control, state.listening, state.alarm)
        };
        poller.watch(&control, READABLE, CONTROL);
        if listening {
            let notice = toyos::quit::notice().expect("libc listens only to a notice it holds");
            poller.watch(notice, READABLE, NOTICE);
        }
        for (pid, handle) in &children {
            poller.watch(handle, READABLE, *pid as u64);
        }
        let wait = due.map_or(u64::MAX, |due| due.saturating_sub(toyos_abi::clock::nanos_since_boot()));
        let mut ended = alloc::vec::Vec::new();
        let (mut kicked, mut asked) = (false, false);
        poller.wait(u32::from(wait != 0), wait, |token| match token {
            CONTROL => kicked = true,
            NOTICE => asked = true,
            pid => ended.push(pid as i32),
        });
        if kicked {
            STATE.lock().keeper.expect("made before this thread ran").drain();
        }
        let mut raised = alloc::vec::Vec::new();
        if asked {
            let notice = toyos::quit::notice().expect("libc listens only to a notice it holds");
            while let Ok(Some(reason)) = notice.try_read() {
                raised.push(signal_of(reason));
            }
        }
        let ends = ended.into_iter().filter(|&pid| crate::spawn::raise(pid)).count();
        let mut deliveries = alloc::vec::Vec::new();
        {
            let mut state = STATE.lock();
            if ends > 0 || state.unseen_ends > 0 {
                state.unseen_ends = 0;
                raised.push(SIGCHLD);
            }
            if state.alarm.is_some_and(|due| due <= toyos_abi::clock::nanos_since_boot()) {
                state.alarm = None;
                raised.push(SIGALRM);
            }
            for sig in raised {
                if let Raised::Deliver(delivery) = state.signals.arrive(sig) {
                    deliveries.push(delivery);
                }
            }
            while let Some(orphan) = state.signals.orphan() {
                deliveries.push(orphan);
            }
            let running = deliveries.iter().fold(0, |all, d| match d {
                Delivery::Handle { sig, .. } => all | crate::sigreq::bit(*sig),
                Delivery::End(_) => all,
            });
            RUNNING.store(running, Ordering::Release);
            state.settle();
        }
        for delivery in deliveries {
            let sig = match delivery {
                Delivery::Handle { sig, .. } => Some(sig),
                Delivery::End(_) => None,
            };
            run(delivery, None);
            STATE.lock().keeper_mask = 0;
            if let Some(sig) = sig {
                RUNNING.fetch_and(!crate::sigreq::bit(sig), Ordering::Release);
                RAN.fetch_add(1, Ordering::Release);
                // SAFETY: `RAN` is a live, aligned u32.
                unsafe { syscall::futex_wake(RAN.as_ptr(), u32::MAX) };
            }
        }
    }
}
