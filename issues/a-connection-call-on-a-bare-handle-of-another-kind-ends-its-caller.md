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

- **Owed on metal:** the receive that reports each handle's kind (#1239),
  end to end. Its guest binaries are shared-boot members, which no guest row
  runs.
  - Rows: the `testcases`, `testcases-watchdog` and `shared-debug` boots of
    main, whole, and the `proctreecase` boot (`process_tree`).
  - Bench: the T14.
  - Main must show `messages_are_whole: each handle arrived with its kind, and
    another kind was refused by name`, then `messages_are_whole: ok`.
    `mutual_kill`, `moved_rights` and `served_file_offset` pass. `process_tree`
    passes, with `a_pipe_is_no_place` refused by the supervisor by name.
