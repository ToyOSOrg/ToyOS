---
status: open
kind: defect
opened: 2026-10-03
---

# The T14's firmware interrupts every CPU every 2.2 s under ToyOS

On LENOVO 20W0003AMZ, BIOS N34ET71W (1.71), one boot of `main` at `c59e09ed6`
with a throwaway instrument that reads `MSR_SMI_COUNT` (MSR 0x34) on each CPU
as each interrupts-off window opens and closes read the count from 1.20 s to
38.79 s into the boot, `test_rs_ring_park_herd` running forty times in it. Its
lines are quoted on #681 (comment 5962619169; the boot itself in comment
5962618149).

- **It moved 17 times, from 4824 to 4841, on all eight CPUs alike**: at each of
  the boot's 42 reports the eight read the same value.
- **About every 2.2 s.** Fifteen of the steps fall inside a window the
  instrument printed, and the windows that caught consecutive steps close
  2,215,327,897 to 2,221,576,672 ns apart, median 2,216,534,952.
- **Each step stops every CPU for about 4.5 ms.** The 25 interrupts-off
  windows a step falls in read 4,529,514 to 4,764,935 ns: sixteen open in a
  job's syscall or interrupt, thirteen of them `SYS_SYSINFO`, and nine in a
  pass leaving idle. None of those sections reads 2 ms where no step falls. At
  9.925 s all eight CPUs carry one at once. That the CPU stops is shown by two
  CPUs waiting on a lock across a step, which went 4,503,694 and 4,554,675 ns
  between two turns of their own spin.
- **Linux, in one condition, read none.** On the same machine under Ubuntu's
  `6.8.0-142-generic`, `perf stat -a -A -e msr/smi/` read 0 on every CPU over
  120.378 s beside `rtla timerlat top -q -d 2m --dma-latency 0`, which holds
  every CPU's C-states shallow and runs a 1 kHz timer on each. Linux idle has
  not been read.

What differs, as far as anything has read:

- ToyOS performs no ACPI enable handshake: `rg -n -i
  'smi_cmd|acpi_enable|sci_en|smi_en|0xb2\b' kernel/src bootloader/src` finds
  only the xHCI legacy handoff's own `USBLEGCTLSTS` enables
  (`kernel/src/drivers/xhci/legacy.rs`), and nothing reads `PM1a_CNT.SCI_EN`
  or the chipset's `SMI_EN`. Both controllers' `USBLEGCTLSTS` read
  `0xe0000000`, no enable set, before the handoff wrote it.
- Under that Linux, read as root from `/sys/firmware/acpi/tables/FACP` and
  `/dev/port` with nothing written (comment 5962768253): the FADT's
  `SMI_CMD=0xb2` and `ACPI_ENABLE=0xf0`; `PM1a_CNT` (port 0x1804) `0x0001`,
  `SCI_EN` set; `SMI_EN` (port 0x1830) `0x10002033`, bits 0 (`GBL_SMI_EN`), 1
  (`EOS`), 4 (`SLP_SMI_EN`), 5 (`APMC_EN`), 13 (`TCO_EN`) and 28.
- Under ToyOS neither `SCI_EN` nor `SMI_EN` has been read.

The cause is not known. The `mask-windows` kernel prints such a window as its
CPU's own: cpu4's 4,720,498 ns in the sixth of #649's `mask_windows` boots at
`72f16e39a`, and cpu0's 4,725,822 in `649-r6/3-idle-halt-counted`
(`issues/kernel/a-process-lengthens-an-interrupts-off-walk-by-the-threads-it-parks-on-one-ring.md`),
are this by reading, those kernels not reading the count.

The ACPI enable is itself a write of `ACPI_ENABLE` to `SMI_CMD`, which raises
one firmware interrupt where `APMC_EN` is set, so a boot that tests it reads
the count over an interval after that write, never across it.

**Owner**: the T14 loop
(`issues/hardware/the-t14-boots-toyos-unattended.md`), whose jobs are the
measurements owed on hardware: it builds the row.

**Exit**: a T14 row reads `MSR_SMI_COUNT` on every CPU as init is spawned,
after every write the kernel's bring-up makes, and again at the stop's report,
and on every CPU the two agree.
