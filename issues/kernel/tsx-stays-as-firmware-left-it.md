---
status: open
kind: defect
opened: 2026-09-29
---

# TSX stays as firmware left it

The kernel never reads or writes `IA32_TSX_CTRL` (0x122) or
`MSR_TSX_FORCE_ABORT` (0x10F), so where a CPU still runs transactions, user
code keeps RTM and its enumeration. Linux at `Ubuntu-6.8.0-142.142`, under the
T14's `CONFIG_X86_INTEL_TSX_MODE_OFF=y`, runs `tsx_init`
(`arch/x86/kernel/cpu/tsx.c:158-245`): it clears `RTM_ALLOW` in
`IA32_MCU_OPT_CTRL` where TAA, `TSX_CTRL` and `SRBDS_CTRL` meet
(`tsx.c:139-156`), clears enumeration where RTM_ALWAYS_ABORT
(`tsx.c:108-137,170-176`), and otherwise, where
`ARCH_CAPABILITIES.TSX_CTRL_MSR`, sets `RTM_DISABLE | TSX_CPUID_CLEAR`
(`tsx.c:23-41,189-226`).

**Exit**:
`issues/kernel/a-pure-function-decides-a-cpus-speculation-mitigations-as-linux-does.md`
carries `tsx_init`'s decision and every CPU applies it; on the T14, ToyOS's
reads of 0x122, 0x10F and 0x123 equal the Linux reads of
`issues/hardware/linuxs-readings-of-the-t14-and-the-tcg-model-are-not-committed.md`
where each exists, and dropping the write leaves 0x122 or 0x10F at firmware's
value and reds it.
