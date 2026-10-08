---
status: open
kind: finding
opened: 2026-10-08
---

# logkeeper hears a reader leave only at its next write

A log reader's thread (`userland/logkeeper/src/serve.rs`, `feed`) waits on
`shared.grew` once it has caught up, and learns its reader is gone only when a
write to the sink fails. A reader that leaves while the log is quiet keeps its
thread and its place in `MAX_NETWORK_READERS` or `MAX_LOCAL_READERS` until the
next record lands.

Read from the code, not measured.

**Exit condition**: a reader's end is what wakes its thread, or the header of
`serve.rs` says why the next record is soon enough.

**Owner**: whoever holds `issues/redesign-the-log-subsystem.md`.
