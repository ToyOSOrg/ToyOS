---
status: assigned
kind: defect
opened: 2026-09-30
---

# Nothing fails when a device's release or close stops answering its polls

Held by the small-kernel track's stage 6 step 5
(`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`),
whose exit reads it.

A device's watch is an `IrqWatch`, and two thread paths answer the polls on it
as gone.

- **A close or a move of a claim's handle** ends the polls made through that
  handle. `HandleTable::remove` and a committed `transfer` reach the watch
  through `ops::end_polls` and `WatchRef::end_polls`'s `Claim` arm
  (`kernel/src/object/ops.rs`). The arm calls `DeviceClaim::end_polls`, then
  `pcidev::end_polls` or `isa::end_polls`.
- **A release** answers every poll on the watch: `tear_down_departed` for a
  function that left the device set under a live claim
  (`kernel/src/pcidev/mod.rs`), and `tear_down` and an ISA row's drop
  (`kernel/src/isa.rs`) once the last handle is gone. A close has already ended
  every poll made through that handle, so the release that follows it answers
  none still live. `tear_down_departed` is the release that answers live
  polls.

A poll that neither path answers waits on interrupts that belong to the next
holder or to nobody. No host test compiles `pcidev`, `device` or `ops`, and no
guest test ends a polled device, so either call can go and nothing reds.

**Evidence**: the x86-64 kernel clippy shape exited 0 with the release's
`cancel_polls()` deleted. That was measured before the close path moved to
`end_polls`. Making the `Claim` arm `{}` type-checks by reading, since every
arm's value is `()`; it was not measured.

**Exit**: two guest tests, each red with its call removed.
- A claim's holder watches its claim and closes the claim's handle. The test
  reads that poll answered `-NotFound`, and reds with `WatchRef::end_polls`'s
  `Claim` arm made `{}`.
- A claimed function leaves the device set under a watch. The test reads the
  poll answered `-NotFound`, and reds with `tear_down_departed`'s
  `cancel_polls()` deleted.

- **Owed on metal:** a poll belonging to the handle it was made through, and a
  ring to its one submitter (#1261), end to end. These are shared-boot
  members, which no guest row runs.
  - Rows: the `testcases`, `testcases-watchdog` and `shared-debug` boots of
    main, whole.
  - Bench: the T14.
  - Main must show `poll_belongs_to_its_handle`'s four progress lines and
    `poll_belongs_to_its_handle: ok`, `pipe_other_end_gone` passing, and every
    other shared member that polls passing.
- **Owed on metal:** the cost of #1261 on the poll path. It is not measured.
  - Comparison: osbench's `benchcase` boot of main from before #1261 against
    main from after it, two boots a side in the order B, H, H, B, over every
    `wake.*`, `pingpong.pipe.*` and `ipc.*` row.
  - Bench: the T14.
  - Main must show no such row slower. A row is slower only if all its head
    p50s lie above all its base p50s over four boots a side; the first two a
    side are a screen, and a flagged row gets a second round. Each row is
    reported as its after/before median ratio beside both sides' boot-to-boot
    ranges, and a row with no flag as "no difference resolved".
