---
status: open
kind: defect
opened: 2026-10-01
---

# An AP its host has not scheduled for 100 ms is booted without

`virt_smp` red in `636r2-virt.log` (`wt/toyos-rulesbatch` `8f142ba9e`, userland only, TCG,
`VirtEl2`, a loaded host, "liveness ceilings paid at 1.93x"):

```
[kernel 0.236 cpu0] SMP: cpu6 mpidr=0x6 online
[kernel 0.368 cpu0] SMP: cpu7 mpidr=0x7 did not echo within 100ms (the machine boots with the CPUs that came up before the first that did not); the rest stay off
[kernel 0.372 cpu0] SMP: 7 of 8 MADT CPUs online
```

`time::AP_START` gives an AP 100 ms between `CPU_ON` (or the SIPIs) and its echo, and its expiry
boots the machine without that CPU for good. On metal an AP that is alive answers in
microseconds; under any hypervisor a vCPU its host has not scheduled answers late, and 100 ms of
steal reads as a dead CPU. Linux waits 5 s for the same echo on arm64
(`arch/arm64/kernel/smp.c`, `__cpu_up`: `wait_for_completion_timeout(&cpu_running,
msecs_to_jiffies(5000))`) and 10 s in its generic bring-up (`kernel/cpu.c`,
`cpuhp_wait_for_sync_state`).

Owner: the orchestrator.

**Exit**: the bound is one on a dead CPU — the span this kernel already holds a live CPU to
(`time::DEAF_CPU`) — and `virt_smp` passes in a whole-suite run at a host load average above
the dev host's core count.
