---
status: open
kind: defect
opened: 2026-09-08
---

# A 32-bit BAR cannot be handed to a process, because nothing here reads the host bridge's aperture

`kernel/src/pcidev`'s window allocator places a claimed function's BARs on
2 MiB pages **above everything firmware described**. In 64 bits that is always
possible. Below 4 GiB it never is: the platform's fixed MMIO sits at
`0xFEC00000` and the UEFI map reaches it, so `window(narrow_end, PLATFORM_MMIO)`
answers `0x0..0x0` on every machine. Read off the ThinkPad T14, run 29:

```
pcidev: 24 functions; a 32-bit window comes from 0x0..0x0, a 64-bit one from 0x603dc00000..0x6040c00000
pcidev: PCI 00:1f.6 NOT HANDED OVER — this machine has no 2 MiB-aligned address space above what firmware assigned to put a BAR in
```

That function is the bench's own NIC, and its BAR is 32-bit
(`bar0=0xbcf00000`). So the cable this project's test bench answers on cannot be
driven by a process at all until this is built.

**Leaving the BAR where firmware put it is not the fix.** The kernel maps 2 MiB
pages, and on this machine the I219's BAR shares its page with the internal
NVMe's (`0xbcf00000` and `0xbce00000` are both inside `0xbce00000..0xbd000000`):
handing it over unmoved would put a disk controller's registers inside a
network daemon's mapping. `alone_in_its_page` is the assertion that says so, and
the refusal is right.

**What is missing is a free run, and what is missing to find one is ACPI.** A
32-bit window is a run *between* things rather than a span above them, and three
of the four things it has to miss are readable already — the firmware map, the
BARs this bus assigned, and every range a PCI-to-PCI bridge forwards to a
secondary bus (`toyos_pci::bridge`, and `survey_low_space` prints all three on
the machine where the window comes out empty). The fourth is the host bridge's
own aperture: which addresses below 4 GiB the root complex decodes and forwards
to PCI at all. That is the `_CRS` of the `PNP0A08` device, an AML method, and
this kernel runs no AML. An address outside the aperture is not free space — it
is unrouted, and a read of it answers ones, which `Refusal::Dead` cannot tell
from a device that is simply not there.

Linux's own answer is the same one: `acpi_pci_probe_root_resources` reads
`_CRS`, and the per-chipset fallbacks it keeps are quirks for firmware that gets
`_CRS` wrong, not an alternative to it. Reading a host bridge register such as
Intel's `TOLUD` instead would be chipset-specific and is not a road this project
takes.

So the work is one of:

- an AML interpreter far enough to evaluate `_CRS` on the host bridge, which is
  a large thing to want for one method; or
- a 4 KiB mapping for a claimed BAR, which removes the need to move a BAR whose
  page is shared and is a change in `mm` rather than here; or
- the owner ruling that some other source of the aperture is admissible.

The survey is committed and prints the candidates; nothing hands one out. The
line it ends with says why:

```
pcidev: a run above is a candidate and not a claim — what says whether an address below 4 GiB
reaches this bus at all is the host bridge's own aperture, which is ACPI's `_CRS`, and this
kernel runs no AML
```
