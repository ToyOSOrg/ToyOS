//! The clipboard's watcher: what it is sent, and which of its sets land.
//!
//! **One notification outstanding.** The watcher asks for the next copy, and
//! a client's copy made while it has asked is sent to it at once; one made
//! before is sent at its next ask, a copy made before any watcher came
//! included. Only the latest unsent copy goes, so a client copying in a loop
//! moves one and never fills the watcher's pipe.
//!
//! **A watcher's set lands only while its ask is outstanding**: the ask and the
//! set travel on one ordered connection, so an outstanding ask means the
//! watcher had been sent every client copy before its set. An inline set is
//! judged at its frame; a set through a region lands only if the ask was
//! outstanding when the region was opened and no copy was sent since. A set
//! that crossed a delivery is refused, and the client's copy stays the
//! clipboard.
//!
//! **A watcher's set is never sent back to it.**

/// Whether a watcher's set lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landing {
    Lands,
    /// It was made before the watcher saw a client copy now in its pipe.
    Stale,
}

/// The watcher's ask, the copy it has not been sent, and the region of a set
/// it is writing (`R`, opaque here) with whether that set can still land.
///
/// It lives as long as the compositor: a copy made while no watcher is there
/// is the next one's.
pub struct Watch<R> {
    asked: bool,
    unsent: bool,
    region: Option<(R, Landing)>,
}

impl<R> Default for Watch<R> {
    fn default() -> Self {
        Self { asked: false, unsent: false, region: None }
    }
}

impl<R> Watch<R> {
    /// The watcher asks for the next copy. Answers whether the clipboard is to
    /// be sent to it now.
    pub fn ask(&mut self) -> bool {
        self.asked = true;
        self.deliver()
    }

    /// A client's copy landed. Answers whether the clipboard is to be sent to
    /// the watcher now.
    pub fn client_set(&mut self) -> bool {
        self.unsent = true;
        self.deliver()
    }

    fn deliver(&mut self) -> bool {
        let now = self.asked && self.unsent;
        if now {
            self.asked = false;
            self.unsent = false;
            if let Some((_, landing)) = &mut self.region {
                *landing = Landing::Stale;
            }
        }
        now
    }

    /// The watcher's set, inline.
    pub fn set(&self) -> Landing {
        match self.asked {
            true => Landing::Lands,
            false => Landing::Stale,
        }
    }

    /// The watcher opens `region` for a set; `Err` gives it back where one is
    /// already open, which is out of protocol.
    pub fn begin(&mut self, region: R) -> Result<(), R> {
        match self.region {
            Some(_) => Err(region),
            None => {
                self.region = Some((region, self.set()));
                Ok(())
            }
        }
    }

    /// The watcher's set in the open region: the region, and whether what it
    /// holds lands. `None` where none is open, which is out of protocol.
    pub fn commit(&mut self) -> Option<(R, Landing)> {
        self.region.take()
    }

    /// The watcher is gone: its ask and its region go with it, and a copy it
    /// was not sent is the next watcher's.
    pub fn leave(&mut self) {
        self.asked = false;
        self.region = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch() -> Watch<()> {
        Watch::default()
    }

    #[test]
    fn a_bridge_set_is_never_sent_back() {
        let mut w = watch();
        assert!(!w.ask());
        assert_eq!(w.set(), Landing::Lands);
        assert!(!w.ask(), "the watcher's own set was sent back to it");
    }

    #[test]
    fn a_client_set_is_sent_once_asked() {
        let mut w = watch();
        assert!(!w.client_set());
        assert!(w.ask());
        assert!(!w.ask(), "one copy was sent twice");
        assert!(w.client_set());
    }

    #[test]
    fn only_the_latest_of_two_client_sets_is_sent() {
        let mut w = watch();
        assert!(!w.client_set());
        assert!(!w.client_set());
        assert!(w.ask());
        assert!(!w.ask());
    }

    #[test]
    fn nothing_is_sent_unasked() {
        let mut w = watch();
        w.ask();
        assert!(w.client_set());
        assert!(!w.client_set(), "a copy was sent to a watcher that had not asked again");
    }

    /// The watcher set its text before it saw a client copy that is now in its
    /// pipe: the client's copy stays.
    #[test]
    fn a_bridge_set_made_before_a_delivery_was_seen_is_refused() {
        let mut w = watch();
        w.ask();
        assert!(w.client_set());
        assert_eq!(w.set(), Landing::Stale);
    }

    #[test]
    fn a_delivery_between_begin_and_commit_refuses_the_commit() {
        let mut w = watch();
        w.ask();
        w.begin(()).expect("no region was open");
        assert!(w.client_set());
        assert_eq!(w.commit(), Some(((), Landing::Stale)));
        w.ask();
        w.begin(()).expect("the commit closed the region");
        assert_eq!(w.commit(), Some(((), Landing::Lands)));
    }

    /// The watcher opened its region after a delivery it had not yet read, and
    /// asked again before the commit: the ask at the commit is the one that
    /// followed the delivery, and the text was chosen before it.
    #[test]
    fn a_region_opened_after_a_delivery_refuses_the_commit_after_the_next_ask() {
        let mut w = watch();
        w.ask();
        assert!(w.client_set());
        w.begin(()).expect("no region was open");
        w.ask();
        assert_eq!(w.commit(), Some(((), Landing::Stale)));
    }

    /// A copy made while no watcher watches, before the first or after one
    /// left, is sent at the next watcher's first ask.
    #[test]
    fn a_copy_made_while_nobody_watched_is_sent_at_the_next_ask() {
        let mut w = watch();
        // A watcher leaves with its ask outstanding, and a client copies.
        w.ask();
        w.leave();
        assert!(!w.client_set(), "a copy was sent to a watcher that had left");
        assert!(w.ask(), "the copy made while nobody watched was not sent");
        // A client copies after a delivery, and the watcher leaves before it
        // asks again.
        assert!(!w.client_set());
        w.leave();
        assert!(w.ask(), "the copy the last watcher was not sent went with it");
    }

    #[test]
    fn a_region_is_opened_once_and_committed_once() {
        let mut w: Watch<u8> = Watch::default();
        assert_eq!(w.begin(1), Ok(()));
        assert_eq!(w.begin(2), Err(2));
        assert_eq!(w.commit().map(|(r, _)| r), Some(1));
        assert_eq!(w.commit(), None);
    }

    #[test]
    fn a_watcher_that_leaves_takes_its_region() {
        let mut w: Watch<u8> = Watch::default();
        w.begin(1).expect("no region was open");
        w.leave();
        assert_eq!(w.commit(), None);
    }
}
