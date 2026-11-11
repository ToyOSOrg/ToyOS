---
status: open
kind: finding
opened: 2026-11-11
---

# A capability holder's refused claim or stop writes a record per call

A refused spawn writes no record (`kernel/src/loader/mod.rs`'s header), so no
program without a capability can have the kernel write one per call it is
refused. Two holders still can:

- a `SysCap` carrying the device right: each refused claim of a PCI function
  writes one (`pcidev::claim` in `kernel/src/pcidev/mod.rs`: `NOT HANDED OVER`,
  `cannot be claimed`, `has no … window`), and each granted one several; and
  the `acpi` claim's holder writes one per declaration of the root bridges'
  memory past the first answer (`pcidev::declare`: `after the first answer is
  refused`);
- a `SysCap` carrying the power right, on a machine with no power-off or no
  reset: each refused stop writes one (`shutdown_refused` and
  `reboot_refused` in `kernel/src/syscall/machine.rs`).

Both rights are a manifest's to grant, and the claim's records are what the
device judges read (`tests/toyos.rs`), so this is not the defect a refused
spawn was. Evidence: by reading; no test loops either.

Exit: a holder's refusals are counted and said once, or the judges that read
them read the claimer's own line, and `pcidev`'s header says which. Owner: the
syscall layer, `kernel/src/syscall/`.
