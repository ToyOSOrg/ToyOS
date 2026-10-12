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

Two decisions make it true, and neither has a test of its own:

- the supervisor endows a row's launcher in a namespace of its own under
  `launcher`, never as an entry of `svc` (`launcher_namespace`,
  `system/supervisor/src/main.rs`);
- std's direct spawn endows the child a copy of the caller's `svc` and
  only what the caller endowed, never the caller's launcher
  (`sdk/std/sys/process.rs`).

**Owner**: the orchestrator, which holds
`issues/the-supervisor-is-host-tested-and-owns-the-stop.md`, whose stage 2
moves the supervisor's namespace selection into a host-tested crate.

**Exit**: each decision is held by a test on the cheapest tier that reaches
it, and a mutation named in its pull request reds each: the launcher
added to `svc`, and a direct spawn endowing the child its caller's launcher.
