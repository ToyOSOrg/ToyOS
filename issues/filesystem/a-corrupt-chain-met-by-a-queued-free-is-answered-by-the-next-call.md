---
status: open
kind: defect
opened: 2026-09-26
---

# A corrupt chain met by a queued free is answered by the next call

`Fat32::atomic` re-drives an earlier call's queued repair before a mutating
call starts (`settle_first`, `toyos-fat32/src/repair.rs`). A queued
`Repair::Free` walks the rest of a chain whose name is already gone, and a
walk that meets a cyclic or out-of-range link answers `Error::CorruptChain`.
That error is returned by whichever call came next, for a chain it never
named, and the call did not start. The kernel adapter logs it as that call
failing (`refused` in `kernel/src/fat32_adapter.rs`: "`<op>` of `<name>`:
corrupt cluster chain") and its caller reads `SyscallError::Io`.

Two things are lost:

- **Attribution.** The line names a file that is intact and a call that never
  touched the volume. `Error::CorruptChain`'s doc
  (`toyos-fat32/src/error.rs`) states the case as a property of the variant,
  which documents it and does not remove it.
- **The leak.** The free stops at the corrupt link, so the chain past it stays
  allocated with no entry reaching it. Nothing records which clusters those
  are beyond the one log line, which names the wrong file.

## Owner

`toyos-fat32`, which owns `settle_first` and `free_chain`; the adapter's log
line follows from what the crate answers.

## Exit condition

A call refused by an earlier call's repair meeting a corrupt chain answers an
error that says so, carrying the chain's start and the corrupt link, so the
adapter logs it as the repair's rather than the call's. A host test in
`toyos-fat32/tests/refused_writes.rs` queues a free, corrupts its chain on the
medium, and asserts the next call's answer names the repair and not its own
target.
