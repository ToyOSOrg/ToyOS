---
status: open
kind: defect
opened: 2026-09-27
---

# A refused handle move leaves the compositor holding what it meant to send

`deliver_with_handles` (`userland/compositor/src/client.rs`) sends through
`Connection::try_send_with_handles`, which calls `syscall::handle_send` and
then the frame. When `handle_send` itself is refused, the kernel restores every
handle at its own number (`sys_handle_send` in `kernel/src/syscall/ipc.rs`).
The compositor drops the client, but it still holds the handle and never
closes it. The function's doc says the handles are moved whether or not the
frame lands, and that is false for this refusal.

Each refusal keeps one handle slot and one region. Three sites are affected:

- `create_window`'s `MSG_WINDOW_CREATED`: a client that closes before the
  answer arrives.
- `paste`'s `MSG_CLIPBOARD_PASTE_SHM`.
- `rebuffer`'s `MSG_WINDOW_RESIZED`, reached by any app through
  `MSG_SET_RESOLUTION`, which reallocates every window's buffer. A window that
  never takes its handles fills its `MAX_QUEUED_BATCHES` queue, and the next
  move is refused.

A client can repeat this, and nothing bounds it before the handle table or
memory runs out.

Owner: the compositor's client delivery, `deliver_with_handles` in
`userland/compositor/src/client.rs`.

**Exit**: `deliver_with_handles` calls `syscall::handle_send` on its own and
closes the handles if that is refused, then sends the frame. It never closes a
handle after a successful move. `copy_begin` in
`userland/compositor/src/session.rs` already has this shape.
