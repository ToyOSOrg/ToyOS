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

The other four things this row assumed are answered: the metal loop's third run
read `8086:a0a3 TCO at 0x400` off the log partition, armed without a refusal,
and came back to Ubuntu on its own, which is `toyos-tco`'s Tiger Lake row.

**Exit condition**: the kernel's own `watchdog:` line, off the log partition, on
the boot *after* a reset — so it needs a metal run whose boot reaches `logd`,
which run 3's did not.
