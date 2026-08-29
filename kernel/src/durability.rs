//! Durability debt, counted in write generations — one rule at three sites
//! (a page's dirty state, a file's flush, a mount's device commit): debt is
//! discharged only by presenting a [`Settlement`] minted before the work that
//! discharged it, so "success recorded before the device committed" and
//! "a clear that erases a later write" are both unwritable, not just absent.
//! `kernel-loom/tests/durability.rs` drives this file over every interleaving.

/// What a durability site owes: writes recorded against writes settled.
pub struct Owed {
    written: u64,
    settled: u64,
}

/// The only value [`Owed::settle`] accepts; minted by [`Owed::snapshot`], so a
/// discharge is always bounded by a state observed before the work began.
#[derive(Clone, Copy)]
pub struct Settlement(u64);

impl Owed {
    pub const fn new() -> Self {
        Self { written: 0, settled: 0 }
    }

    /// Record one write; the debt stands until a settlement minted at or after
    /// this call is presented.
    pub fn record_write(&mut self) {
        self.written += 1;
    }

    pub fn is_owed(&self) -> bool {
        self.written != self.settled
    }

    #[must_use = "an unpresented settlement discharges nothing"]
    pub fn snapshot(&self) -> Settlement {
        Settlement(self.written)
    }

    /// Discharge every write the settlement covers; a write recorded after its
    /// mint stays owed. `settled` never passes `written`: the settlement was
    /// minted from a `written` that only grows.
    #[cfg(not(feature = "durability-settle-blind"))]
    pub fn settle(&mut self, upto: Settlement) {
        self.settled = self.settled.max(upto.0);
    }

    /// The pre-generation kernel's shape — a blind clear — compiled only by
    /// `kernel-loom`'s negative control, where `tests/durability.rs` must red.
    #[cfg(feature = "durability-settle-blind")]
    pub fn settle(&mut self, _upto: Settlement) {
        self.settled = self.written;
    }
}

impl Default for Owed {
    fn default() -> Self {
        Self::new()
    }
}
