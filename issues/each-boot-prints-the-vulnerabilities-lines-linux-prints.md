---
status: open
kind: track
opened: 2026-09-29
---

# Each boot prints the vulnerabilities lines Linux prints

**Exit**: each proving machine's ToyOS boot prints, for each vulnerabilities
file, a line stating what
`issues/a-pure-function-decides-a-cpus-speculation-mitigations-from-its-vendors-guidance.md`
decided for that CPU from the words ToyOS read there. It omits only clauses
about what ToyOS does not run: `spectre_v1`'s swapgs barriers while it runs no
`swapgs`, the KVM and VM-exit clauses since it runs no guest, and `IBRS_FW`
since the kernel calls no firmware; `spec_store_bypass` reads "Mitigation:
Speculative Store Bypass disabled per program". **Mutation**:
`ARCH_CAPABILITIES` read as 0 reds the T14. **Oracle**: the vendor's
guidance through that function, and ToyOS's own reads. The line Linux at
`Ubuntu-6.8.0-142.142` prints on each machine is the goal, compared in
evidence; a clause ToyOS's line lacks is a finding against this issue, never a
test's expected value.
