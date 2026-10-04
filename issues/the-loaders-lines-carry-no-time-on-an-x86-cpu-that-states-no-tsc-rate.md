---
status: open
kind: defect
opened: 2026-10-04
---

# The loader's lines carry no time on an x86 CPU that states no TSC rate

Every line of the log opens with its time since the CPU's counter last started
(`toyos_abi::log::Head`). The loader converts the counter at the rate CPUID
leaves 15H/16H state (`bootloader/src/stamp.rs`, `toyos_tsc::stated_hz`), and
the kernel does the same for its records until it has measured the TSC against
the HPET (`kernel/src/clock.rs`'s `stamp`). A CPU that states neither leaf gets
`--.---` (`toyos_abi::log::UNTIMED`) in place of the time on every loader line,
one `Loader clock: this CPU states no counter rate` line, and `--.---` on every
kernel record before `clock: HPET at …` — `boot:`, `ACPI:`, the interrupt
controller's and the per-CPU area's among them. The records after it carry
their time from the same zero.

Every AMD part is one. InstLatx64's recorded CPUID dumps
(`github.com/InstLatx64/InstLatx64`, `AuthenticAMD/`) read leaf 0's EAX, the
largest standard leaf, as `00000010` on Zen 2 (`0870F10_K17_Matisse_CPUID`),
Zen 3+ (`0A40F41_K19_Rembrandt_01_CPUID`), Zen 4 (`0A60F12_K19_Raphael_01_CPUID`)
and Zen 5 (`0B00F21_K20_Turin_01_CPUID`, `0B20F40_K20_StrixPoint_06_CPUID`,
`0B40F40_K20_GraniteRidge_02_CPUID`): neither leaf exists there. QEMU's
`qemu64` states neither either, and under the `guest / suite` check's
`-cpu host` whether a line is timed is the runner's CPU's to decide.

A second rate source is the fix, and the choice of one is open: the
`EFI_TIMESTAMP_PROTOCOL`'s stated frequency, the ACPI PM timer or the HPET, each
a calibration the loader then pays for before its first timed line.

Until then no guest test sees a time on the screen: `screen_loader_clears`
(`tests/toyos.rs`) measures the screen's first line, head and all, and on every
x86 guest that head reads `--.---`. The fix is where that test starts asserting
a time in it.

Exit: a loader line and a kernel record written before `clock: HPET at …` carry
their time on an x86-64 CPU that states neither leaf 15H's ratio nor leaf 16H's
base frequency, and `screen_loader_clears` asserts that line's time on the
screen.
