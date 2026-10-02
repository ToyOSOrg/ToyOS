---
status: open
kind: defect
opened: 2026-10-02
---

# A watch outlives the close of the Process handle it was made through

`ops::close` ends no poll for a `Process` (`close_ends_polls`): closing one
handle may end no other handle's watch, and a close cannot tell the polls made
through its own handle from another's. So a watch made through a handle that
then closes stays armed on the process's watch and kept by its ring, where it
counts against `MAX_PENDING_WATCHES`, until the process ends. The look then
finds the handle stale and answers `-NotFound` under the watch's token.

Measured at 81e68535c in one QEMU guest, with an arm that is in no commit: a
ring holding one watch on a duplicate of a held child's handle answers nothing
at the submit after the duplicate's close, and answers token 7 with result -1
once the child ends.

By `close_ends_polls`, a console's, the log's and a keyboard claim's polls are
kept the same way.

**Exit**: a handle's close answers the polls its own process made through it,
and no other handle's; a test watches a held child through a duplicate, closes
the duplicate, and reads `-NotFound` before the child ends.
