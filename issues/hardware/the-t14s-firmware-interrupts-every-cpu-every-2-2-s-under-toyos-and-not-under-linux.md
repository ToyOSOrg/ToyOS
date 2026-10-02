---
status: open
kind: defect
opened: 2026-10-03
---

# The T14's firmware interrupts every CPU every 2.2 s under ToyOS, and not under Linux

On LENOVO 20W0003AMZ, BIOS N34ET71W (1.71), one boot of `main` at `c59e09ed6`
with a throwaway instrument that reads `MSR_SMI_COUNT` (MSR 0x34) on each CPU
as each interrupts-off window opens and closes read the count from 1.20 s to
38.79 s into the boot, `test_rs_ring_park_herd` running forty times in it. Its
lines are quoted on the pull request that landed this text.

- **It moved 17 times, from 4824 to 4841, on all eight CPUs alike**: at each of
  the boot's 42 reports the eight read the same value.
- **About every 2.2 s.** Fifteen of the steps fall inside a window the
  instrument printed, and the windows that caught consecutive steps close
  2,215,327,897 to 2,221,576,672 ns apart, median 2,216,534,952.
- **Each step stops every CPU for about 4.5 ms.** The 25 interrupts-off
  windows a step falls in read 4,529,514 to 4,764,935 ns: sixteen open in a
  job's syscall or interrupt, thirteen of them `SYS_SYSINFO`, and nine in a
  pass leaving idle. None of those sections reads 2 ms where no step falls.
  Each window has one sample of its CPU, 4.52 to 4.70 ms after it opened,
  where a CPU the instrument holds masked on purpose is sampled every 2 ms. At
  9.925 s all eight CPUs carry one at once. Two CPUs waiting on a lock across a
  step went 4,503,694 and 4,554,675 ns between two turns of their own spin.
- **Under Linux it does not move.** On the same machine under Ubuntu's
  `6.8.0-142-generic`, `perf stat -a -A -e msr/smi/` read 0 on every CPU over
  120.378 s.

What differs, as far as anything has read:

- ToyOS performs no ACPI enable handshake: `rg -n -i
  'smi_cmd|acpi_enable|sci_en|smi_en|0xb2\b' kernel/src bootloader/src` finds
  only the xHCI legacy handoff's own `USBLEGCTLSTS` enables
  (`kernel/src/drivers/xhci/legacy.rs`), and nothing reads `PM1a_CNT.SCI_EN`
  or the chipset's `SMI_EN`. Both controllers' `USBLEGCTLSTS` read
  `0xe0000000`, no enable set, before the handoff wrote it.
- Under that Linux the orchestrator reports the FADT's `SMI_CMD=0xb2` and
  `ACPI_ENABLE=0xf0`, `PM1a_CNT.SCI_EN=1` and `SMI_EN=0x10002033`. The output
  of those reads was not kept.
- Under ToyOS neither `SCI_EN` nor `SMI_EN` has been read.

The cause is not known. The `mask-windows` kernel prints such a window as its
CPU's own: the lines of 4,720,498 and 4,725,822 ns in #649's boots
(`issues/kernel/cpu0-holds-interrupts-and-preemption-off-for-6-5-ms-on-the-t14.md`,
`issues/kernel/a-process-lengthens-an-interrupts-off-walk-by-the-threads-it-parks-on-one-ring.md`)
are this by reading, those kernels not reading the count.

**Exit**: `MSR_SMI_COUNT` does not move during a boot on the T14, read by a
row.
