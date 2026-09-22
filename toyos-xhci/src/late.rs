//! When a wait on the event ring gives up.
//!
//! **A deadline is the controller's silence, so past it the ring is still read,
//! and the wait gives up at the first empty read.** A CPU can be held past a
//! deadline with the answer already posted: QEMU answers a doorbell inside the
//! vCPU's write to it, and a Stop Endpoint cancelling a USB disk's in-flight
//! SCSI request there waits out the host's flush first.
//!
//! **Checked before every read, not only an empty one**, and a ring read past
//! the deadline at most once around: a ring that keeps producing events the
//! wait is not waiting for would otherwise make the bound unreachable, and the
//! caller holds the controller lock and a block operation with `IF` clear for
//! the whole of it.

/// Nanoseconds since boot.
pub type Nanos = u64;

/// One wait's deadline, and how many reads it has taken past it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Late {
    deadline: Nanos,
    /// The most reads a wait takes once its deadline has passed: one ring's
    /// worth, so every event already posted when the deadline passed is read.
    ring: usize,
    read_past: usize,
}

impl Late {
    /// A wait that ends at `deadline` on a ring of `ring` TRBs.
    pub const fn new(deadline: Nanos, ring: usize) -> Self {
        Self { deadline, ring, read_past: 0 }
    }

    /// Whether the deadline has passed at `now`.
    pub fn past(&self, now: Nanos) -> bool {
        now >= self.deadline
    }

    /// Whether the wait, about to read the ring at `now`, gives up instead;
    /// counts the read it allows.
    pub fn gives_up(&mut self, now: Nanos) -> bool {
        if !self.past(now) {
            return false;
        }
        self.read_past += 1;
        self.read_past > self.ring
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RING: usize = 256;

    /// What a wait does with a ring that answers `event(n)` on its `n`th read,
    /// the clock standing at `now(n)`: the reads it took, and whether it ended
    /// on the event it waited for.
    fn wait(
        deadline: Nanos,
        now: impl Fn(usize) -> Nanos,
        event: impl Fn(usize) -> Option<bool>,
    ) -> (usize, bool) {
        // Far past anything a wait may take, so a wait that never gives up is
        // a failure here and not a test that never ends.
        const NEVER: usize = 1_000 * RING;
        let mut late = Late::new(deadline, RING);
        for n in 0..NEVER {
            if late.gives_up(now(n)) {
                return (n, false);
            }
            match event(n) {
                Some(true) => return (n + 1, true),
                Some(false) => continue,
                None if late.past(now(n)) => return (n + 1, false),
                None => continue,
            }
        }
        panic!("the wait read {NEVER} events and never gave up")
    }

    /// The bound this exists to keep: a ring that never stops producing events
    /// nobody is waiting for is still given up on, one ring's worth of reads
    /// past the deadline.
    #[test]
    fn a_ring_that_keeps_producing_is_given_up_on_one_ring_past_the_deadline() {
        let (reads, answered) = wait(100, |n| 100 + n as Nanos, |_| Some(false));
        assert!(!answered);
        assert_eq!(reads, RING, "one ring's worth of reads past the deadline, and no more");
    }

    /// The reason the ring is read past the deadline at all: an answer posted
    /// while the CPU was held is taken, not called silence.
    #[test]
    fn an_answer_posted_before_the_deadline_passed_is_taken_after_it() {
        // Held past the deadline with 40 unrelated events and then the answer
        // already on the ring.
        let (reads, answered) = wait(100, |_| 5_000, |n| Some(n == 40));
        assert!(answered);
        assert_eq!(reads, 41);
    }

    /// Past the deadline an empty ring is silence at once.
    #[test]
    fn past_the_deadline_the_first_empty_read_ends_the_wait() {
        let (reads, answered) = wait(100, |_| 100, |n| if n < 3 { Some(false) } else { None });
        assert!(!answered);
        assert_eq!(reads, 4);
    }

    /// Before the deadline nothing is given up, however busy the ring is.
    #[test]
    fn before_the_deadline_nothing_is_given_up() {
        let mut late = Late::new(1_000, RING);
        for now in 0..1_000 {
            assert!(!late.gives_up(now), "{now}");
        }
        assert!(late.past(1_000));
    }
}
