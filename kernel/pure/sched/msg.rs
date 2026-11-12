//! The cross-CPU message set — these four and no others.
//!
//! Two of the four messages are requests about a task the target already
//! owns; one *is* the ownership transfer. That is the property that makes
//! overflow unrepresentable: a queue that cannot drop a message cannot
//! lose a task, because the message is the task.
//!
//! Its own module rather than part of `mailbox.rs`, so that the scheduler core's single
//! `#[allow(unsafe_code)]` island stays exactly the intrusive-list code.

use crate::sched::cpu::CpuHandles;
use crate::sched::hw::{CpuId, Kicker};
use crate::sched::mailbox::{MailboxNode, PreemptGuard, SchedMsg};
use crate::sched::sync::Arc;
use crate::sched::task::{BlockedTask, SchedPayload, TaskShared, TransitTask, WaitClass, WakeCause};
use crate::sched::hw::Nanos;

pub enum Msg<X: SchedPayload> {
    /// The claim that ended a park, handed to the task's home CPU, which
    /// takes the record from its `TaskShared` and places it. Rides on
    /// `TaskShared.wake_node`.
    Wake { shared: Arc<TaskShared<Msg<X>>>, cause: WakeCause },
    /// Ownership transfer: spawn placement, migration, wake-forwarding. Rides
    /// on the node embedded in the transferred task's own record, which is
    /// why a transfer can never be dropped for lack of queue space.
    Adopt { task: TransitTask<X> },
    /// Kill protocol. Rides on `TaskShared.retire_node`.
    Retire { shared: Arc<TaskShared<Msg<X>>> },
}

/// What a parked task leaves in its `TaskShared` for whoever ends the park.
pub struct ParkedRecord<X: SchedPayload> {
    pub(crate) task: BlockedTask<X>,
    pub(crate) class: WaitClass,
    /// The deadline its home CPU holds for it, if any.
    pub(crate) deadline: Option<Nanos>,
}

#[cfg(feature = "protocol-port")]
impl<X: SchedPayload> ParkedRecord<X> {
    pub fn is_rt(&self) -> bool {
        self.task.rt().is_rt()
    }
}

impl<X: SchedPayload> Msg<X> {
    /// Where an `Adopt`'s node lives: inside the task record the message
    /// carries. Handed to `MailboxProducer::post_owned`, whose safety
    /// condition is exactly what `Task`'s `Box` guarantees — the node's
    /// address does not change when the message moves.
    pub fn adopt_node(&self) -> &MailboxNode<Msg<X>> {
        match self {
            Msg::Adopt { task } => task.adopt_node(),
            _ => panic!("adopt_node on a message that does not carry a task"),
        }
    }
}

impl<X: SchedPayload> SchedMsg for Msg<X> {
    type Parked = ParkedRecord<X>;

    fn wake(shared: Arc<TaskShared<Msg<X>>>, cause: WakeCause) -> Self {
        Msg::Wake { shared, cause }
    }

    fn send_wake(
        shared: &Arc<TaskShared<Msg<X>>>,
        home: CpuId,
        cause: WakeCause,
        cpus: &CpuHandles<Msg<X>>,
        kicker: &impl Kicker,
        preempt: &impl PreemptGuard,
    ) {
        cpus.send_claimed_wake(shared, home, cause, kicker, preempt);
    }

    /// `Retire` carries no `notify` handle, deliberately: a notify riding the
    /// message would be a second wake path, where `TaskShared::claim_wake` is
    /// the only one, and would have to outlive the message anyway — a
    /// *running* target consumes the retire and dies at some later safe point.
    /// Joiners wait on the environment's finalize sink instead, which runs
    /// exactly once per task and after the payload is gone.
    fn retire(shared: Arc<TaskShared<Msg<X>>>) -> Self {
        Msg::Retire { shared }
    }
}

impl<X: SchedPayload> core::fmt::Debug for Msg<X> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Msg::Wake { shared, cause } => write!(f, "Wake({:?}, {:?})", shared.key(), cause.reason),
            Msg::Adopt { task } => write!(f, "Adopt({:?})", task.key()),
            Msg::Retire { shared } => write!(f, "Retire({:?})", shared.key()),
        }
    }
}
