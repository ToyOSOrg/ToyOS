---
status: assigned
kind: defect
opened: 2026-09-27
---

# A read or write answered Lost is never answered

`Client::complete` (`toyos-blockring/src/client.rs`) takes a completion's tag
off the wire before it looks at the status. A user read or write answered
`Status::Lost` is then refused as `Violation::Entry`, and the session ends; but
the tag is no longer on the wire, so `Client::session_ended` does not answer it
`Refused`, and nothing ever answers its ticket. blockd's client
(`userland/blockd/src/session.rs`) keeps that ticket in `pending` for good,
across every reconnect. That breaks the module's own "every request asked for
is answered exactly once". Only a server that breaks the protocol writes that
completion; blockd does not.

Held by the orchestrator.

**Exit condition.** A completion the client refuses leaves its tag answered by
the session's end — refused before the tag is taken off the wire, or answered
where it is refused — and a model or unit test in which a server answers a
write `Lost` goes red without the fix and green with it.
