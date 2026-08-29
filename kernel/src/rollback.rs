//! An RAII commit-guard: `undo` runs on drop unless [`Rollback::commit`] ran,
//! so an early return between acquiring a resource and committing it undoes the
//! acquire — "install, then fail, then leak" is not writable behind one.

#[must_use = "a Rollback runs its undo when dropped; bind it and commit() on the success path"]
pub struct Rollback<F: FnOnce()> {
    undo: Option<F>,
}

impl<F: FnOnce()> Rollback<F> {
    pub fn new(undo: F) -> Self {
        Self { undo: Some(undo) }
    }

    /// The success path: keep the resource, cancel the rollback.
    pub fn commit(mut self) {
        self.undo = None;
    }
}

impl<F: FnOnce()> Drop for Rollback<F> {
    fn drop(&mut self) {
        if let Some(undo) = self.undo.take() {
            undo();
        }
    }
}
