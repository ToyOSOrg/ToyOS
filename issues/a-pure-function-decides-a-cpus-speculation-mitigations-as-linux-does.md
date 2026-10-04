---
status: assigned
kind: track
opened: 2026-09-29
---

# A pure function decides a CPU's speculation mitigations as Linux does

From a CPU's facts to the line Linux at `Ubuntu-6.8.0-142.142` prints in each
`/sys/devices/system/cpu/vulnerabilities/*` file and the mitigation it
selects, built by the kernel and by a host test. Pull request #602 holds it.

**Exit**: each committed fixture's facts give its lines: the T14's and the TCG
model's from
`issues/linuxs-readings-of-the-t14-and-the-tcg-model-lack-reads-owed-before-the-t14s-wipe.md`,
and each EPYC guest's once its runner is captured. **Mutation**: `GDS`
deleted from `cpu_vuln_blacklist`'s TIGERLAKE_L row reds the T14's fixture,
and `SRSO` deleted from its family 0x19 row reds a family-0x19 EPYC guest's.
**Oracle**: those lines, and Linux's `cpu_vuln_whitelist` and
`cpu_vuln_blacklist` row for row
(`arch/x86/kernel/cpu/common.c:1182-1244,1284-1344`).
