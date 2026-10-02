---
status: open
kind: finding
opened: 2026-10-02
---

# A peer that posts as fast as a submitter looks keeps it from parking

A write of no bytes posts a pipe's readers. A submitter waiting on that pipe
looks, finds nothing, arms the poll again and reads its polls before it parks
(`polls::awake`); a peer whose next post has landed by then sends it round
again with no park. Each round is one bounded pass and answers whatever else
is ready, so the wait returns as soon as anything is. With nothing else ready
the thread stays in the kernel for as long as the peer wins that race, and
`watch::wait_until` tells a thread it was killed only where it would park.
Not measured: no test sustains the race. On main the same posts each return
the waiter to Ring 3 with an answer for nothing.

**Exit**: a round that answered nothing reads the caller's kill before it goes
round again, as `ops::until_answered` does, with a test; or a measurement
that no peer sustains the race.
