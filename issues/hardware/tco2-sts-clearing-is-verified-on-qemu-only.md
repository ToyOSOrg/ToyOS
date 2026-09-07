---
status: open
kind: tooling
opened: 2026-09-06
---

# Whether writing `TCO2_STS` back clears it is verified on QEMU only

`kernel/src/drivers/watchdog.rs`'s `arm` writes `TCO_SECOND_TO_STS` and
`TCO_BOOT_STS` back so that a reset is reported by the boot after it and not by
every boot after it. That the write clears them is established for QEMU, whose
store masks both bits out (`hw/acpi/ich9_tco.c:167`). Whether a Tiger Lake-LP
PCH's are write-one-to-clear, and what the same word does to the rest of that
register there, is unread. The symptom is a T14 that keeps reporting a reset it
already reported.

The other things this row assumed are answered in part: the metal loop's third
run read `8086:a0a3 TCO at 0x400` off the log partition and armed without a
refusal, which is `toyos-tco`'s Tiger Lake row. **It did not come back on its
own** — the owner power-cycled it by hand (owner ruling, 2026-09-06). No run
has yet seen this PCH reset itself, which is
`issues/hardware/an-armed-tco-has-never-reset-the-t14.md`; until that is
answered this register's clearing cannot be exercised on the machine at all.

**Exit condition**: a T14 boot that resets itself on an armed timer, and the
kernel's own `watchdog:` line off the log partition on the boot *after* it — so
it needs a metal run whose boot reaches `logd`, which no run's has.
