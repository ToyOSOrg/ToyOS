---
status: open
kind: track
opened: 2026-09-29
---

# The kernel is at least as secure as Linux on every machine ToyOS supports

Parity with Linux at tag `Ubuntu-6.8.0-142.142`
(53e5d07aac028a1523ab0b115f079d6d1bc831ef) of
`https://git.launchpad.net/~ubuntu-kernel/ubuntu/+source/linux/+git/noble`,
config sha256 3b8533dd9d235ca634ac58f82c5ce1ee35f12ef620693e17033184d2c9ca5890,
is the floor on every CPU ToyOS supports, and the kernel also takes every
security feature such a CPU offers. The proving machines are the T14 and
AMD EPYC KVM guests; the PR gate's TCG model proves wiring only. A
probe is a `boot-actuators` arm or a `test-actuators` `SYS_DEBUG` action.

One line of Linux's is declined, and ToyOS stays below it there: the
firmware's memory-overwrite request, by the owner's "Keep crash records"
(`issues/the-loader-never-sets-the-firmwares-memory-overwrite-request.md`).

**Exit**: every issue below is closed, in the order listed. The T14's Linux
readings issue is not among them: since #915 it owes no reading this track's
work needs.

- `issues/a-pure-function-decides-a-cpus-speculation-mitigations-from-its-vendors-guidance.md`
- `issues/spec-ctrl-and-gds-stay-as-firmware-left-them.md`
- `issues/no-program-runs-with-speculative-store-bypass-disabled.md`
- `issues/no-gate-decodes-kernel-elfs-instructions.md`
- `issues/no-entry-or-switch-clears-the-bhb-fills-the-rsb-or-issues-an-ibpb.md`
- `issues/user-pointer-checks-have-no-spectre-v1-fence-and-smap-is-optional.md`
- `issues/indirect-branches-and-returns-run-without-thunks.md`
- `issues/tsx-stays-as-firmware-left-it.md`
- `issues/no-user-address-is-drawn-per-spawn.md`
- `issues/the-kernel-has-no-stack-protector.md`
- `issues/no-kernel-address-is-drawn-per-boot.md`
- `issues/every-syscall-runs-at-one-kernel-stack-offset.md`
- `issues/kernel-functions-return-with-their-used-registers-intact.md`
- `issues/a-threads-kernel-stack-has-no-guard-page.md`
- `issues/the-kernels-relro-stays-writable.md`
- `issues/x86-64s-boot-map-is-writable-and-executable.md`
- `issues/the-kernel-heap-has-none-of-slubs-hardening.md`
- `issues/a-device-without-a-domain-of-its-own-reaches-all-memory.md`
- `issues/user-programs-run-without-a-shadow-stack.md`
- `issues/the-kernel-runs-without-indirect-branch-tracking.md`
- `issues/the-kernel-runs-without-a-shadow-stack.md`
- `issues/user-programs-have-no-protection-keys.md`
- `issues/programs-hold-aes-keys-as-key-locker-handles.md`
- `issues/a-split-lock-goes-unnoticed.md`
- `issues/each-boot-prints-the-vulnerabilities-lines-linux-prints.md`
