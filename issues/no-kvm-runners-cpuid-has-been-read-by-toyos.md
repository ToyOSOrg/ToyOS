---
status: open
kind: tooling
opened: 2026-09-29
---

# No KVM runner's CPUID has been read by ToyOS

The nightly guest suite's KVM shards run on EPYC 7763, 9V74 and 9V45 CPUs
(nightly run 36496779560), each as `-cpu host,+rdrand,+smap,+fsgsbase,+x2apic,+smep`
with `-smp cores=N`. Which bits KVM passes through, and whether it enumerates
`IA32_ARCH_CAPABILITIES`, are unknown: no ToyOS boot on a runner has logged
them.

**Exit**: a ToyOS guest boot on each runner CPU model logs CPUID leaves 0, 1,
7.0, 7.2, 0x80000000, 0x80000008 and 0x80000021, MSR 0x10A where it is
enumerated, and the microcode revision. One fixture per model is committed for
`issues/a-pure-function-decides-a-cpus-speculation-mitigations-from-its-vendors-guidance.md`.
