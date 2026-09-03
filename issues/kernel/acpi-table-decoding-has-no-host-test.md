---
status: open
kind: defect
opened: 2026-09-01
---

# Nothing reproduces an ACPI table decode off a booted machine

`kernel/src/drivers/acpi.rs` reads firmware-supplied, untrusted bytes and its
own header says so — *"no panic on any input path, every failure is a
[`TableError`]"*. Nothing checks that claim anywhere but on a running guest.
There is no `toyos-acpi`, no fixture, and no crafted-input corpus; the whole of
what exercises the decoder is a boot that happens to work, plus
`kernel/src/iommu/vtd/dmar.rs`, which parses DMAR through the same
`Table::open` and is reached only on a machine that has one. Measured
2026-09-01, `git grep -lie acpi` over `*.rs` outside `rust/`: fourteen files,
eleven of them kernel code, one the bootloader, and two in `tests/` ---
`tests/toyos.rs` in a comment, and `tests/common/wallclock.rs:479`, which waits
on a kernel line an ACPI decode produced (*"ACPI: the FADT names no RTC century
register"*). That last one is the nearest thing to coverage that exists, and it
reads a log line rather than a decode. Case-sensitively the same grep names
twelve, which is the figure this entry carried until the sentence was checked.

What is decoded there is what SMP is built out of — `arch/smp.rs` takes its APIC
IDs from `parse_madt`, `drivers/ioapic.rs` its windows and overrides — so a
decoder that reads one field wrong on an unusual firmware is a machine that
brings up the wrong set of CPUs.

**The blocker is the reader, and it is one shape.** Every read goes through
`Mapped`, a `DirectMap` over a physical address with two `unsafe` accessors
(`field<T>` unaligned, `byte` volatile). A pure crate wants the same decode over
a byte reader the kernel implements — the `unsafe` stays in the kernel, the
decode becomes `forbid(unsafe_code)` and takes `&[u8]` fixtures. About half the
file moves: `find_table`, `Table`, `parse_madt`, `find_ecam_base`,
`find_hpet_base`, `iapc_boot_arch`, `rtc_century_register`. The other half is
machine action — `init_power` and `shutdown` write PM1 ports — and stays.

**Two things a taker needs.** The oracle is ACPI's own specification, quoted
in-tree at the field offsets, plus the tables QEMU publishes read back off a
boot as a differential fixture; a crafted-input corpus over lengths, checksums
and entry counts is the boundary half. And it is a change to the SMP boot path,
so the negative control is a guest suite on both arms, not a host one.

The record that sent this here is `issues/design-debt/what-is-owed-on-file-size.md`,
where it was the ninth of nine owner review notes. It is not a file-size case:
the file is 611 lines.
