---
status: open
kind: defect
opened: 2026-10-10
---

# An ECAM window off the 2 MiB grain is not enumerated on x86

x86-64's `map_mmio` maps whole 2 MiB pages (`paging::MMIO_GRAIN`,
`kernel/src/arch/x86_64/paging.rs`), so `EcamWindow::mappable`
(`toyos-acpi/src/mcfg.rs`) refuses a window whose first decoded byte or length
is off that grain, `AllocationRefused::OffGrain`, rather than map the
neighbouring megabyte uncached with it. A well-formed MCFG window that starts
or ends on an odd bus, or whose start bus sits on an odd megabyte, is then
never enumerated (`kernel/src/drivers/acpi.rs`, `ecam_windows`); where it is
the only window, the machine boots with no PCI function at all. AArch64 maps
at 4 KiB and takes every such window.

**Evidence:** the code, and `a_window_off_the_grain_is_refused_and_on_it_is_mapped`
in `toyos-acpi/tests/mcfg.rs`. No MCFG read so far is off the grain: q35's
decodes buses 0x00..=0xff, the T14's 0x00..=0x79 from 0xc0000000, and the target
laptop's 0x00..=0x3f, each an even number of megabytes from a 2 MiB boundary.

Owner: orchestrator. Exit: x86-64 maps an MMIO window exactly, to the 4 KiB
page, and a host test accepts a window of an odd bus count at x86-64's grain.
