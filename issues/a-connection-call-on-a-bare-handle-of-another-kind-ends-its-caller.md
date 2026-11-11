---
status: open
kind: finding
opened: 2026-11-11
---

# A connection call on a bare handle of another kind ends its caller

The SDK's connection calls take any `AsHandle`: `FrameRx::pump` and `recv`,
and `ipc::send`, `signal` and their `try_` forms (`sdk/toyos/src/ipc.rs`). The
message syscalls resolve that handle with `HandleTable::get`, which ends the
caller for a handle of another kind. So a received handle that a process keeps
bare (`OwnedHandle::from(received)`) and then uses as a connection can end it,
which `ReceivedHandle::typed` exists to prevent. No receive site does this
today: every connection a peer sends is made a `Connection` through `typed`
before any connection call. Found in review of #1239, by reading.

**Exit**: the SDK's connection calls take a `Connection` rather than any
`AsHandle`, so a bare handle cannot reach a message syscall.
