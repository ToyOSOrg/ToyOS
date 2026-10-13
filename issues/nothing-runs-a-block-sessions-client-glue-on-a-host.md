---
status: open
kind: tooling
opened: 2026-10-13
---

# Nothing runs a block session's client glue on a host

diskserver's
`a_session_opened_over_a_killed_servers_page_is_told_after_its_ends_are_made`
holds the server's half of a reopen: a client told its session is open finds
the new server's ends made. What the client does with a page that breaks the
protocol is read off the code and not run. `Session::pump`, `drain` and
`wait` (`system/diskserver/src/session.rs`) hold a `Region`, a `Connection`
and a `Poller`, so no host reaches them: that a `Violation::HeadPastTail` or
`Violation::Tag` ends the session, and that `disk::Served` answers its caller
`Gone`, is unexercised.

**Exit**: a host test drives the client's ring half over a page whose server
cursor is past what the client published, and the session ends with the
caller's call refused; red with that check in `Session::drain` removed.

Owner: the orchestrator.
