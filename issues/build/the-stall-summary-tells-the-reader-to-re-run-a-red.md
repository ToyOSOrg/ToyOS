---
status: open
kind: tooling
opened: 2026-09-27
---

# The STALL summary tells the reader to re-run a red

A run with a STALL exits 1, and its summary (`tests/toyos.rs`, the `stalls`
arm of the run's report) says `Re-run; if one recurs with the host to itself,
the guest really is stopping.` A STALL is a red, and a red is fixed or
disabled with its issue, never re-run.

Seen at PR #542's head as the negative control for `shipped_config_boots`: a
program init never starts ended as `STALL shipped_config_boots (303s)` with
that sentence under it.

**Exit condition.** The sentence is deleted. Owner: orchestrator.
