---
status: assigned
kind: defect
opened: 2026-09-30
---

# Nothing fails when a device's release or close stops answering its polls

Held by the small-kernel track's stage 6 step 5
(`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`),
whose exit reads it.

A device's watch is an `IrqWatch`, and three thread sites answer the polls on
it as gone: `pcidev::tear_down` when a claimed function is released
(`kernel/src/pcidev/mod.rs:1489`), `Claim::drop` when an audio claim goes
(`kernel/src/device.rs:86`), and the close of a claim or an audio device
through `WatchRef::cancel_polls`'s `Irq` arm (`kernel/src/object/ops.rs:263`).
A poll none of them answers waits on interrupts that are the next holder's or
nobody's. No host test compiles `pcidev`, `device` or `ops`, and no guest test
ends a polled device, so each call can go and nothing reds.

**Evidence**: each deletion builds. The x86-64 kernel clippy shape exits 0
with the release's `cancel_polls()` deleted, 0 with the `Irq` arm made `{}`,
and 0 with the audio claim's cancel made `{}`.

**Exit**: `WatchRef`'s cancel is one dispatch over every variant, as main's
`Deref` was, so the `Irq` arm above cannot be written apart from the others;
and a guest test, in which a claim's holder exits with a poll of its claim on a
ring another process holds, reads that poll answered `-NotFound` for a claimed
function and for an audio device, and reds with the release's cancel deleted
and with the audio claim's.
