# Tests

The mechanics live where the work is: profiles and shapes in `tests/common/`, registration in `tests/toyos.rs` — read those, not this file, for how the harness works.

## Caveats that bite every agent

- **A host that sleeps mid-run is reported, not measured** — a run whose wall clock jumped against the monotonic one reports `INVL` per test and exits 2. A wild outlier *not* marked that way is a real finding.
- **A machine-wide kernel panic reds whichever test was running** — that red's name is the workload, never the cause. `QEMU died before ===READY=== (status 0)` is the same thing said silently: a guest that reset itself is a kernel death, and its evidence is the boot log.
- **A machine-wide death during boot is reproduced by boots, not by suites** — parallel `bootable.img` guests, each waited on its completion marker and never on a fixed timer; the baseline is measured in the same session as the arm; a death counts whether or not a marker printed, so run guests with `-action reboot=shutdown -action shutdown=pause` and read a silent one's registers over QMP; a defect whose rate is set by interrupts per unit of guest work is measured on the *slowest* instrument — TCG can be the stronger oracle.
- **`/system/bin/supervisor` speaks in every program's name before that program runs** — a predicate keyed on a `<program>: ` prefix is satisfied by the wrong speaker; wait for the whole line, in the constant the assertion also reads.
- **A guest binary cannot ask what a handle it does not hold does** — the probe ends its caller with exit 139, so it runs in a child, one fault per child.
- **A boot's capture has two pieces** — `boot_log()` ends at the ready marker, `run_test`'s capture begins at `===TEST_START===`.
- **A guest of an architecture the host is not runs under TCG** (`Arch::accel`) — TCG prices an uncontended atomic read-modify-write unlike hardware, and an x86-64 guest there boots `qemu64`, which has no PCID: every `INVPCID` path is dead, so a change gated on a CPUID feature is unverified by a green TCG suite.
- **A liveness ceiling scales by two host facts** — boot-derived host speed *and* the guest's own `vcpus/cores` oversubscription. Widen a *liveness* guard for this, never a correctness bound.
- **A wedge verdict needs both the budget spent and the guest gone quiet** — a healthy idle guest can be silent for minutes, and a guest still talking past its budget is slow, not stuck; only a far backstop stands behind a guest that keeps talking.
- **A measured bound is asserted against the derivation, never against the measurement** — a bound that has to be widened to pass is a finding. A test asserting a kernel `Budget` never expires asserts a bound the kernel does not promise; the red is only the outcome that is neither the answer nor the declared degradation.
- **A crafted-input test asserts the harm before the return value, and never Debug-prints a refused value** — an unrefused one is as large as the input asked for.
- **A stimulus sent through a channel that can silently lose it is verified before its effect is asserted** — QEMU's PS/2 queue drops the seventeenth byte, so typed input paces against the guest's report; a guest's console reaches the host as whole lines only, so a partial line exists on no channel.
- **A harness field that can be silently inert is this suite's worst defect class** — where two options can describe the same guest they refuse each other by name, and an image is asked what it is armed with.
- **A test whose premise is arranged by a defect passes for the wrong reason** — a staging device's resource has to survive its own enumeration.
- **Host suites** are the root `Cargo.toml`'s `[workspace]` members, the kernel's library, the SDK and every userland crate `src/userlandhost.rs` finds a test in; `src/ci.rs`'s `host` runs them all. `kernel/loom/` is the memory-ordering check — x86 TSO hides a missing acquire edge from every guest test.
