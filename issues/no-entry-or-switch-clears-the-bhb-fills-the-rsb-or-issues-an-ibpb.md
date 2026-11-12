---
status: open
kind: defect
severity: [security]
opened: 2026-09-29
---

# No entry or switch clears the BHB, fills the RSB or issues an IBPB

**Built**: a switch into a program's address space issues an IBPB where the
space this CPU last ran a program in is another and either asked for
isolated speculation (the ask of
`issues/no-program-runs-with-speculative-store-bypass-disabled.md`) and the
CPU's decision names IBPB (`kernel::mitigation::LastAsked`,
`arch::x86_64::mitigation::switch_to`). On an AMD CPU whose IBPB leaves the
return address predictor's entries, which `IBPB_RET` (APM Vol. 3
Fn8000_0008_EBX[30]) does not enumerate, AMD-SB-1040 asks 70586's V2-3
after it: the decision names the predictor filled after IBPB there, and the
switch follows each IBPB with 32 calls. Intel's IBPB covers the RSB
("Speculative Execution Side Channel Mitigations", the IBPB note on near
returns), and no other fill on a switch is a vendor's for a CPU the decision
covers. `an_ibpb_goes_between_two_spaces_one_of_which_asked` holds two IBPBs
over B, C, idle, B, A, idle, A, B, where A asked, and one under the
mutation that decides by the previous space alone. `speculation_on_request`
counts IBPBs on a CPU where a program that asked takes turns with this one,
and none where neither asked. Linux at `Ubuntu-6.8.0-142.142` issues an IBPB
on a switch between address spaces one of which asked for it
(`arch/x86/mm/tlb.c:384`).

**Left**:
- Nothing clears branch history on entry. Linux runs `clear_bhb_loop` on
  entry where its decision selects the BHI loop
  (`arch/x86/entry/entry_64.S:1534`).
- **Owed on metal:** `speculation_on_request`, a member of main's
  `shared-debug` boot.
  - Benches: the T14 and the AMD bench.
  - Main must show the member exit 0, its line naming at least 2 IBPBs
    beside the probe and none beside the child that did not ask.

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
