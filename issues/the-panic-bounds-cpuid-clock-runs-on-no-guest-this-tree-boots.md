---
status: open
kind: tooling
opened: 2026-09-06
---

# The panic bound's CPUID clock runs on no guest this tree boots

`cpu::stated_counter_hz`, CPUID leaf 15H, is what times the panic bound
before `clock::init` on a part that states a rate, the T14 among them. Its
arithmetic is checked by a `const _` in `lib/tsc`; its *use* is checked by
nothing, because no guest this tree boots reaches it.

TCG's `qemu64`, whose largest standard leaf is below 15H, states none, so
`an_early_panic_resets_at_its_bound` times its bound by the ceiling on the
counter's rate and asserts that its line says so. The guest runner's KVM
boots take `-cpu host,…` and whichever branch the runner's CPU states;
nothing asserts which.

What is owed is an instrument, not a fix: either a boot option that hands one
guest a CPU model stating a TSC frequency, or an assertion on a KVM shard that
the early-panic line names the stated rate. Until then the branch that times
the owner's T14 before its clock is carried by a compile-time arithmetic check
and a reading of the SDM.
