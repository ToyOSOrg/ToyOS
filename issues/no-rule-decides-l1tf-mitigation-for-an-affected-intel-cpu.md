---
status: open
kind: defect
opened: 2026-09-29
---

# No rule decides L1TF's mitigation for an affected Intel CPU

Nothing in the kernel decides whether a CPU is affected by L1 Terminal Fault
or what mitigates it. Intel's L1TF guidance prescribes the mitigation for the
CPUs it lists as affected. No proving machine is known to be one.

**Exit**: the function of
`issues/a-pure-function-decides-a-cpus-speculation-mitigations-from-its-vendors-guidance.md`
decides L1TF's mitigation from Intel's L1TF guidance for every CPU Intel lists
as affected. A host test holds that decision to the document.
