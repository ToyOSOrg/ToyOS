---
status: open
kind: defect
opened: 2026-10-01
---

# The ABI still keeps retired syscall numbers

The ABI is completely unstable and a removed syscall's number is free (owner,
2026-10-01): there are no retired numbers and no compatibility shims. The tree
still keeps them:

- `kernel/src/syscall/dispatch.rs`'s `retired_syscalls!` names deleted calls so
  that "an old binary is told which call it was", logging each call of one;
  every other unassigned number answers `InvalidArgument` silently.
- `toyos-abi/src/syscall.rs` and `toyos-abi/src/inbox.rs` carry a "formerly …"
  or "retired and unused" entry per deleted syscall, `SYS_DEBUG` action and
  inbox op, `SYS_INBOX_SETUP`'s doc states the retirement rule, and
  `kernel/src/inbox/mod.rs` names op 2 retired.
- `toyos-abi/src/syscall.rs`'s `device_classes!` keeps device classes 3 (`Nic`)
  and 4 (`Audio`) "retired rather than reused", and the decode test in
  `toyos-abi/src/inventory.rs` refuses them as retired.
- `tests/toyos-rust-tests/src/bin/panic_halts_first.rs` and `tests/toyos.rs`
  take syscall 26 as their logged refusal and read `syscall 26 is retired`.
- Plans still follow the old rule:
  `issues/kernel/sys-clock-realtime-is-now-a-format-of-sys-clock-epoch.md`,
  `issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`,
  `issues/kernel/the-kernel-still-parses-what-userland-writes.md`,
  `issues/kernel/the-capability-end-state-is-twelve-answers.md`,
  `issues/diagnostics/the-kernel-keeps-nothing-it-enumerates.md`,
  `issues/diagnostics/no-cyclictest.md`, and
  `issues/isolation/the-supervisor-is-host-tested-and-owns-the-stop.md`, whose
  "ABI brief" names a gate `.claude/agents/implementer.md` no longer has.

**Exit**: `retired_syscalls!` and every retirement entry are gone, a deleted
number answers as an unassigned one does, both test sites take their logged
refusal from something live, and no issue plans by retirement.

Owner: `toyos-abi` and `kernel/src/syscall/dispatch.rs`, whoever next changes
the ABI.
