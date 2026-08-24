//! Every decision a process or a thread's lifecycle makes.
//!
//! The states are two and the questions about them are the whole of this
//! crate: a thread is [`Scheduled`] or it is a [`Zombie`] with a code, and a
//! process is either being torn down by somebody or it is not. What is hard
//! about the subject is not either state — it is that **two CPUs are inside one
//! process's lifecycle at once**, and the defects it has are the interleavings
//! rather than the arithmetic. A spawn builds a thread's TLS block, its kernel
//! stack and its scheduler record between two acquisitions of the process
//! table lock, and a kill on another CPU claims the process in that window; a
//! thread's exit posts a completion whose subject decides whether a joiner ever
//! runs again; an idle pass takes an entry whose threads another CPU may still
//! be retiring.
//!
//! So the decisions live here and the effects stay in `kernel/src/process.rs`.
//! Nothing in this crate locks, retires a task, frees a page, reads a clock or
//! allocates a stack: the kernel gathers what a decision needs under
//! `PROCESS_TABLE`, calls one function, and performs the value it is handed.
//! Both halves are then reachable from a host test — [`interleave`] enumerates
//! every ordering of a scripted pair of operations and checks the laws at every
//! state, which before this crate existed took a booted guest and a race that
//! had to land the wrong way.
//!
//! ## What is *not* here
//!
//! The scheduler's own states. A live thread is running, ready or blocked and
//! `toyos-sched` is authoritative about which; this crate knows only that it is
//! alive. The same split the kernel already had between [`ThreadLocation`] and
//! `scheduler::task_sched_state`.
//!
//! The exit *code* of a process, which is published on its `ProcessObject` and
//! not on any entry — [`Processes::published_exit`] is how a decision here asks
//! about it, and the answer comes from outside the table.
//!
//! ## The one shape every decision has
//!
//! A decision takes the lifecycle view, answers a value, and mutates only the
//! two words it is about. It never performs the consequence: a
//! [`teardown::ThreadExit::Sibling`] carries the [`Watch`] to post on and posts
//! nothing, because the post must happen with the table lock given up and this
//! crate cannot know that the caller is holding it.
//!
//! [`Scheduled`]: ThreadLocation::Scheduled
//! [`Zombie`]: ThreadLocation::Zombie

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod join;
pub mod poison;
pub mod reap;
pub mod spawn;
pub mod table;
pub mod teardown;

#[cfg(test)]
mod interleave;
#[cfg(test)]
mod model;

pub use table::{Lifecycle, Processes};

pub use toyos_abi::{Pid, Tid};

/// Where a *thread* is in its lifecycle.
///
/// **A process has no such state.** Its exit code lives on its
/// `ProcessObject`, published once and readable for ever after, so the table
/// never holds a corpse waiting for somebody entitled to claim it. A thread
/// still has one, because `SYS_THREAD_JOIN` reads it out of the table and a
/// `Tid` names nothing outside its own process.
///
/// For a live thread the scheduler is authoritative about running, ready or
/// blocked — `scheduler::task_sched_state()` has that detail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadLocation {
    /// Alive: running, ready, or blocked. The scheduler owns the detail.
    Scheduled,
    /// Exited with the given code, waiting for its joiner.
    Zombie(i32),
}

impl ThreadLocation {
    /// The code a dead thread carries, or `None` while it is alive.
    pub fn zombie_code(self) -> Option<i32> {
        match self {
            Self::Scheduled => None,
            Self::Zombie(code) => Some(code),
        }
    }

    pub fn is_zombie(self) -> bool {
        matches!(self, Self::Zombie(_))
    }
}

/// What a waiter arms on, named rather than pointed at.
///
/// The kernel resolves each to the real thing a completion is posted against —
/// a thread's `ThreadSched::handle.watch()`, a process's
/// `ProcessObject::watch()` — and this crate decides only *which*. That is the
/// whole of the distinction a real defect turned on: `process::thread_exit`
/// posted one wake and it was always the process's main thread, so a non-main
/// thread joining a sibling was owed a wake nobody sent.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Watch {
    /// One thread's exit. `SYS_THREAD_JOIN` arms here.
    Thread(Pid, Tid),
    /// A process's exit. `SYS_PROCESS_WAIT` arms here.
    Process(Pid),
}

impl Watch {
    /// The thread this names, or `None` for a process's own watch.
    ///
    /// Total rather than a match at the call site: the kernel resolves a
    /// [`Watch::Thread`] through `process::thread_sched` and a
    /// [`Watch::Process`] through the object, and a caller that can only
    /// perform one of the two says so here instead of writing an arm it
    /// believes is unreachable.
    pub fn thread(self) -> Option<(Pid, Tid)> {
        match self {
            Self::Thread(pid, tid) => Some((pid, tid)),
            Self::Process(_) => None,
        }
    }
}

/// The code every thread but the main one is marked dead with when a process is
/// torn down out from under it.
///
/// It is not an exit code anybody chose: the thread did not run to a return, and
/// the only reader is a `SYS_THREAD_JOIN` that arrived after its target's
/// process had already gone.
pub const TORN_DOWN_THREAD_CODE: i32 = -1;
