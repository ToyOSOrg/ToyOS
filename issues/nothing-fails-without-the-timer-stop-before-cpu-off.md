---
status: open
kind: tooling
opened: 2026-10-01
---

# Nothing fails without the timer stop before `CPU_OFF`

`kernel/src/arch/aarch64/power.rs`'s `cpu_off` stops the CPU's timer before
it calls `CPU_OFF`. The reason is DEN0022 §5.5.2: an interrupt that reaches a
core `CPU_OFF` turned off is an erroneous state. That section is where the
requirement comes from. It is not evidence that this kernel meets it, and no
test can fail if the stop is deleted:

- QEMU's PSCI is the only provider any guest reaches, and it delivers nothing
  to a vCPU it holds off.
- A second provider, TF-A under QEMU, needs a C cross-toolchain, which is not
  a declared host tool.
- No AArch64 metal target exists. By the owner's ruling in
  `issues/toyos-runs-on-arm64.md`, no ARM hardware is a target.
- `SGI_OFF` lands on idle CPUs. An idle CPU with no parked deadline has
  already stopped its one-shot (`kernel/pure/sched/timer.rs:43`), so deleting
  the stop changes nothing even on metal in that case.

The stop serves a wider claim: no interrupt reaches a core `CPU_OFF` turned
off. That claim also covers a kick from a CPU that is not off yet, and the
stop does nothing about kicks.

**Evidence**: the deletion mutation, applied at `94dea677c` and reversed
after the run:

```diff
--- a/kernel/src/arch/aarch64/power.rs
+++ b/kernel/src/arch/aarch64/power.rs
@@ pub(super) fn cpu_off() -> ! {
-    irqchip::stop_timer();
```

`cargo test --test toyos-build -- virt_` exited 0 with it applied: `19
passed, 19 total`, the power-off of eight CPUs in `virt_smp`, `virt_el1_smp`
and `virt_off_names_the_cpus_left_on` among them. The patch and the run are
https://github.com/ToyOSOrg/ToyOS/pull/647#issuecomment-5956924013.

**Exit**: an AArch64 metal target where `SGI_OFF` lands on a CPU whose timer
is armed, with the deletion mutation red there.

Owner: `kernel/src/arch/aarch64/power.rs`'s `cpu_off`, under
`issues/toyos-runs-on-arm64.md`.
