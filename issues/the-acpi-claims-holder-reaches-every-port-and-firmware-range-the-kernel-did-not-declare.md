---
status: open
kind: defect
opened: 2026-10-07
---

# The `acpi` claim's holder reaches every port and firmware range the kernel did not declare

The kernel reads and writes for the holder of the `acpi` claim whatever the
firmware's AML names (`SYS_ACPI`, `kernel/src/arch/x86_64/acpi_mode.rs`), and
decides each access by what its address is
(`toyos-userbound/src/firmware.rs`). What that decision passes is wider than
what any one machine's AML needs, because nothing tells the kernel which
regions a machine's tables define until the holder's interpreter has loaded
them:

- **Every port the kernel did not declare and no other row names**, both
  ways. An I/O BAR of a PCI function is such a port, a kernel driver's and a
  claim holder's alike: the kernel records the memory BARs it decodes and no
  I/O BAR. So is every chipset register the firmware never mentioned.
- **All ACPI NVS and reserved memory**, both ways, outside the FACS and outside
  every page a device the kernel knows of decodes in (a window it mapped, a
  function's memory BAR): the firmware's own state, which its SMI handlers
  read and trust, and any device register firmware typed reserved that is no
  BAR and that the kernel maps nothing of.
- **A function's configuration space, to read**, anywhere the MCFG's window
  reaches. No configuration write is made: each is refused `ConfigWrite`
  until a machine's AML makes one, and the arm that then passes it is designed
  against that write.

What the kernel's declarations do not follow:

- **A declared block that moves.** `PM1a_CNT`, the TCO block and `SMI_CMD` are
  declared by the port numbers the tables gave at boot. The registers that
  place those blocks are in configuration space, which the holder cannot
  write, but a chipset reaches them a second way wherever it mirrors them in
  memory firmware typed reserved or behind an undeclared index and data pair:
  a write there that moved a block would leave its registers on ports nothing
  declared, read-only no longer.
- **A port the firmware traps.** An access to an undeclared port that raises an
  SMI is a stay in SMM for as long as the firmware's handler takes, made with
  the mediation's spinlock held; the kernel neither bounds it nor counts it,
  where it counts the SMI its own `SMI_CMD` write raises.
- **A machine whose FADT names no `SMI_CMD`.** Nothing is declared there, so
  the chipset's software-SMI port is a port like any other and the holder
  writes it; the write is refused by name only where the FADT names the port.

So a bug in `/system/bin/acpiserver`, or AML it runs, can reach those; the
kernel bounds where, and not what. The owner's ruling on the server reading
and writing firmware-owned memory, ports and PCI configuration space through a
kernel-checked call, as the orchestrator's brief for this slice records it:
"just do it properly the first time i dont know what it is but no workarounds
as always and do the work whatever that is". That the surface so built is
address-bounded and no narrower is this change's design, not his ruling, and
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md` owns
this beside
`issues/the-acpi-servers-holder-drives-the-embedded-controller-unfiltered.md`.

What the T14's tables need was measured without the machine, on a model whose
reads answer zero: at load, reads of six ACPI NVS pages and of four functions'
configuration space, and no write; across its initialisation and query
methods, memory writes in three pages, port writes to two ports nothing
declared and to `SMI_CMD`, and no configuration write. Which UEFI types the
real bases fall in is unread.

Where that machine's devices decode is read: by its firmware's map as two
recorded boots give it (ToyOS's own, whose `pcidev` lists what the map, the
BARs and the bridges' forwarded ranges leave free, and Linux's print of the
same map), none of the 21 memory BARs of its 24 functions is at an address the
map lists, 16 of them past the end of the direct map; nor, by Linux's print,
is the I/O APIC, the HPET or any of the four DMA remapping units. There a BAR
and a kernel-driven window answered `MemoryType` or `Unmapped` before the
kernel's record refused them, and the record is what refuses them on a machine
whose firmware lists such a range as reserved.

**The holder reads runtime-services data whole** (the orchestrator's ruling,
not the owner's). The T14's firmware keeps the FADT and every definition
block its XSDT lists in `EfiRuntimeServicesData`, read on that machine: the
server's first load there was refused `MemoryType` in type 6 for all 30 such
entries, where UEFI 2.10 §2.3.4 has tables in ACPI reclaim or NVS memory. A
read there passes now, as in the other three firmware types; a write stays
refused `TableWrite` until a machine's AML is measured making one, and
`EfiRuntimeServicesCode` stays refused both ways. No memory the kernel hands
out has the type, so no kernel or process memory is reached by it. What it
costs is that the holder reads whatever else a firmware keeps in
runtime-services data, which nothing here has listed: its variable store's
working copy and its services' own state are candidates, unread.

**The holder reads a register at an address the firmware's map does not
list** (the orchestrator's ruling, not the owner's). With its tables readable,
the T14's load was refused one table of 14 for one byte read at an address
the map lists nowhere, below 4 GiB, outside the ECAM window, in no page the
kernel drives and no function's BAR: by its place, the chipset's own register
space. Such a read passes now where the address is inside the direct map and
the boot processor's range registers, as the kernel read them at boot, type
it uncacheable (`acpi_mode::uncached`, decided by `kernel::mtrr`); a write
stays refused, range registers that are off type nothing, so there every
such read is refused, and on a machine where any CPU's range registers are
on and are not the boot processor's every such read is refused
`RangeRegistersDiffer`.

What keeps kernel and process memory out of that read is not the range
registers. The allocator hands out only memory the firmware's map lists as
usable (`toyos_bootmap::is_usable_type`), so memory the map does not list
holds nothing ToyOS put there, whatever it is; and an access is refused where
any usable range of the map holds a byte of it, whichever range lists that
byte first. Three things are not checked:

- **The range registers are not the effective type everywhere.** A processor
  that types RAM from 4 GiB to its top of memory write-back by a
  configuration bit outside the range registers (AMD's `SYSCFG` and `TOM2`)
  answers the registers' default type for that RAM, which is uncacheable on
  such firmware: an unlisted range of RAM above 4 GiB inside the direct map
  is read there as a register would be. Nothing reads that bit. What such a
  read reaches is RAM the map left out, which the allocator never handed
  out.
- **A CPU whose range registers are off reads a register uncached, and one
  whose are on and differ stops the read; nothing makes them the boot
  processor's.** The read is made on whichever CPU the call runs on. Each
  CPU reads its own range registers as it comes up and the kernel says how
  they stand beside the boot processor's (`arch::mtrr::compare`, decided by
  `kernel::mtrr::beside`): the same words; off, where every read that CPU
  makes is uncached by the architecture, so a register is still read once;
  or on and not the same, where what the boot processor's say of an address
  is not known of that CPU's read, and every unlisted read on the machine is
  refused `RangeRegistersDiffer`. Measured, three readings of the kernel's
  lines: on the T14's `acpi_tables_loaded` boot the boot processor has 10
  variable pairs and each of its seven other CPUs has the boot processor's
  words; on a QEMU 11.1.1 q35 guest under KVM the boot processor reads
  `IA32_MTRR_DEF_TYPE` as 0xc06 with 8 variable pairs and the second CPU has
  the boot processor's words; on a QEMU 11.1.1 q35 guest under TCG the boot
  processor reads the same and the second CPU reads `IA32_MTRR_DEF_TYPE` as
  0, off. So the on-and-different arm has run on no machine, and only TCG's
  second CPU is not the same. Firmware is to leave them the
  same (Intel SDM Vol. 3A, "MTRR Considerations in MP Systems"), and a
  kernel that programmed every other CPU's from the boot processor's would
  make them so and the refusal unreachable; this kernel programs no range
  register. Two things are open:
  - **Who owns the other CPUs' range registers**, firmware as now or the
    kernel. Owner: the owner, whose decision it is. **Exit**: he rules, and
    the kernel either programs them and asserts them as it does a control
    register, or this bullet records that it never will.
  - **Why the second CPU of the guest under TCG has them off is unread**:
    whether that guest's firmware programs only the boot processor's, or the
    emulation resets them when the CPU is started, where under KVM the same
    QEMU version leaves them the same. Whether the two guests ran the same
    firmware build is unread too. Owner: the stage,
    `issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`.
    **Exit**: the firmware's source or the emulator's read for that CPU,
    and this bullet says which of the two it is.
- **The fixed range registers are not read**, so an unlisted address below
  1 MiB is refused whole, a register there included
  (`firmware::FIXED_RANGE_END`).
- **A read of a register can have an effect in the device** — a status bit
  cleared, a FIFO advanced — that the kernel cannot know: it bounds where
  the holder reads and not what reading does there. On the T14 one such read
  is measured, at load, in one page; what the initialisation and query
  methods read there is unread.

**Exit**: an access is passed only inside a region the machine's loaded tables
define, checked by something other than the holder; or the owner rules the
address-only bound is the one ToyOS keeps.
