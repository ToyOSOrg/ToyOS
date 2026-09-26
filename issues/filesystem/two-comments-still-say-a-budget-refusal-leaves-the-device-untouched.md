---
status: open
kind: finding
opened: 2026-09-26
---

# Two comments still say a budget refusal leaves the device untouched

A write refused on `block::OPERATION` may already be on the medium
(`IoError::BudgetExpired`'s doc, `toyos-fat32/src/repair.rs`). Two comments
outside `toyos-fat32` still state the opposite:

- `kernel/src/fat32_adapter.rs`, in `update_metadata` above the
  `fat-flush-meta-refuse` actuator: "Refused before the entry is written, like a
  spent `block::OPERATION`".
- `userland/logd/src/policy.rs`'s module doc: a budget that expired means
  "nothing was issued, the device is untouched".

Neither changes what the code does; each tells the next reader that a refusal
is a no-op. Found while answering the first review of the branch that made
`toyos-fat32` treat every refused write as of unknown outcome, whose brief named
only the adapter's other two such comments.

## Exit condition

Both sentences deleted, or rewritten to say the refused write's outcome is
unknown.
