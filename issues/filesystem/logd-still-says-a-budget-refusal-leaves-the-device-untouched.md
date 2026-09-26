---
status: open
kind: finding
opened: 2026-09-26
---

# logd still says a budget refusal leaves the device untouched

A write refused on `block::OPERATION` may already be on the medium
(`IoError::BudgetExpired`'s doc, `toyos-fat32/src/repair.rs`).
`userland/logd/src/policy.rs` states the opposite twice: its module doc says a
budget that expired means "nothing was issued, the device is untouched", and
the comment on `fate`'s `(Step::Flush, WouldBlock)` arm says the same.

Neither changes what the code does; each tells the next reader that a refusal
is a no-op. Found while answering the first review of the branch that made
`toyos-fat32` treat every refused write as of unknown outcome; the kernel
adapter's comments saying the same were deleted on that branch.

## Exit condition

Both sentences deleted, or rewritten to say the refused write's outcome is
unknown.
