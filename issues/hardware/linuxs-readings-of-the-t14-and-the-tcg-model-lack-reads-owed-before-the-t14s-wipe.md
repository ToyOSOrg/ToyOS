---
status: open
kind: tooling
opened: 2026-09-29
---

# Linux's readings of the T14 and the TCG model lack reads owed before the T14's wipe

The floor of
`issues/kernel/the-kernel-is-at-least-as-secure-as-linux-on-every-machine-toyos-supports.md`
is tag `Ubuntu-6.8.0-142.142` (53e5d07aac028a1523ab0b115f079d6d1bc831ef) of
`https://git.launchpad.net/~ubuntu-kernel/ubuntu/+source/linux/+git/noble`,
config sha256 3b8533dd9d235ca634ac58f82c5ce1ee35f12ef620693e17033184d2c9ca5890,
and what it reads on the T14 and on the TCG model are the fixtures
of `issues/kernel/a-pure-function-decides-a-cpus-speculation-mitigations-as-linux-does.md`,
`toyos-cpuvuln/fixtures/`'s `t14.txt`, `t14/` and `tcg/`;
`the_t14s_facts_are_linuxs_reading_of_it` and
`the_tcg_models_signature_gives_its_lines` hold `T14` and `TCG` to them.
Ubuntu leaves the T14 only after this issue and
`issues/hardware/no-program-measures-toyos-against-linux-on-one-machine.md`
close.

**Exit**: before the wipe the T14's readings add CPUID 5, 0x19 and
0x80000001, MSR 0xCF, MSR 0x3A (`rdmsr -a 0x3a`, IA32_FEAT_CTL), the
split-lock line, and the config's `X86_KERNEL_IBT` and
`X86_INTEL_MEMORY_PROTECTION_KEYS`, each committed with the test that reads
it, and `the_t14s_facts_are_linuxs_reading_of_it` holds `T14`'s `feat_ctl`
to 0x3A's. **Mutation**: an added reading off by one bit reds the test that
reads it. **Oracle**: that Linux.

## Linux's counter readings, read 2026-10-03

The orchestrator takes this Linux as the one independent oracle for the T14's
hardware counters (`issues/diagnostics/toyos-explains-itself.md`). Read as
root under Ubuntu's `6.8.0-142-generic` from 16:22:37 UTC, one after the
other, and committed whole with their commands in the `SOURCE` beside them:
turbostat idle and under one `yes` per CPU in `tests/t14-linux/`, which
`counters_on_metal` prints ToyOS's reading beside, and `perf stat` over the
msr PMU in `toyos-cpuvuln/fixtures/t14/perf-msr.txt`, which
`the_t14_has_what_linux_counted_on_it` holds the T14's counter verdict to.
Idle, per 10 s: busy 0.11 to 0.36%, busy clock 980 to 1668 MHz, TSC 2419
MHz, SMI 0. Loaded: busy 99.77%, busy clock 3800 MHz for the first two
intervals, 3555 MHz in the third, then 3075 to 3094 MHz; SMI 0.

Owner: the orchestrator, which holds the T14 the exit runs on.
