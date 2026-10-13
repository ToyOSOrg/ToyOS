---
status: open
kind: defect
opened: 2026-10-12
---

# The hard-lockup detector samples no AMD CPU

`kernel/src/hardlockup/mod.rs` arms a counter only where CPUID leaf 0AH
states Intel's architectural performance monitoring. An AMD CPU states none
there, so on the AMD bench the boot says `hard lockup: 5000 ms, but CPUID
states no architectural performance counter on this machine, so nothing
samples a cpu that stops taking interrupts` (PR #874's whole-suite run on that
bench), and the `hard_lockup_ends_a_deaf_cpu` metal row is red there. Its
probe sends the deaf CPU the NMI from another CPU instead, which the row does
not count as the detector.

AMD CPUs have core performance counters of their own, which CPUID
Fn8000_0001_ECX states and the local APIC's performance-counter LVT delivers
(AMD64 Architecture Programmer's Manual Vol. 2, "Performance Monitoring
Counters"); the AMD bench's leaf is unread here.

Owned by `issues/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md`,
whose next step arms the detector on every boot.

**Exit**: the detector samples every CPU of an AMD machine from its own
counter, and `hard_lockup_ends_a_deaf_cpu` passes on the AMD bench.
