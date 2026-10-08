//! Who holds a pipe's two ends, and what a watch on one of them is told.
//!
//! A pipe end is held by counted references, one per object that names it: a
//! handle duplicated or moved to another process is the same object or another
//! reference, so an end is gone only when the last of them is. Every answer
//! here is a function of the two counts and of what the ring holds, read under
//! the kernel's pipe lock.

#![forbid(unsafe_code)]

/// One end of a pipe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum End {
    Read,
    Write,
}

impl End {
    pub fn other(self) -> Self {
        match self {
            Self::Read => Self::Write,
            Self::Write => Self::Read,
        }
    }
}

/// What a reference's release left of its pipe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[must_use = "a release that left an end unheld owes the other end a post"]
pub enum Released {
    /// Its end has another holder.
    Held,
    /// It was its end's last holder: the other end's watch is owed a post.
    EndGone,
    /// It was the pipe's last holder.
    PipeGone,
}

/// How many references hold each end of one pipe.
#[derive(Default)]
pub struct Holders {
    readers: u32,
    writers: u32,
}

impl Holders {
    pub const fn new() -> Self {
        Self { readers: 0, writers: 0 }
    }

    fn count(&mut self, end: End) -> &mut u32 {
        match end {
            End::Read => &mut self.readers,
            End::Write => &mut self.writers,
        }
    }

    pub fn hold(&mut self, end: End) {
        let count = self.count(end);
        *count = count.checked_add(1).expect("pipe holder overflow");
    }

    pub fn release(&mut self, end: End) -> Released {
        let count = self.count(end);
        *count = count.checked_sub(1).expect("pipe holder underflow");
        match (self.readers, self.writers) {
            (0, 0) => Released::PipeGone,
            _ if !self.held(end) => Released::EndGone,
            _ => Released::Held,
        }
    }

    pub fn held(&self, end: End) -> bool {
        match end {
            End::Read => self.readers > 0,
            End::Write => self.writers > 0,
        }
    }

    /// A read answers now: with bytes, or with the end of a stream no writer
    /// can add to.
    pub fn readable(&self, available: u32) -> bool {
        available > 0 || !self.held(End::Write)
    }

    /// A write answers now: it takes bytes, or is refused for want of a reader.
    pub fn writable(&self, space: u32) -> bool {
        space > 0 || !self.held(End::Read)
    }

    /// The end opposite `end` has no holder, whatever the ring holds.
    pub fn other_end_gone(&self, end: End) -> bool {
        !self.held(end.other())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pipe as `pipe::create` leaves it: one holder of each end.
    fn made() -> Holders {
        let mut pipe = Holders::new();
        pipe.hold(End::Read);
        pipe.hold(End::Write);
        pipe
    }

    #[test]
    fn an_end_whose_other_end_is_held_is_not_told_it_is_gone() {
        let pipe = made();
        assert!(!pipe.other_end_gone(End::Read));
        assert!(!pipe.other_end_gone(End::Write));
    }

    /// **Whatever the ring holds.** `readable` answers for the bytes and says
    /// nothing of the writer; `writable` for the room and nothing of the reader.
    #[test]
    fn an_end_dropped_with_the_ring_neither_empty_nor_full_is_gone() {
        for (available, space) in [(0, 8), (3, 5), (8, 0)] {
            let mut pipe = made();
            assert_eq!(pipe.release(End::Write), Released::EndGone);
            assert!(pipe.other_end_gone(End::Read), "the writer left with {available} byte(s) in the ring");
            assert!(pipe.readable(available));
            assert!(!pipe.other_end_gone(End::Write), "a reader that is held was told gone");

            let mut pipe = made();
            assert_eq!(pipe.release(End::Read), Released::EndGone);
            assert!(pipe.other_end_gone(End::Write), "the reader left with room for {space} byte(s)");
            assert!(pipe.writable(space));
            assert!(!pipe.other_end_gone(End::Read), "a writer that is held was told gone");
        }
    }

    /// The two older conditions do not stand in for it: each is ready with the
    /// other end held.
    #[test]
    fn readable_and_writable_say_nothing_of_the_other_end() {
        let pipe = made();
        assert!(pipe.readable(1) && !pipe.other_end_gone(End::Read));
        assert!(pipe.writable(1) && !pipe.other_end_gone(End::Write));
        assert!(!pipe.readable(0) && !pipe.writable(0));
    }

    /// A handle duplicated, or moved to another process, is a second
    /// reference: the end is gone with the last of them and not before.
    #[test]
    fn an_end_with_a_second_holder_outlives_the_first() {
        for end in [End::Read, End::Write] {
            let mut pipe = made();
            pipe.hold(end);
            assert_eq!(pipe.release(end), Released::Held);
            assert!(!pipe.other_end_gone(end.other()), "{end:?} was told gone with a holder left");
            assert_eq!(pipe.release(end), Released::EndGone);
            assert!(pipe.other_end_gone(end.other()));
        }
    }

    /// The pipe's last reference owes nobody a post: no end is left to watch.
    #[test]
    fn the_last_holder_of_the_pipe_ends_it() {
        let mut pipe = made();
        assert_eq!(pipe.release(End::Read), Released::EndGone);
        assert_eq!(pipe.release(End::Write), Released::PipeGone);
    }
}
