---
status: open
kind: track
opened: 2026-09-29
---

# Each boot prints the vulnerabilities lines Linux prints

**Exit**: each proving machine's ToyOS boot prints, for each vulnerabilities
file, the line Linux at `Ubuntu-6.8.0-142.142` printed on that machine: the
T14's from
`issues/hardware/linuxs-readings-of-the-t14-and-the-tcg-model-are-not-committed.md`,
each EPYC guest's from its runner's capture. It omits only clauses about what
ToyOS does not run: `spectre_v1`'s swapgs barriers while it runs no `swapgs`,
the KVM and VM-exit clauses since it runs no guest, and `IBRS_FW` since the
kernel calls no firmware; `spec_store_bypass` reads "Mitigation: Speculative
Store Bypass disabled per program". **Mutation**: `ARCH_CAPABILITIES` read as
0 reds the T14. **Oracle**: Linux's lines.
