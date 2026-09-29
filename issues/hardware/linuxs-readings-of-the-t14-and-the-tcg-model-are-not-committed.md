---
status: assigned
kind: tooling
opened: 2026-09-29
---

# Linux's readings of the T14 and the TCG model are not committed

The floor of
`issues/kernel/the-kernel-is-at-least-as-secure-as-linux-on-every-machine-toyos-supports.md`
is tag `Ubuntu-6.8.0-142.142` (53e5d07aac028a1523ab0b115f079d6d1bc831ef) of
`https://git.launchpad.net/~ubuntu-kernel/ubuntu/+source/linux/+git/noble`,
config sha256 3b8533dd9d235ca634ac58f82c5ce1ee35f12ef620693e17033184d2c9ca5890,
and what it reads on the T14 and on the PR gate's TCG model are the fixtures
of `issues/kernel/a-pure-function-decides-a-cpus-speculation-mitigations-as-linux-does.md`.
Pull request #601 holds it. Ubuntu leaves the T14 only after this issue and
`issues/hardware/no-program-measures-toyos-against-linux-on-one-machine.md`
close.

**Exit**: both captures committed, and no Linux image; before the wipe the
T14's adds CPUID 5, 0x19 and 0x80000001, MSR 0xCF, MSR 0x3A (`rdmsr -a 0x3a`, IA32_FEAT_CTL), the split-lock line, and
the config's `X86_KERNEL_IBT` and `X86_INTEL_MEMORY_PROTECTION_KEYS`.
**Mutation**: a config hash off by one byte is refused. **Oracle**: that
Linux.
