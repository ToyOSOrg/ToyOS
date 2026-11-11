---
status: open
kind: track
opened: 2026-09-29
---

# Each boot prints the vulnerabilities lines Linux prints

**Built**: each boot logs `mitigations: cpuN <vulnerability>: ` and what
`kernel::mitigation::decide` decided for that CPU from the words it logs on
the line before them: `not affected`, `mitigated: ` and what by, or
`vulnerable: needs ` and what is missing, with what is in force and what the
CPU does not enumerate, then `; SMT vulnerable: ` and the exposure where the
kernel stays open to the core's other thread. A vulnerability that needs a
guest reads `mitigated: no guest runs`. The `mitigations` metal row holds every CPU's lines to the
decision on its words, and a bench's words to its pinned ones.

**Left**:
- **Owed on metal:** the `mitigations` and `microcode` rows, one `metalcase`
  boot each on the T14 and on the AMD bench. Main must show both rows green,
  with no hang and no early or `mitigation:` panic, and:
  - on the T14, cpu0's lines exactly `checks::T14_BENCH_LINES`, and
    `mcu_opt_ctrl` with bits 0 (`RNGDS_MITG_DIS`) and 4 (`GDS_MITG_DIS`)
    clear; a set bit is reported, since those lines assume both clear;
  - on the AMD bench, cpu0's lines exactly `checks::AMD_BENCH_LINES`,
    `de_cfg2` and `c001_1029` each with bit 1 set, and the value of
    `c001_1029_found` reported, which tells whether firmware had set the bit;
  - on either, a later CPU's lines only where its registers differ.
- `spec_store_bypass` reads `vulnerable: needs SSBD for programs that ask`:
  nothing applies SSBD for a program that asks yet.
- The lines read on each proving machine, compared there with the lines
  Linux at `Ubuntu-6.8.0-142.142` prints.

**Exit**: each proving machine's ToyOS boot prints, for each vulnerabilities
file, a line stating what
`issues/a-pure-function-decides-a-cpus-speculation-mitigations-from-its-vendors-guidance.md`
decided for that CPU from the words ToyOS read there, and on the T14 the words
it logs equal the T14's words that issue commits with their provenance, so its
line is the function's verdict on them. It omits only clauses
about what ToyOS does not run: `spectre_v1`'s swapgs barriers while it runs no
`swapgs`, the KVM and VM-exit clauses since it runs no guest, and `IBRS_FW`
since the kernel calls no firmware; `spec_store_bypass` reads "Mitigation:
Speculative Store Bypass disabled per program". **Mutation**:
`ARCH_CAPABILITIES` read as 0 reds the T14. **Oracle**: the vendor's
guidance through that function, over the words committed with provenance. The line Linux at
`Ubuntu-6.8.0-142.142` prints on each machine is the goal, compared in
evidence; a clause ToyOS's line lacks is a finding against this issue, never a
test's expected value.
