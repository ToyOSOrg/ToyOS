---
status: open
kind: defect
opened: 2026-10-11
---

# acpi_press_across_a_lock_wait went red under load

`cargo test`, the whole guest suite, went red on one test at
`wt/toyos-lan` 777c96b2b: `acpi_press_across_a_lock_wait: "acpi: the ACPI
row: PM1a events " never reached the the boot`, 9 s after it started, with 60
of 61 green. The guest log is posted whole on pull request #861.

The host was a 14-core macOS machine. Two `--metal-readback` image builds ran
beside the suite, and the load average read 34.6 / 26.7 / 18.8 a minute after
the red. The boot it reads, `tests/images/acpicase.toml`, starts no netstack
and claims only the NVMe controller. That branch changes netstack, its TCP
stack and its I219 driver, so the change does not reach the boot. Nobody has
run the test again, because the tree's rule is that a red is not re-run to
green.

**Exit**: the cause is found and fixed, and the fix is shown by a run under
at least this load, or the test is deleted with this file recording the
commit that restores it.
