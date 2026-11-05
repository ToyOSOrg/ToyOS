---
status: open
kind: track
opened: 2026-11-05
---

# A crash record survives a reset outside RAM

The loader asks the firmware to overwrite memory before every next boot
(`loader/src/overwrite.rs`), so nothing a boot leaves in RAM reaches the
next one. A kernel panic or fault on a machine with no serial port is read
only off its panel, and the boot deadline and the hard-lockup detector reset
the machine with no record at all. A record has to cross the reset in
storage the wipe does not touch.

What went with the black-box page and comes back with this track: the
metal rows that judged a sealed record (`boot_deadline_ends_a_wedge`,
`hard_lockup_ends_a_deaf_cpu`, `watchdog_resets` and the
`wedge-before-reset` and `hard-lockup-probe` arms they staged), the
records a stop writes after `logkeeper`'s last flush (its stop record, the
machine's census and the panel's, which `irq_census_conservation` and the
boot's panel timings read).

The routes other systems take, none chosen:

| Route | What persists it | What it costs ToyOS |
|---|---|---|
| ACPI ERST (ACPI 6.5 §18.5) | firmware's error-record store, written through the table's serialization actions | a kernel path that executes those actions at a panic; whether the T14 and the AMD bench publish an ERST is unread |
| UEFI variable (Linux's efi-pstore, macOS's panic log in NVRAM) | a non-volatile variable | a runtime `SetVariable` at a panic, which the rule that the loader makes every UEFI call forbids; variable-store size and wear |
| Disk (Windows' bugcheck dump) | a reserved area of the boot disk | a polled write from the panicking kernel to the disk, which no ToyOS kernel has; the drivers are userland's |

Each one is written at the panic, by the kernel alone, and read and
cleared by the next boot into `/log`.

Owner: the route is the owner's to choose; the kernel's panic path implements it.

**Exit**: on the T14 and the AMD bench, with the memory-overwrite request in
force, a staged kernel panic's report and a staged wedge's deadline record
are in the next boot's `/log`, and a boot that stopped cleanly reads no
record.
