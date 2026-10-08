//! What the server refuses, each said the first time it is seen and counted
//! after: a machine whose firmware asks the same refused thing a thousand
//! times says so in one line and a number.

use std::collections::BTreeMap;

/// Every distinct thing seen, and how often.
#[derive(Default)]
pub struct Ledger(BTreeMap<String, u64>);

impl Ledger {
    /// Count `what`; `true` the first time it is seen, which is when its
    /// caller says it.
    pub fn see(&mut self, what: &str) -> bool {
        let count = self.0.entry(what.to_owned()).or_insert(0);
        *count += 1;
        *count == 1
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Each thing seen with its count, in the order of their text.
    pub fn counts(&self) -> String {
        self.0.iter().map(|(what, count)| format!("{what} x{count}")).collect::<Vec<_>>().join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thing_is_new_once_and_counted_every_time() {
        let mut ledger = Ledger::default();
        assert!(ledger.is_empty());
        assert!(ledger.see("a write to SystemIO"));
        assert!(!ledger.see("a write to SystemIO"));
        assert!(ledger.see("a read of RAM"), "another thing is new whatever was seen before it");
        assert!(!ledger.see("a write to SystemIO"));
        assert!(!ledger.is_empty());
        assert_eq!(ledger.counts(), "a read of RAM x1; a write to SystemIO x3");
    }
}
