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
5968784527).

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

**The cause, measured, is the firmware's legacy mode.** A scout on `main` at
`73282fa93` (comment 5966817345) booted the T14 twice, each boot 30 s idle and
then 30 s with a spinning thread on every CPU:

- With nothing written, `PM1a_CNT` read `0x0000`, `SCI_EN` clear, from 0.156 s
  to 61.226 s, and `SMI_EN` read `0x10002033`, Linux's value. The count rose
  by 14 in the idle 30 s and 13 in the busy 30 s, on every CPU alike.
- With `ACPI_ENABLE` written to `SMI_CMD` at 0.156 s, `SCI_EN` read set
  2,129,279 ns later. The write moved the writing CPU's count from 4817 to
  4818, and every CPU then read 4818 at every report through 61.233 s. The
  SCI's line, GSI 9, stayed masked: nothing in ToyOS handles an SCI. Two GPEs
  moved after the switch, in that boot's `kernel.log` lines quoted in comment
  5966982752 (the patch, comment 5966982939, prints each GPE register byte by
  byte, lowest address first):
  - the firmware's enable set GPE 9's enable bit: `gpe0_en` byte 1 reads `00`
    at line 121, before the write, and `02` at line 123, after it;
  - GPE 110's status rose after the switch and stayed set, served by nothing:
    `gpe0_sts` byte 13 reads `00` at line 123 and `40` at every report from
    line 298 through line 759. The boot with nothing written shows it in none
    of its 25 reports (comment 5966982609).

The `mask-windows` kernel prints such a window as its CPU's own: cpu4's
4,720,498 ns in the sixth of #649's `mask_windows` boots at `72f16e39a`, and cpu0's 4,725,822 in `649-r6/3-idle-halt-counted`
(`issues/a-process-lengthens-an-interrupts-off-walk-by-the-threads-it-parks-on-one-ring.md`),
are this by reading, those kernels not reading the count.

**Owner**: stage 1 of
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`,
for the fix and for the reading: its exit holds the count flat on the T14 over
this exit's interval, so the stage builds the row, and it reads the count
through the general counters ("General counters", owner, 2026-10-03,
`issues/toyos-explains-itself.md`).

**Exit**: a T14 row reads `MSR_SMI_COUNT` on every CPU after init is spawned
and after the boot's last write to `SMI_CMD`, whoever makes it, and again at
the stop's report at least 4.444 s later, two of the longest period read, and
on every CPU the two agree. The interval opens after that write because the
ACPI enable is one, a write of `ACPI_ENABLE` to `SMI_CMD`, and raises one
firmware interrupt where `APMC_EN` is set.
