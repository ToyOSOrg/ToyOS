---
status: open
kind: defect
opened: 2026-10-10
---

# A second poller on a window's handle stalled the prompt, and why is unknown

Filepicker's prompt (`userland/filepicker/src/consent.rs`, `ask`) waits on
one `Poller` watching both its window's connection and the supervisor's.
While the branch that made it was built, a version that also drained the
window with `Window::poll_event(0)` after each wake, which watches the same
handle through the window's own second `Poller`, stalled the prompt in
`consent_prompt` (`tests/toyos.rs`) after its first keys. Reading one event per wake off the first poller
alone, as `ask` does now, does not stall.

The kernel-side cause was not found. Nothing says the shape is the prompt's
alone: a program that waits on a `Window` beside another handle through its
own poller and drains with `poll_event(0)` watches one handle from two
pollers in the same way. `rg poll_event` finds those callers.

**Evidence**: the stalling variant was not committed and its runs' logs were
not kept; what is recorded is the shape above and that removing the second
poller ended the stall. Related, and not shown to be the cause:
`issues/a-close-of-one-handle-ends-every-rings-poll-on-its-object.md` and
`issues/toyos-poller-is-sync-and-a-watch-moves-the-tail-in-two-steps.md`.

## Owner

`toyos-window`'s `Window::poll_event` and the windows that drain through it,
in the desktop track `issues/toyos-has-a-desktop.md`.

## Exit condition

A guest test in which one thread watches a connection from two pollers, one
waiting and one polling with no wait, and is handed every frame its peer
sends, red on the shape above or green with the cause named and fixed; or
`Window::poll_event` no longer owns a poller of its own.
