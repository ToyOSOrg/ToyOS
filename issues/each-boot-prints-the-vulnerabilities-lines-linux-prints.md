---
status: open
kind: track
opened: 2026-09-29
---

# Each boot prints the vulnerabilities lines Linux prints

**Built**: each boot logs `mitigations: cpu0 <vulnerability>: ` and what
`kernel::mitigation::decide` decided for that CPU from the words it logs on
the line before them: `not affected`, `mitigated: ` and what by, or
`vulnerable: needs ` and what is missing, with what is in force and what the
CPU does not enumerate. A vulnerability that needs a guest reads `mitigated:
no guest runs`.

**Left**: the lines read on each proving machine, and compared there with the
lines Linux at `Ubuntu-6.8.0-142.142` prints.

**Exit**: each proving machine's ToyOS boot prints, for each vulnerabilities
file, a line stating what the function decided for that CPU from the words
ToyOS read there, so its line is the function's verdict on them. It omits only
clauses about what ToyOS does not run: `spectre_v1`'s swapgs barriers while it
runs no `swapgs`, the KVM and VM-exit clauses since it runs no guest, and
`IBRS_FW` since the kernel calls no firmware. **Mutation**:
`ARCH_CAPABILITIES` read as 0 changes the T14's lines. **Oracle**: the
vendor's guidance through that function. The line Linux at
`Ubuntu-6.8.0-142.142` prints on each machine is the goal, compared in
evidence; a clause ToyOS's line lacks is a finding against this issue, never a
test's expected value.
