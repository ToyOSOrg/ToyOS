---
status: open
kind: defect
opened: 2026-10-12
---

# diskserver claims QEMU's NVMe by its PCI id and no other controller

Both shipped boot configs give diskserver one device, `pci:01:08:02/1b36:0010`
(`images/system.toml`, `images/console.toml`): the NVMe class with the vendor and device id of
QEMU's own `nvme` model, under Red Hat's vendor id. The supervisor matches the id exactly
(`toyos_manifest::device::PciMatch`, `toyos_supervisor::claims`), so on any other NVMe controller it
endows diskserver nothing, diskserver says `no NVMe controller this row names is on
this machine; serving no partition`, and DATA, LOG and BOOT are absent unless
the image is on a USB stick the kernel drives. The driver is not QEMU's:
`Controller::open` (`system/diskserver/src/nvme.rs`) takes any function whose
class code is `01:08:02`.

Read, not booted:

| | NVMe id | MSI-X table |
|---|---|---|
| QEMU `nvme` | `1b36:0010`, or `8086:5845` with `use-intel-id=on` | BAR 0, unless `msix-exclusive-bar=on` |
| VirtualBox | `80ee:4e56` | BAR 5 |
| bhyve | `fb5d:0a0a` | BAR 4 |

Cloud Hypervisor and crosvm offer no NVMe. Every controller on metal carries
its maker's id.

The identity every NVMe controller shares is its class code: base class 01h,
subclass 08h, programming interface 02h, the "NVM Express I/O controller" of
the PCI-SIG's *PCI Code and ID Assignment Specification*, which QEMU, VirtualBox
and bhyve each set. `issues/the-lan-is-not-yet-production-grade.md` plans the
same for NICs ("init binds drivers to hardware from a declared match table by
vendor, device and class").

Two constraints a class row meets that an id row does not: a machine with two
NVMe controllers matches twice, which refuses diskserver's start, naming both
(`toyos_supervisor::claims`); and a machine whose NVMe holds another operating system, as the T14's
holds Ubuntu, is handed to diskserver, which then serves only the partitions
its table and grants name (`system/diskserver/src/main.rs`).

**Owner**: whoever holds `issues/toyos-runs-in-any-open-source-hypervisor.md`.

**Exit**: a `devices` row names a PCI function by class code, which
`toyos_manifest::device::PciMatch::parse` takes and refuses malformed (#926); both
shipped configs name NVMe that way; and a guest boot whose controller is
QEMU's `nvme` with `use-intel-id=on` serves DATA through diskserver, which a
row naming `1b36:0010` instead reds. The configs the T14 boots keep their id
row, outside this exit, until the T14's NVMe holds no Ubuntu: a class row there
would hand diskserver the disk the metal workflow reboots through (the
orchestrator's ruling, #896, issuecomment-6131213771, ruling 4).
