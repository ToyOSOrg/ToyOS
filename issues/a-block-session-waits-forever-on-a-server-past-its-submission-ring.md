---
status: open
kind: defect
opened: 2026-10-13
---

# A block session waits forever on a server past its submission ring

A diskserver client trusts its server's cursor on the submission ring. Where
the server's consumer index is past what the client published,
`Session::pump` (`system/diskserver/src/session.rs`) meets the transport's
`Violation::HeadPastTail` from `space` and only stops pushing, on the comment
that "the next read of the connection is where that is acted on". Nothing
acts on it: `drain` reads only the completion ring, so with no completion to
refuse `wait` blocks in `poller.wait(1, u64::MAX, ..)` until the server hangs
up. A server that keeps its connection and writes that index holds every
caller of the session for good: an unbounded wait on an untrusted peer, where
the producer's violation should end the session as `drain`'s do.

**Exit**: a submission-ring violation ends the session, so the caller's call
is refused; held by a host test of the client's ring half over a page whose
server cursor is past what the client published, red with that end removed.

Owner: the orchestrator.
