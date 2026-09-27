//! Negative controls for the leak-rollback fixes: each reproduces an "acquire
//! before a fallible step" site and asserts the in-tree count returns to baseline. Run behind `leak-rollback-selftest`.

/// Runs every leak-rollback control.
pub fn run() {
    crate::object::device::mint_rollback_selftest();
}
