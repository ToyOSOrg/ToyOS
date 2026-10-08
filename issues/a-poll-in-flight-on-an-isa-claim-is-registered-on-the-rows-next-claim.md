---
status: open
kind: defect
opened: 2026-10-08
---

# A poll in flight on an ISA or ACPI claim is registered on the row's next claim

A poll submission resolves its handle and clones the object out of the table
(`resolve`, `kernel/src/inbox/mod.rs`), lets the process-data lock go, and then
`arm` asks the object whether it is ready and registers on its watch. For an
ISA or ACPI claim both answers are the row's, not the claim's: `ops::has_data`
reads `isa::has_irq(row)` and `ops::read_watch` hands out `isa::watch(row)`
(`kernel/src/object/ops.rs`), and neither asks whose the row is by then.

The row is the next claim's by then on one path. A process reads the claim,
which binds the row's ports to it, moves the handle on to a second process and
ends: `isa::process_ends` takes the binding back while the claim stays minted
in the second process's table. A thread there submits a poll and is past
`resolve`; a sibling closes the handle, the zero-handle hook runs
`isa::release`, and `isa::claim_row` now finds the row neither minted nor bound
and mints the next claim. The first thread's `arm` then fires at once if the
next holder's function has an interrupt unread, or leaves its poll on the
row's watch, where the next holder's first interrupt completes it. A process
that holds no claim learns when another's device interrupted, once per race
won; on the i8042's row that is the time of a key transition.

**The calls on the claim do not have this window.** A read and a write
(`read_device`, `write_device`) run whole under the process-data lock of the
table that holds the claim's one handle (`sys_read`, `sys_read_nonblock`,
`with_object_ref`), and a claim carries no `Rights::DUP`, so no close can
release the claim inside one; after the close the handle refuses the call.
Where the reader is the process that bound the row, the poll is safe too:
the binding ends only in `process_ends`, after the last thread has left, and
a thread inside a submission has not left, so `claim_row` answers `Owned`.

**Read from the code, not run.** The interval is between two lock holds of
one submission and needs a close, the hook and a second claim inside it; no
actuator in this tree holds a thread there.

**Exit condition**: a poll on an ISA or ACPI claim is answered and registered
for that claim or refused — and a test that holds a submission between
`resolve` and `arm`, across the claim's release and the row's next claim, sees
it answered as gone, with no completion at the next holder's interrupt.

**Owner**: whoever holds `issues/every-driver-is-still-in-the-kernel.md`; the
PCI half of the shape is
`issues/a-pci-call-in-flight-names-its-function-by-a-slot-the-next-claim-reuses.md`.
