---
status: open
kind: defect
severity: [security]
opened: 2026-09-29
---

# No program runs with speculative store bypass disabled

**Built**: a row of the system manifest asks for isolated speculation with
`isolated-speculation = true`; the supervisor starts it asking
(`SpawnArgs::isolated_speculation`), and every program a program that asked
starts itself asks too (`kernel::mitigation::Speculation::of_child`). While
a thread of a program that asked runs, its CPU's `IA32_SPEC_CTRL` holds SSBD,
and STIBP where its decision names it, or AMD's `VIRT_SPEC_CTRL` SSBD in a
guest that enumerates only that (`Decision::on_request`); every other thread,
and the idle context, runs with them clear
(`arch::x86_64::mitigation::switch_to`). Linux at `Ubuntu-6.8.0-142.142`
defaults to `prctl`, where a task that asks runs with it
(`arch/x86/kernel/cpu/bugs.c:2172,2278`). `speculation_on_request`, a member
of the `shared-debug` boot, reads the register through `SYS_DEBUG`: clear for
itself, set for `speculationprobe`, whose row asks, and for the child the
probe starts without asking. AArch64 is
`issues/no-aarch64-program-runs-with-speculative-store-bypass-disabled.md`.

**Left**:
- **Owed on metal:** `speculation_on_request`, a member of main's
  `shared-debug` boot.
  - Benches: the T14 and the AMD bench.
  - Main must show the member exit 0 with its line, which on the T14 names
    `0x4` (SSBD) asked and on the AMD bench `0x6` (SSBD and STIBP).
  - The same boot with `Speculation::of_child` ignoring the spawner's ask
    must red it, the mutation below.

**Exit**: on every proving machine a flagged program runs with SSBD set,
through the control
`issues/a-pure-function-decides-a-cpus-speculation-mitigations-from-its-vendors-guidance.md`
names for its CPU, and so does its child, while an unflagged one runs with it
clear. **Mutation**: no inheritance. **Oracle**: the CPU's own register.
