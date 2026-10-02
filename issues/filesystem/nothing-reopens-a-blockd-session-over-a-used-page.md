---
status: open
kind: tooling
opened: 2026-10-02
---

# Nothing reopens a blockd session over a used page

A nightly went red on `blockd_io: FAIL /AFTER.BIN after the restart: Io` (run
36753172688, guest 7). blockd answered an open and only then made its ends of
the session page. A client that reconnects sends the page its last server
wrote, and one that looks before the new server's ends are made has no room to
ask (`Violation::HeadPastTail`) and hears the dead session's completion
(`Violation::Tag`). The order is a type now: `wire::Opened::over` makes a
server's ends and its answer together.

That was fixed by reading. Nothing reproduced the `Io` before it, and nothing
that runs reopens a session over a used page:

- blockd's host test reaches `Service::place` and `Service::listing`, never
  `Service::open`;
- `toyos-blockring`'s model test holds what the transport answers over a page
  a crashed session left, and is green with blockd's order reverted;
- no host runs the client's glue (`Session::pump`, `drain` and `wait` in
  `userland/blockd/src/session.rs`), so that `HeadPastTail` and `Tag` end the
  session and the caller's call is `Io` is read off it, not run;
- `520c0d129` cut `blockd_survives_its_death`, the guest test that killed
  blockd under its client, and no guest test or metal row ends blockd.

**Exit**: the restart test that Stage D of
`issues/build/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md` owes
for `blockd_survives_its_death`, at the tier that stage builds it on: red with
that `Io` under a mutation that moves blockd's two stores after its answer,
and green without it.

Owner: the orchestrator, which dispatches Stage D.
