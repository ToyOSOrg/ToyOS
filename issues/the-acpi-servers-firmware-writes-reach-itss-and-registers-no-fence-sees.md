---
status: open
kind: defect
opened: 2026-10-13
---

# The ACPI server's firmware writes reach ITSS and registers no fence sees

The kernel makes the firmware's AML's reads and writes for the holder of the
`acpi` claim, `/system/bin/acpiserver`, unless a fence refuses one
(`toyos_userbound::firmware`, `kernel/src/arch/x86_64/acpi_mode.rs`). The
server is trusted platform code, granted by name in the signed image's
manifest, and each fence is a bug fence kept where it is cheap. These are
what the fences do not see: a bug in the server or the AML it runs reaches
them.

- **The P2SB sideband, the ITSS among it** (Intel 631119-007 Table 109, port
  C4h): the 8259 and I/O APIC routing and polarity. Fencing it needs
  SBREG_BAR, which the T14's firmware hides behind `P2SBC.HIDE` (631120-002
  §3.1.22): a configuration write to unhide the bridge, open to an SMI taken
  between it and the restore. Ruled unfenced on cost (orchestrator,
  2026-10-13, #906).
- **AMD's SMN behind 00:00.0's 0xB8 and 0xBC.** No AMD document held maps
  it on Family 17h Model 60h (PPR 55922-A1); on sibling parts it reaches the
  IOMMU's, the data fabric's and the memory controllers' registers.
- **An unheld function from 40h**: its MSI and MSI-X capabilities, its PM
  D-state, and in extended space SR-IOV and resizable BAR, which move decode
  the kernel did not record.
- **The PCH's eSPI controller and SMBus function from 40h, but what places a
  declared block**: on the 500 Series, 00:1F.0's generic I/O and memory
  ranges (`ESPI_LGIR1`–`4` 84h–93h, `ESPI_LGMR` 98h), its BIOS decode enable
  and BIOS control (D8h, DCh) (631120-002 §2.1), and 00:1F.4's host
  configuration `HCFG` (40h) and power gating `SMBSM` (80h) (§6.1).
  `ESPI_IOD_IOE` and the TCO base and control are fenced.
- **Registers at an address the firmware's map lists nowhere**, below every
  BAR and driven window: the PMC's and the FCH's power gating of devices a
  driver holds, and the SPI flash controller's registers.
- **A GPIO output pad wired to a held device's line**, until a driver holds
  the GPIO controller, whose window is then a claimed one.
- **An Intel host bridge 631122 does not cover**: its PL1 and PL2, interrupt
  redirection, VT-d unit bases and address map pass there.
- **A port the kernel did not declare and no row names**, both ways: an I/O
  BAR of a PCI function, a kernel driver's or a claim holder's, since the
  kernel records memory BARs only; and a chipset register no table names.
  A declared block moved through an undeclared index and data pair leaves
  its registers on ports nothing declared.
- **A port the firmware traps**: an SMI the access raises is a stay in SMM,
  made with the mediation's lock held, which the kernel neither bounds nor
  counts; and on a machine whose FADT names no `SMI_CMD`, the chipset's
  command port is a port like any other
  (`issues/a-firmware-call-does-what-its-handler-chooses-and-the-kernel-bounds-only-the-call.md`).
- **The range registers are not the effective type everywhere.** A
  processor that types RAM above 4 GiB write-back outside them (AMD's
  `SYSCFG` and `TOM2`) answers their default type for it, uncacheable on
  such firmware: unlisted RAM above 4 GiB is accessed there as a register.
  No usable range holds it, so the allocator never handed it out.
- **Nothing makes every CPU's range registers the boot processor's.** Where
  one differs every unlisted access is refused `RangeRegistersDiffer`; on
  QEMU 11.1.1 under TCG the second CPU reads them off, which every one of
  its accesses then makes uncached. Who owns the other CPUs' range
  registers, firmware or the kernel, is the owner's to rule; why that
  guest's second CPU has them off is unread.
- **A register's read can act in the device**, a status bit cleared or a
  FIFO advanced: the kernel bounds where the holder accesses, not what the
  access does there.
- **A claim minted between a decision and its access** is one the access was
  made a moment before: the records are read under their own locks and let
  go before the access.
- **The firmware locking 5F50h or 5F54h until reset**, which the T14's
  `CTCN` writes and the kernel passes.

Owner: the AML track
(`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`).

## Exit condition

Each item is fenced once fencing it is cheap, or the orchestrator rules it
a residual of the trust model and moves it to the policy's module header;
and the owner rules who owns the other CPUs' range registers, the kernel
then programming and asserting them, or this issue recording that it never
will.
