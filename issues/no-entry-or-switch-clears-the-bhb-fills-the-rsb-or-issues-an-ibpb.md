---
status: open
kind: defect
severity: [security]
opened: 2026-09-29
---

# No entry or switch clears the BHB, fills the RSB or issues an IBPB

**Built**:
- Where a CPU's decision names the BHB cleared on entry, every entry from
  Ring 3 runs Intel's short branch history clearing sequence ("Branch
  History Injection and Intra-mode Branch Target Injection", Listing 1)
  before any indirect branch (`arch::x86_64::mitigation::clear_branch_history`).
  `LSTAR` names the `SYSCALL` entry that calls it, and every gate the IDT
  names opens with a 12-byte prefix that calls it where the frame's `CS` is
  Ring 3's; such a CPU loads the table that enters each gate there, and
  every other CPU the one that enters past it, so it runs none of it
  (`arch::x86_64::entry::gate_prefix`, `arch::x86_64::idt`).
  `branch_history_cleared_per_syscall` counts at least one clear per
  syscall where the CPU's decision names it.
- A switch into a program's address space issues an IBPB where the space
  this CPU last ran a program in is another and either asked for isolated
  speculation (the ask of
  `issues/no-program-runs-with-speculative-store-bypass-disabled.md`) and the
  CPU's decision names IBPB (`kernel::mitigation::LastAsked`,
  `arch::x86_64::mitigation::switch_to`). On an AMD CPU whose IBPB leaves the
  return address predictor's entries, which `IBPB_RET` (APM Vol. 3
  Fn8000_0008_EBX[30]) does not enumerate, AMD-SB-1040 asks 70586's V2-3
  after it: the decision names the predictor filled after IBPB there, and
  the switch follows each IBPB with 32 calls. Intel's IBPB covers the RSB
  ("Speculative Execution Side Channel Mitigations", the IBPB note on near
  returns), and no other fill on a switch is a vendor's for a CPU the
  decision covers. `an_ibpb_goes_between_two_spaces_one_of_which_asked`
  holds two IBPBs over B, C, idle, B, A, idle, A, B, where A asked, and one
  under the mutation that decides by the previous space alone.
  `speculation_on_request` counts IBPBs on a CPU where a program that asked
  takes turns with this one, and none where neither asked.
- Linux at `Ubuntu-6.8.0-142.142` runs `clear_bhb_loop` on `SYSCALL` entry
  where its decision selects the BHI loop (`arch/x86/entry/entry_64.S:1534`),
  and issues an IBPB on a switch between address spaces one of which asked
  for it (`arch/x86/mm/tlb.c:384`).

**Left**:
- A guest that clears with the short sequence does not tell its VMM so:
  Intel's guidance asks it to set `BHB_CLEAR_SEQ_S_USED` in
  `MSR_VIRTUAL_MITIGATION_CTRL` where `MSR_VIRTUAL_MITIGATION_ENUM`
  enumerates `BHB_CLEAR_SEQ_S_SUPPORT`, so that a VMM migrating it to a part
  the short sequence does not cover sets `BHI_DIS_S` under it. A guest on
  the guest runner is one; no proving machine is.
- **Owed on metal:** `branch_history_cleared_per_syscall` and
  `speculation_on_request`, members of main's `shared-debug` boot.
  - Benches: the T14 and the AMD bench.
  - Main must show both members exit 0: on the T14 the first naming at
    least 1000 clears over 1000 syscalls, on the AMD bench naming no clear;
    the second naming at least 2 IBPBs beside the probe and none beside the
    child that did not ask.
  - The T14's `mitigations` row, the `metalcase` boot of main, with
    `branch_history_injection: mitigated: BHB cleared on entry` on every CPU.

**Exit**: a gate over `kernel.elf`, on the decoder of
`issues/no-gate-decodes-kernel-elfs-instructions.md`, matches it against
the branch history clearing and return predictor filling sequences Intel's
and AMD's published guidance prescribes; on every proving machine probes
count what
`issues/a-pure-function-decides-a-cpus-speculation-mitigations-from-its-vendors-guidance.md`
selects for it: a clear per syscall where it selects the loop, and IBPBs
where a program that asked takes turns with another on one CPU and none
where neither asked, each followed by the fill where it selects one. A host
test holds the IBPB's decision over B, C, idle, B, A, idle, A, B to two.
**Mutation**: either sequence edited, the clear's condition inverted, no
fill, the IBPB decided by the previous space alone. **Oracle**: the vendors'
published sequences. Linux's sequences are a goal compared in evidence,
never the gate's expected text.
