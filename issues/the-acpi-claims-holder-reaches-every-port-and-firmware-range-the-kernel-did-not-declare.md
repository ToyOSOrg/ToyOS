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

**Exit**: an access is passed only inside a region the machine's loaded tables
define, checked by something other than the holder; or the owner rules the
address-only bound is the one ToyOS keeps.
