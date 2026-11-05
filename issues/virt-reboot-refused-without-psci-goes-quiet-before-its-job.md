---
status: open
kind: defect
severity: [hang]
opened: 2026-11-05
---

# `virt_reboot_refused_without_psci` goes quiet before its job ends

On the guest runner the AArch64 row stalls: the console stops at the kernel's
`Boot: complete` and its boot-time summary, the `reboot` job never ends, and
the harness reports `STALLED: waiting for the job reboot to end — it went
quiet` after 17 s. It reds on `main` at `ec8912181` (guest-rows run
42094494342, alone) as on #1196's branch (runs 42085130353 and 42091551186),
so it predates that change. The same image with PSCI, `virt_reboot`, passes on
the same runs; this one boots with `psci-withheld`, so one CPU.

Unmeasured: whether userland starts at all on a one-CPU `virt` boot without
PSCI, or starts and is silent.

Owner: nobody yet.

**Exit**: the row green on the guest runner, with the cause named.
