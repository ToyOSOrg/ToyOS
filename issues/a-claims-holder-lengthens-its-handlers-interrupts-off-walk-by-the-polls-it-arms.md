---
status: assigned
kind: defect
severity: [kernel-from-userland]
opened: 2026-09-30
---

# A claim's holder lengthens its handler's interrupts-off walk by the polls it arms

Held by the small-kernel track's stage 6 step 2
(`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`),
whose instrument is the only thing that can read it.

Every poll ring's own watch sits behind an `IrqLock` (`kernel/src/inbox/mod.rs`),
because a device handler's post reaches it through the polls it fires. A
ring has one submitter, so its watch holds at most one thread. But a claim's
holder polling its claim from R rings, P polls each (up to
`MAX_PENDING_WATCHES`, 1024), makes its device's handler fire R × P entries
under the claim's list lock, each posting its ring's watch. Entries a post in
place fired stay in the list until registrations sweep them four at a time.
Nothing caps R: a ring costs its process a charge and no count. The walk has
run in the device's handler since #634.

How long it is has not been read.

**Exit**: the interrupts-off window that step 2's instrument reads on the T14
does not grow with the polls armed on a claim: read at two sizes on one
kernel, each size the median of at least five boots, each boot from the
load's own report, with the tail reported beside it.

- **Owed on metal:** `mask_windows`' load, as #1261 reshaped it.
  - Row: `mask_windows`, the `windowscase` boot of main.
  - Bench: the T14.
  - Main must show `mask_windows` passing and `test_rs_ring_park_herd` passed
    (`256 threads parked in submit, each on its own ring, woken 64 times`),
    which also shows the job admits the 256 rings.
