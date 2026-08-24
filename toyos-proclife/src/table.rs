//! The lifecycle face of the process table, and of one entry in it.
//!
//! Two traits, and they are deliberately narrow: everything a lifecycle
//! decision may read or write, and nothing else. The kernel's `ProcessEntry`
//! carries an address space, a handle table, a symbol table, a name and a
//! scheduler record per thread, and not one decision in this crate can name any
//! of them — which is what makes the host model in `model.rs` a `BTreeMap` and
//! not a simulated kernel.
//!
//! `each_thread` and `each_pid` take a `&mut dyn FnMut` rather than answering an
//! iterator, because the kernel's two containers are a `hashbrown::HashMap` and
//! this crate's model is a `BTreeMap`: an associated iterator type would put
//! both spellings in the trait for no decision's benefit. A caller that needs
//! an order sorts what it collected, and the two that do
//! ([`crate::teardown::exit_set`] and [`crate::reap::finished_pids`]) say so.

use crate::{Pid, ThreadLocation, Tid};

/// One process's lifecycle state: the whole of what a spawn, an exit, a kill, a
/// join or a reap reads or writes about it.
pub trait Lifecycle {
    /// The thread the process began with. `Tid(0)` for every process the loader
    /// builds, but nothing here relies on that.
    fn main_tid(&self) -> Tid;

    /// Whether some path has claimed this process's teardown.
    fn tearing_down(&self) -> bool;

    /// Raise the teardown claim. [`crate::teardown::claim_teardown`] is the one
    /// caller — the flag exists to be claimed exactly once, so raising it
    /// anywhere else is what that function is written to prevent.
    fn begin_teardown(&mut self);

    /// Where `tid` is, or `None` for a thread this process does not have.
    fn location(&self, tid: Tid) -> Option<ThreadLocation>;

    /// Move `tid` to `to`. Silent about a thread that is not there: the callers
    /// are teardown paths, and a thread already collected by its joiner is one
    /// this decision has nothing left to say about.
    fn set_location(&mut self, tid: Tid, to: ThreadLocation);

    /// Take `tid` out of the table. Only a join does this — a zombie is
    /// collected by whoever was entitled to its code.
    fn forget_thread(&mut self, tid: Tid);

    /// Every thread of this process, in whatever order the container has.
    fn each_thread(&self, f: &mut dyn FnMut(Tid, ThreadLocation));
}

/// The lifecycle face of the process table.
pub trait Processes {
    type Proc: Lifecycle;

    fn get(&self, pid: Pid) -> Option<&Self::Proc>;
    fn get_mut(&mut self, pid: Pid) -> Option<&mut Self::Proc>;

    /// Whether `pid` has published its exit.
    ///
    /// **Not a field of the entry**, and that is the design rather than an
    /// accident of where it is stored: the exit lives on the `ProcessObject`,
    /// which outlives the entry for as long as anybody holds a handle to it. A
    /// reap asks this and the entry cannot answer.
    fn published_exit(&self, pid: Pid) -> bool;

    /// Every process in the table, in whatever order the container has.
    fn each_pid(&self, f: &mut dyn FnMut(Pid));
}
