---
status: open
kind: defect
opened: 2026-10-12
---

# PCID waits on INVPCID, so the AMD bench flushes its TLB on every process switch

`CR4.PCIDE` is declared only where CPUID enumerates both PCID and INVPCID
(`kernel/src/arch/x86_64/control_regs.rs:439-442`), on the ground that
"nothing could flush by ASID" without INVPCID. That is not the hardware's
rule: with `PCIDE` set, a CR3 load with bit 63 clear invalidates the
entries of the PCID it loads (SDM Vol. 3A §4.10.4.1). Without `PCIDE`,
`Cr3::activate` loads CR3 with no tag (`kernel/src/arch/x86_64/paging.rs:298-303`),
and with no global page anywhere
(`issues/page-global-is-a-decision-nobody-has-made.md`) every switch between
address spaces drops every translation, the kernel's direct map included.

The T14 runs with `PCIDE` (`cr4=0x00320e68 … pcid`,
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6114828923). The
AMD bench, a Ryzen 7 4800U, runs without it (`cr4=0x00300e68`, its
`control_regs` line naming no `pcid`, at `fb03703a1`,
https://github.com/ToyOSOrg/ToyOS/pull/852#issuecomment-6107031099). Which of
the two bits it lacks is unread: no boot logs CPUID.01H:ECX[17] or
CPUID.(07H,0):EBX[10]. What the flush costs it is unmeasured too, and ToyOS
reads no AMD performance counter.

**Exit**: the AMD bench's boot logs both bits. Where it enumerates PCID, its
`control_regs` line names `pcid`, and a cross-process ping-pong on it is
measured with the tags and without. Where it does not, this file is closed with
the reading.
