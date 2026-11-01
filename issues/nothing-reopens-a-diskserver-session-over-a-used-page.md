---
status: open
kind: tooling
opened: 2026-10-02
---

# Nothing reopens a diskserver session over a used page

A nightly went red on `blockd_io: FAIL /AFTER.BIN after the restart: Io` (run
36753172688, guest 7). blockd answered an open and only then made its ends of
the session page. A client that reconnects sends the page its last server
wrote, and one that looks before the new server's ends are made hears the dead
session's completion. The server's half is a type now: `wire::Opened::over`
makes a server's ends and its answer together, and diskserver sends the answer
it returns.

The client's half is read off the code and not run. `Session::pump`, `drain`
and `wait` (`system/diskserver/src/session.rs`) hold a `Region`, a
`Connection` and a `Poller`, so no host reaches them. A page whose completion
ring holds the dead session's answer ends the session only through `drain`'s
refusal of a completion for no ticket in flight (`Violation::Tag`, through
`client.complete`); `disk::Served` then reconnects and asks again, and answers
its caller `Gone` once a reconnect fails or the request has been refused across
more than `MAX_RECONNECTS` ends, which a file server's caller hears as `Io`. That nothing else ends it is
`issues/a-block-session-waits-forever-on-a-server-past-its-submission-ring.md`.

**Exit**: a host test reopens a client's session over a page a killed server
left its answer on, and the session ends and the caller's call is `Io`, by
`drain`'s `Tag` refusal: red with that refusal removed, and green with it.

Owner: the orchestrator.
