---
status: open
kind: defect
opened: 2026-09-25
---

# A log reader that never reads holds its slot until its buffers fill

`logd` lets a network reader go once its sink has taken no byte for `STALLED`
while bytes are owed (`userland/logd/src/serve.rs`). A connection that never
reads is only seen to take no bytes once everything between `logd` and the
peer is full: the pipe to netd (one 2 MiB page), netd's send buffer and the
peer's window. Until the log has grown by that much, such a connection holds
one of `MAX_NETWORK_READERS` slots, so enough idle connections refuse a later
network reader for as long as the log grows slowly. This machine's own readers
are counted apart and are not affected.

## Exit condition

A connection that has not acknowledged what it was sent is let go on a bound
that does not depend on how fast the log grows — netd telling `logd` its peer's
window is closed, or a bound on unacknowledged bytes — and a test that opens
`MAX_NETWORK_READERS` connections that never read, with no flood, finds a later
reader admitted.
