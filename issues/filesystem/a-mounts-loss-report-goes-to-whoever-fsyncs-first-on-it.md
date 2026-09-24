---
status: open
kind: defect
opened: 2026-09-24
---

# A mount's loss report goes to whoever fsyncs first on it, not to the writer whose file lost the writes

The block layer answers a flush for the writer that made the writes
(`toyos-blockhold`), and a mount is one writer: its partition's view is one
account for every file on it (`kernel/src/block.rs`, the page cache's flush in
`kernel/src/vfs.rs`). So when a USB disk comes back having lost writes the
kernel wrote for `/log`, the first `fsync` of *any* file on that mount — or
`sync_all` at shutdown — takes the one `Io` the loss is owed, and logd's own
`fsync` of `/log` after it answers `Ok`.

The paths are ambient by the owner's ruling (root `CLAUDE.md`, *Capabilities*),
so a second process that writes under `/log` and fsyncs can take logd's
report. Nothing does today — logd is the only writer there — so nothing fails,
but the claim "a flush answers for its writer's own writes" is true of a
partition claim and of a mount, and not of a file.

**Exit condition.** The capability end-state track
(`issues/kernel/the-capability-end-state-is-twelve-answers.md`) commits the
ambient set, and `/log` either leaves it — logd is its one writer by
construction — or the page cache keeps the account per file, and a test with
two writers on one mount sees each told only of its own file's loss.
