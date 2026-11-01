//! An event's two sides, as two object types over one [`Event`].
//!
//! [`EventSignal`] has no take and [`EventWait`] no signal, so the two
//! authorities are separate by type. Each side's last handle is that side's
//! end ([`kernel::event`]): the signal side's makes the wait side ready for
//! good, the wait side's makes a signal answer `Gone`.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use kernel::event::{Signaled, State, Taken};

use crate::mm::charge::{self, Charge, Exhausted};
use crate::watch::Watch;

use super::{KObjectVariant, ObjectCore, ZeroHandles};

pub struct Event {
    state: State,
    /// Posted when the event is raised and when its signalers end; the wait
    /// side's readable watch.
    watch: Arc<Watch>,
    /// A real-time signaler's window, owed to whoever takes the signal next:
    /// a waiter already runnable misses the wake-time loan.
    lend_rt: AtomicBool,
    /// Itself and its watch, charged to the job that made it for as long as
    /// either side holds it.
    _charge: Charge,
}

pub struct EventSignal {
    pub(super) core: ObjectCore,
    event: Arc<Event>,
}

pub struct EventWait {
    pub(super) core: ObjectCore,
    event: Arc<Event>,
}

/// A fresh event's two sides.
pub fn create() -> Result<(Arc<EventSignal>, Arc<EventWait>), Exhausted> {
    let charge = Charge::current(charge::arc::<Event>() + charge::arc::<Watch>())?;
    let signal = EventSignal::new_core(0)?;
    let wait = EventWait::new_core(0)?;
    let event = Arc::new(Event {
        state: State::new(),
        watch: Arc::new(Watch::new()),
        lend_rt: AtomicBool::new(false),
        _charge: charge,
    });
    Ok((Arc::new(EventSignal { core: signal, event: event.clone() }), Arc::new(EventWait { core: wait, event })))
}

impl EventSignal {
    /// Raise the event; never blocks and allocates nothing. `false` once no
    /// wait side is left.
    pub fn signal(&self) -> bool {
        let event = &self.event;
        match event.state.signal() {
            Signaled::Raised => {
                crate::scheduler::post_lending(&event.watch, || event.lend_rt.store(true, Ordering::Relaxed));
                true
            }
            Signaled::Coalesced => true,
            Signaled::NoWaiter => false,
        }
    }
}

impl EventWait {
    /// Take the pending signal, granting the window a real-time signaler lent.
    pub fn take(&self) -> Taken {
        let taken = self.event.state.take();
        if taken == Taken::Signal && self.event.lend_rt.swap(false, Ordering::Relaxed) {
            crate::scheduler::boost_current_rt_inherited();
        }
        taken
    }

    pub fn ready(&self) -> bool {
        self.event.state.ready()
    }

    pub fn watch(&self) -> &Arc<Watch> {
        &self.event.watch
    }
}

impl ZeroHandles for EventSignal {
    fn on_zero_handles(&self) {
        self.event.state.end_signalers();
        self.event.watch.post();
    }
}

impl ZeroHandles for EventWait {
    fn on_zero_handles(&self) {
        self.event.state.end_waiters();
    }
}
