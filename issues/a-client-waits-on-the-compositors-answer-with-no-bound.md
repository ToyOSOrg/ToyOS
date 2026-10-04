---
status: open
kind: defect
opened: 2026-09-28
---

# A client waits on the compositor's answer with no bound

`window::clipboard_set` (`userland/toyos-window/src/lib.rs`) blocks in
`recv_header` for `MSG_COPY_REGION` on a copy past `MAX_INLINE_PAYLOAD`, and
`Window::create` blocks the same way for `MSG_WINDOW_CREATED`. Neither wait has
a deadline, so a compositor that is alive and not answering wedges every
terminal or editor that copies, and every program that asks for a window.

Owner: `toyos-window`.

**Exit**: each wait has a bound, and a missed one is a `CreateError` of its own.
