---
status: open
kind: tooling
opened: 2026-10-12
---

# Nothing holds that a child spawned directly holds no launcher

The `launch_authority` metal row, deleted by the owner's ruling on pull
request #889, ended by spawning its own binary, which no row declares, and
asserting the child held no launcher: none under the `launcher` label, and
none in the `svc` namespace it inherits. No test at any tier asserts that
now.

This file owes std's half of it: a direct spawn endows the child a copy of
the caller's `svc` and only what the caller endowed, never the caller's
launcher (`sdk/std/sys/process.rs`). The supervisor's half, a row's launcher
endowed under `launcher` and never in `svc`, is owed by stage 2 of
`issues/the-supervisor-is-host-tested-and-owns-the-stop.md`.

**Owner**: the orchestrator.

**Exit**: a test on the cheapest tier that reaches std's direct spawn holds
that it endows the child no launcher, and a mutation named in its pull
request reds it: a direct spawn endowing the child its caller's launcher.
