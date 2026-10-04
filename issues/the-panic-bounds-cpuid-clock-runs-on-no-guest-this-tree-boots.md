---
status: open
kind: tooling
opened: 2026-09-06
---

# The panic bound's CPUID clock runs on no guest this tree boots

`clock::cpuid_tsc_hz` is what times the panic reboot bound before
`clock::init` — the only clock a panic inside `init_bsp` has. Its arithmetic is
checked by a `const _` in `kernel/src/clock.rs`; its *use* is checked by nothing,
because no guest this tree boots reaches it.

Measured 2026-09-06, dev host, `screen_early_panic` with the panel decoded: a
`test-early-panic` guest renders `panic: holding this panel: this CPU states no
TSC frequency and none is calibrated`. `toyos_build::CPU_TCG` is
`qemu64,+rdrand,+smap,+fsgsbase,+x2apic,+smep`, whose maximum basic CPUID leaf is
below 15H, so both leaves are absent and the refusal branch is what runs. The
KVM shards boot `CPU_KVM` (`host,…`) and would take the CPUID branch on an Intel
runner; nothing asserts that they do, and the dev host is cross-arch TCG only.

What is owed is an instrument, not a fix: either a boot option that hands one
guest a CPU model stating a TSC frequency, or an assertion on a KVM shard that
the early-panic panel names a source rather than refusing one. Until then the
branch that decides whether the owner's T14 can time its own reboot is carried
by a compile-time arithmetic check and a reading of the SDM.
