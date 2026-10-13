//! The guest clipboard's sets, as the agent makes them through the
//! compositor's watcher.
//!
//! **One in flight** until the compositor answers it, and only the newest
//! behind it: a host copying at any rate never fills the compositor's pipe.
//!
//! **A guest copy the compositor delivers drops the set behind**: its text was
//! taken before that copy, and sent after the watcher's next ask it would land
//! over the copy.

use alloc::string::String;

#[derive(Default)]
pub struct Sets {
    in_flight: bool,
    queued: Option<String>,
}

impl Sets {
    /// The guest's clipboard is to become `text`. Answers it where it goes now.
    pub fn set(&mut self, text: String) -> Option<String> {
        if self.in_flight {
            self.queued = Some(text);
            return None;
        }
        self.in_flight = true;
        Some(text)
    }

    /// The compositor answered the set in flight. Answers the next to go.
    pub fn answered(&mut self) -> Option<String> {
        self.in_flight = false;
        let next = self.queued.take()?;
        self.in_flight = true;
        Some(next)
    }

    /// The compositor delivered a guest copy.
    pub fn delivered(&mut self) {
        self.queued = None;
    }
}

#[cfg(test)]
mod tests {
    use std::string::ToString;

    use super::*;

    #[test]
    fn a_set_waits_for_the_answer_to_the_one_in_flight() {
        let mut sets = Sets::default();
        assert_eq!(sets.set("first".to_string()), Some("first".to_string()));
        assert_eq!(sets.set("second".to_string()), None);
        assert_eq!(sets.answered(), Some("second".to_string()));
        assert_eq!(sets.answered(), None);
        assert_eq!(sets.set("third".to_string()), Some("third".to_string()));
    }

    #[test]
    fn only_the_newest_set_waits() {
        let mut sets = Sets::default();
        sets.set("first".to_string());
        sets.set("second".to_string());
        sets.set("third".to_string());
        assert_eq!(sets.answered(), Some("third".to_string()));
        assert_eq!(sets.answered(), None);
    }

    #[test]
    fn a_delivered_guest_copy_drops_the_set_behind() {
        let mut sets = Sets::default();
        sets.set("in flight".to_string());
        sets.set("host text".to_string());
        sets.delivered();
        assert_eq!(sets.answered(), None);
    }
}
