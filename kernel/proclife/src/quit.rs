//! A request to quit, and whom it reaches.
//!
//! **A quit reaches a process and every process below it, and kills the ones
//! that do not listen for it.** Each process is started holding its notice, which
//! is readable while a reason is asked and not yet read; a read takes it.
//! [`ask`] puts the reason in a process's notice when the process listens —
//! it has read its notice or armed a watch on it — and its notice holds none,
//! and claims the process for [`End::Killed`] otherwise: a process that never
//! listened, or one that has not yet taken the reason it was last asked. The
//! check and the claim are one act, so no read comes between them. Below a
//! process it kills nothing is asked: its end takes its subtree. A child
//! that lands holding an inherited reason ([`Notice::under`]) is not asked
//! by that quit, so one that never listens is ended only by the next.
//!
//! The kernel keeps each notice behind a lock of its own and passes it here
//! held, under `PROCESS_TABLE`; a read of the notice takes that lock alone.

use alloc::vec::Vec;

use crate::table::{Lifecycle, Processes};
use crate::{tree, End, Pid};

pub use toyos_abi::quit::Reason;

/// One process's notice.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub struct Notice {
    listened: bool,
    asked: Option<Reason>,
}

impl Notice {
    /// A process's notice placed under none, as the supervisor is.
    pub const fn new() -> Self {
        Self { listened: false, asked: None }
    }

    /// The notice a child placed under `parent`'s starts with: never listened
    /// to, and holding the reason `parent`'s holds unread.
    pub fn under(parent: &Notice) -> Self {
        // The mutation this feature stages: the child starts unasked, whatever
        // its parent was asked and has not read.
        if cfg!(feature = "mutate-quit-spawn-starts-unasked") {
            return Self::new();
        }
        Self { listened: false, asked: parent.asked }
    }

    /// A read: the process listens from now on, and takes the reason asked.
    pub fn read(&mut self) -> Option<Reason> {
        self.listened = true;
        self.asked.take()
    }

    /// A watch armed on it: the process listens from now on.
    pub fn listen(&mut self) {
        self.listened = true;
    }

    /// Whether a read would take a reason.
    pub fn readable(&self) -> bool {
        self.asked.is_some()
    }

    pub fn listened(&self) -> bool {
        self.listened
    }

    pub fn asked(&self) -> Option<Reason> {
        self.asked
    }
}

/// What a quit still owes: the processes to ask, and the ones whose end it
/// claimed and whose subtrees its kill walk takes.
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug)]
pub struct Owed {
    pub ask: Vec<Pid>,
    pub kill: Vec<Pid>,
}

impl Owed {
    /// A quit of `pid`, and so of everything below it.
    pub fn of(pid: Pid) -> Self {
        Self { ask: alloc::vec![pid], kill: Vec::new() }
    }
}

/// What [`ask`] did to one process.
#[must_use = "a claimed process's threads are its quit's to retire, and a notified one's watch its to post"]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Asked {
    /// It is gone, or another end has claimed it.
    Gone,
    /// It is claimed for [`End::Killed`].
    Killed,
    /// Its notice holds the reason, and its children are owed.
    Notified,
}

/// What a quit does to a process whose notice is `notice`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    Notify,
    Kill,
}

/// [`ask`]'s decision, before it acts on it.
pub fn decide(notice: &Notice) -> Decision {
    // The mutation this feature stages: a process that never listened is
    // notified like one that did.
    let listened = notice.listened || cfg!(feature = "mutate-quit-notifies-a-deaf-process");
    match listened && notice.asked.is_none() {
        true => Decision::Notify,
        false => Decision::Kill,
    }
}

/// Ask `pid`, whose notice is `notice`, to quit for `reason`, in one hold:
/// claim it, or put the reason in its notice and owe its children to the walk.
pub fn ask<T: Processes>(table: &mut T, pid: Pid, notice: &mut Notice, reason: Reason, owed: &mut Owed) -> Asked {
    match table.get(pid) {
        Some(proc) if !proc.tearing_down() => {}
        _ => return Asked::Gone,
    }
    match decide(notice) {
        Decision::Kill => {
            assert!(tree::claim(table, pid, End::Killed, &mut owed.kill), "ask: pid {pid}, unclaimed in this hold, was claimed by another");
            // The mutation this feature stages: the walk asks the children of
            // a process it kills, whose end is about to take them.
            if cfg!(feature = "mutate-quit-asks-below-a-kill") {
                owed.ask.append(&mut owed.kill);
            }
            Asked::Killed
        }
        Decision::Notify => {
            notice.asked = Some(reason);
            // The mutations these features stage: the walk reads the
            // children a hold later (the model's `Op::Quit` does), or reads
            // none.
            if !cfg!(feature = "mutate-quit-reads-children-late")
                && !cfg!(feature = "mutate-quit-reaches-the-named-process-alone")
            {
                owed_children(table, pid, owed);
            }
            Asked::Notified
        }
    }
}

/// `pid`'s children, read now, owed to the walk; none for a process gone.
pub fn owed_children<T: Processes>(table: &T, pid: Pid, owed: &mut Owed) {
    if let Some(proc) = table.get(pid) {
        owed.ask.extend_from_slice(proc.node().children());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::World;

    fn listening() -> Notice {
        let mut notice = Notice::new();
        notice.listen();
        notice
    }

    #[test]
    fn a_listener_is_asked_and_one_that_never_listened_is_killed() {
        let mut world = World::new();
        let supervisor = world.spawn_process();
        let deaf = world.spawn_child(supervisor);
        let heard = world.spawn_child(supervisor);
        let mut owed = Owed::default();
        assert_eq!(ask(&mut world, deaf, &mut Notice::new(), Reason::Interrupt, &mut owed), Asked::Killed);
        assert_eq!(world.get(deaf).unwrap().teardown_end(), Some(End::Killed));
        let mut notice = listening();
        assert_eq!(ask(&mut world, heard, &mut notice, Reason::Interrupt, &mut owed), Asked::Notified);
        assert_eq!(notice.read(), Some(Reason::Interrupt));
        assert_eq!(notice.read(), None, "a read takes the reason");
    }

    /// A second quit before the first is read kills; once it is read, it asks again.
    #[test]
    fn an_unread_reason_is_the_end_of_a_second_quit() {
        let mut world = World::new();
        let supervisor = world.spawn_process();
        let pid = world.spawn_child(supervisor);
        let mut notice = listening();
        let mut owed = Owed::default();
        assert_eq!(ask(&mut world, pid, &mut notice, Reason::Interrupt, &mut owed), Asked::Notified);
        assert_eq!(notice.read(), Some(Reason::Interrupt));
        assert_eq!(ask(&mut world, pid, &mut notice, Reason::HangUp, &mut owed), Asked::Notified);
        assert_eq!(ask(&mut world, pid, &mut notice, Reason::Terminate, &mut owed), Asked::Killed);
    }

    /// A first read with nothing asked listens, as an empty read and a watch do.
    #[test]
    fn a_read_alone_makes_a_listener() {
        let mut notice = Notice::new();
        assert_eq!(decide(&notice), Decision::Kill);
        assert_eq!(notice.read(), None);
        assert_eq!(decide(&notice), Decision::Notify);
    }

    #[test]
    fn a_child_starts_holding_its_parents_unread_reason_and_never_listened() {
        let mut parent = listening();
        assert_eq!(Notice::under(&parent), Notice::new());
        parent.asked = Some(Reason::Terminate);
        let child = Notice::under(&parent);
        assert_eq!(child.asked(), Some(Reason::Terminate));
        assert!(!child.listened());
    }
}
