//! soundd's lines, and the one rule about them: the mix thread's never wait.
//!
//! soundd's stdout and stderr are its log ring, so every thread's line is a
//! record written with no syscall and no wait ([`toyos::log`]). The mix
//! thread runs in the RT band and may not even retry, so it claims a lane of
//! the ring before it enters the band ([`claim_lane`]): its lines are a
//! formatting pass on its own stack and one wait-free push, and a full lane
//! is a count its reader says, never a wait. Every other thread's lines go
//! through the ring's shared half.

use std::cell::Cell;

use toyos::log::region::Lane;
use toyos::log::Severity;

thread_local! {
    /// The lane the calling thread claimed, if it is the mix thread.
    static LANE: Cell<Option<Lane>> = const { Cell::new(None) };
}

/// Make the calling thread — the mix thread, before it enters the RT band —
/// the writer of a lane of its own, so no line it says can wait or retry.
pub(crate) fn claim_lane() {
    toyos::log::bind();
    let lane = toyos::log::claim_lane()
        .expect("soundd: its stderr is no log ring with a lane free, so the mix thread could not log without waiting");
    LANE.with(|held| held.set(Some(lane)));
}

/// `say!`'s one step.
pub(crate) fn said(args: std::fmt::Arguments) {
    match LANE.with(Cell::get) {
        Some(lane) => toyos::log::say_lane(&lane, Severity::Info, args),
        None => toyos::log::say(Severity::Info, args),
    }
}
