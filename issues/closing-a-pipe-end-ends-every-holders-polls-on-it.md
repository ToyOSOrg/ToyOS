---
status: open
kind: defect
opened: 2026-10-11
---

# Closing a pipe end ends every holder's polls on it

`ops::close` (`kernel/src/object/ops.rs`) cancels every poll on the watches of
the object the closed handle names: a pipe end's data watches and its leaving
watch, and those of a connection, an acceptor, a file and the other objects
`close_ends_polls` lists. `Watch::cancel_rings` (`kernel/pure/sched/watch.rs`)
takes every ring registered on the watch, whoever registered it. So when two
handles name one pipe end — a duplicate, or a handle moved to another process
that kept one — closing either answers the other's poll on its live handle
with `NotFound`, as though that handle had been closed. Its holder reads a
watch it still needs as over.

Found by reading, in review of pull request #861, which put the pipe end's
leaving watch in that same loop. No test or machine has shown it.

An event's wait side has the same defect since #1108: `close_ends_polls` is
`true` for `EventWait`, and its one watch is every holder's, so closing a
duplicate of a wait side answers another holder's poll on it as gone. A
listener's descriptor is such a duplicate (found in review of #1108).

**Exit**: a close ends only the polls made through the handle it closes, and a
guest test that holds a duplicate of a pipe end and of an event's wait side,
watches each, and closes the other handle reads no answer on either.
