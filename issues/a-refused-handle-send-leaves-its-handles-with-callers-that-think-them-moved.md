---
status: open
kind: defect
opened: 2026-10-04
---

# A refused handle send leaves its handles with callers that think them moved

When the kernel refuses `SYS_HANDLE_SEND`, it puts every handle back at its own
number (`sys_handle_send` in `kernel/src/syscall/ipc.rs`). `Connection::send_with_handles`
and its siblings in `toyos/src/ipc.rs` return that refusal and a refused frame
as one error, so a caller cannot tell whether the handles are still its own.
These callers keep what was refused and never close it:

- `handshake` in `userland/diskserver/src/session.rs`: the region's dup, on a
  service that has closed.
- `open_stream` in `userland/soundserver/src/client.rs`: the client's region
  and the signal pipe's read end. Its comment says both are moved whether or
  not the send succeeds.
- `NetstackConn::request_with_handles` in `toyos/src/net.rs`: its doc says a
  refused send drops the batch. Every caller that passes handles it gave up
  keeps them.

`toyos::fs::hello`, the compositor's `copy_begin` and the supervisor's
`serve_launch` each call `handle_send` themselves and close on its refusal.
`issues/a-refused-handle-move-leaves-the-compositor-holding-it.md` is the
compositor's `deliver_with_handles` case of the same thing.

Owner: the SDK's connection, `toyos/src/ipc.rs`.

**Exit**: no caller of a handle send can keep a handle the kernel refused to
move. That holds when the send consumes handles it owns and closes them on the
handle send's refusal, and when the three sites above use it. Its close also
closes `issues/a-refused-handle-move-leaves-the-compositor-holding-it.md`: the
fix that meets this exit deletes both files.
