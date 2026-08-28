//! The user-copy window's pin invariant, checked over every park interleaving.
//!
//! The break this models: a thread parked mid-syscall with a `UserBytes`
//! window open, a sibling's `munmap` freeing the buffer, the allocator
//! reissuing the frame, and the parked thread's wake-up copy landing in it —
//! one process reading a pipe corrupting another's memory. An abstract state
//! machine over one frame and two agents enumerates every interleaving and
//! checks one law at the copy — the frame is never owned by an allocation made
//! after the window opened. [`Policy::RespectsPins`] upholds it everywhere;
//! [`Policy::IgnoresPins`] reaches the break, so the check has teeth. It names
//! nothing the kernel defines: `pmm.rs` is tied to the same law by the guest
//! test `munmap_reissues_read_window`.

#![forbid(unsafe_code)]

/// Which allocation a physical frame currently backs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Owner {
    /// The victim's original mapping — the buffer its blocking read validated.
    Victim,
    /// Unowned: returned to the allocator and available to be handed out.
    Free,
    /// A different allocation the sibling made after the victim parked.
    Sibling,
}

/// Whether the allocator refuses to hand out a frame a live window pins.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    /// The fix: a pinned frame is never reissued.
    RespectsPins,
    /// The defect the kernel had: pins do not exist, so a freed frame is reissued at once.
    IgnoresPins,
}

/// The one frame the scenario turns on.
#[derive(Clone, Copy)]
struct Frame {
    owner: Owner,
    pinned: bool,
}

/// The victim syscall's steps, in order; between `Open` and `Copy` the thread
/// is parked, which is when the sibling's steps may run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Victim {
    Open,
    Copy,
    Close,
    Done,
}

/// The sibling syscall's steps, in order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sibling {
    Munmap,
    Alloc,
    Done,
}

#[derive(Clone, Copy)]
struct State {
    frame: Frame,
    victim: Victim,
    sibling: Sibling,
}

/// Advance the victim one step, returning its label and whether its copy landed
/// on a reissued frame.
fn step_victim(s: &mut State) -> (&'static str, bool) {
    match s.victim {
        Victim::Open => {
            // A window builds only over a still-mapped buffer; a buffer already
            // unmapped is the kernel's `BadAddress`, and no window (or copy) follows.
            if s.frame.owner == Owner::Victim {
                s.frame.pinned = true;
                s.victim = Victim::Copy;
                ("victim: open+pin", false)
            } else {
                s.victim = Victim::Done;
                ("victim: refused (buffer gone)", false)
            }
        }
        Victim::Copy => {
            let reissued = s.frame.owner == Owner::Sibling;
            s.victim = Victim::Close;
            ("victim: copy", reissued)
        }
        Victim::Close => {
            s.frame.pinned = false;
            s.victim = Victim::Done;
            ("victim: close+unpin", false)
        }
        Victim::Done => ("victim: done", false),
    }
}

/// Advance the sibling one step, returning its label.
fn step_sibling(s: &mut State, policy: Policy) -> &'static str {
    match s.sibling {
        Sibling::Munmap => {
            if s.frame.owner == Owner::Victim {
                s.frame.owner = Owner::Free;
            }
            s.sibling = Sibling::Alloc;
            "sibling: munmap (free)"
        }
        Sibling::Alloc => {
            let blocked = policy == Policy::RespectsPins && s.frame.pinned;
            if s.frame.owner == Owner::Free && !blocked {
                s.frame.owner = Owner::Sibling;
            }
            // Otherwise the sibling's allocation lands on some other frame.
            s.sibling = Sibling::Done;
            "sibling: alloc"
        }
        Sibling::Done => "sibling: done",
    }
}

/// The outcome of exploring one policy: how many complete interleavings were
/// walked, and the step trace of the first that broke the law.
pub struct Report {
    pub runs: usize,
    pub violation: Option<Vec<&'static str>>,
}

/// Explore every interleaving of the two agents under `policy`.
pub fn check(policy: Policy) -> Report {
    let start = State {
        frame: Frame { owner: Owner::Victim, pinned: false },
        victim: Victim::Open,
        sibling: Sibling::Munmap,
    };
    let mut trace = Vec::new();
    let mut runs = 0;
    let violation = search(start, policy, &mut trace, &mut runs);
    Report { runs, violation }
}

fn search(
    s: State,
    policy: Policy,
    trace: &mut Vec<&'static str>,
    runs: &mut usize,
) -> Option<Vec<&'static str>> {
    if s.victim == Victim::Done && s.sibling == Sibling::Done {
        *runs += 1;
        return None;
    }
    if s.victim != Victim::Done {
        let mut ns = s;
        let (label, violated) = step_victim(&mut ns);
        trace.push(label);
        if violated {
            return Some(trace.clone());
        }
        if let Some(t) = search(ns, policy, trace, runs) {
            return Some(t);
        }
        trace.pop();
    }
    if s.sibling != Sibling::Done {
        let mut ns = s;
        let label = step_sibling(&mut ns, policy);
        trace.push(label);
        if let Some(t) = search(ns, policy, trace, runs) {
            return Some(t);
        }
        trace.pop();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pinning_allocator_never_reissues_a_frame_under_a_live_copy() {
        let report = check(Policy::RespectsPins);
        assert!(report.runs > 0, "the interleaving walk explored nothing");
        assert!(
            report.violation.is_none(),
            "a victim's copy reached a reissued frame under the pinning allocator: {:?}",
            report.violation,
        );
    }

    #[test]
    fn an_allocator_that_ignores_pins_reissues_under_the_copy() {
        // The teeth: the modelled break is reachable exactly when the allocator
        // does not honour pins, so the check above is a fact and not a vacuous
        // walk over a scenario nothing could ever fail.
        let report = check(Policy::IgnoresPins);
        assert!(
            report.violation.is_some(),
            "the model never reached the defect it exists to catch over {} interleavings",
            report.runs,
        );
    }
}
