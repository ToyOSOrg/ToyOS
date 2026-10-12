---
status: assigned
kind: defect
opened: 2026-09-29
---

# The kernel loads no CPU microcode

The kernel is to load CPU microcode signed by the CPU's maker, pinned by
version and hash the way vendor device firmware is (owner, 2026-09-30).
Pull request #886 builds the loader (`toyos-microcode`,
`kernel/src/arch/x86_64/microcode.rs`) and deletes this file once its exit
holds on both benches.

**Exit**: the kernel loads current microcode early on every CPU, at least as
current as Linux's.

- host: every decision `toyos-microcode` takes is refused by name and red
  under its mutation;
- metal: the `microcode` row is green on the T14, whose eight CPUs find 0xbe
  current on platform 7 as Linux runs them, and on the AMD bench, whose CPUs
  each load 0x0860010d over the firmware's 0x08600106 as Linux does; the T14
  armed with `microcode-reseal`, and the AMD bench with its trigger write
  removed, each stop at the read-back;
- guest: every guest loads nothing, for its CPU reports a hypervisor. QEMU
  11.1.1 sets CPUID.01H:ECX[31] under TCG as under KVM, read over QMP from a
  paused `qemu64` with the suite's own `-cpu` string
  (https://github.com/ToyOSOrg/evidence/blob/499ad40d53eca38cfe6eb7c3689088ee57d9402e/design-measurements.txt#L256-L269),
  so the guest suite booting green is the whole guest exit, and no guest test
  asserts the line.
