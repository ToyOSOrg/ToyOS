---
status: open
kind: defect
opened: 2026-10-22
---

# libc's sigaction answers success and keeps nothing

`signal` and `sigaction` (`sdk/libc/src/misc.rs`) answer success for every
signal but `SIGALRM` and keep nothing: no handler is installed, none runs, and
`oldact` is left as the caller passed it. A program that installs a `SIGINT`,
`SIGTERM` or `SIGCHLD` handler is told it did, and is never called. Which
signals libc delivers, and the ruling that an unsupported one never appears
to succeed, are the process track's
(`issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`).

**Exit**: `signal` and `sigaction` refuse a handler for a signal libc does not
deliver with `EINVAL`, and keep and answer what they do support, with a C
corpus case and every in-tree caller checked for the false success.
