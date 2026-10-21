---
status: open
kind: defect
opened: 2026-10-12
---

# The NVMe driver drives QEMU's NVMe by its PCI id and no other controller

Both shipped boot configs give the `nvme` driver's `drives` row one matcher,
`pci:01:08:02/1b36:0010` (`images/system.toml`, `images/console.toml`): the NVMe class with the
vendor and device id of QEMU's own `nvme` model, under Red Hat's vendor id. The supervisor matches
the id exactly (`toyos_manifest::device::PciMatch`, `toyos_supervisor::drives`), so on any other
NVMe controller no driver starts, and DATA, LOG and BOOT are absent unless the image is on a USB
stick the kernel drives. The driver is not QEMU's: `Controller::open` (`system/nvme/src/nvme.rs`)
takes any function whose class code is `01:08:02`.

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

**A compromise #962 keeps**, accepted by the orchestrator there: the `nvme`
row names QEMU's id in every image, shipped and test alike, so that no metal
boot drives a bench's own NVMe disk — the T14's holds the Ubuntu the metal
loop reboots through after every boot (`tests/metal/src/lib.rs`). That is a
machine with an NVMe disk and no ToyOS driver for it.

**What a shipped image should drive**: every NVMe controller, by class code,
whatever machine it boots on — QEMU, another hypervisor, metal.

**What keeps another system's disk unwritten under a class row**, by reading
and not measured: the driver reads its table and resets and enables the
controller, and writes nothing outside a session. A session opens only what a
grant admits (`system/diskserver/src/main.rs`), and the supervisor mints
grants for the loader's LOG and BOOT GUIDs, for the `TOYOS-DATA` type, and for
`update` on the slots of the running ROOT's disk (`storage_endowment`,
`Supervisor::slot_grant`, `system/supervisor/src/main.rs`), none of which an
Ubuntu partition carries. Whether Ubuntu boots off a controller ToyOS drove
and reset is not measured.

**Owner**: the orchestrator, which dispatches it; the hypervisor track
(`issues/toyos-runs-in-any-open-source-hypervisor.md`) waits on it.

**Exit**: every image's `nvme` row names NVMe by class code, which
`toyos_manifest::device::PciMatch::parse` takes (#926); a guest boot whose
controller is QEMU's `nvme` with `use-intel-id=on` serves DATA through the block
service, which a row naming `1b36:0010` instead reds; a supervisor host test
holds that every block grant it mints names a loader-named GUID, a ToyOS
partition type, or the running ROOT disk's slots, red on a grant by any other
type; and a T14 row whose readback shows the nvme driver offering the T14's
disk with no session opened on any of its partitions, read after the metal
loop's own reboot into Ubuntu off that disk.
