---
status: assigned
kind: track
opened: 2026-10-13
---

# A device function is driven from its arrival to its end

Held by the orchestrator. #910 is stage 1.1 and #926 stage 1.2.

Today the kernel enumerates PCI once (`pcidev::publish`) and numbers each
function by its arrival. The supervisor mints a `[boot] start` row's claims
when it spawns it, and starts a `drives` row's process on each function it
reads free in the inventory, at boot and after each end of a process it
started. Nothing reaches userland when a device arrives or leaves.

**Ruled** (owner, 2026-10-13):

1. The kernel hands out only what the IOMMU can tell apart: PCI functions, ISA
   rows, platform devices. The supervisor brokers those from `system.toml`, at
   boot and whenever one arrives later, by moving the claim to a driver. A bus
   service (usbd, one per xHCI controller) owns everything behind its
   controller, keeps its device classes (HID, mass storage) inside its process,
   and speaks the client protocols directly (input, block). The supervisor
   never sees a USB device; the client port carries a per-device source and
   `gone`. The client-facing protocol is the unifying boundary, not the bus.
2. One driver process per kernel-handed function (diskserver per NVMe
   controller, a NIC driver per function, usbd per controller); one shared
   service per client-facing function (fileserver, netstack, the input owner,
   the compositor). The supervisor is the persistent waiter: a function at boot
   or arriving later is the same event answered by the same spawn; a leaf
   driver starts with its device and ends when it is `gone`. The 4-claim-slot
   limit and the process-count issue are defects to remove, not design inputs.
3. Removal invalidates the claim, which answers `Gone`; a returning device is
   a fresh arrival with a new handle. Identity (serial, GPT GUID) is the
   driver's and fileserver's business, never the kernel's. Short USB glitches
   stay inside usbd's port ladder.

**Ruled** (orchestrator, on #910):

- PCIe hot-plug is stage 2. Stage 1 is the ownership and event model.
- Physical PCI arrival has no metal witness until a bench has a hot-plug port
  whose functions the owner lets a driver claim. That comes through
  `issues/toyos-supports-thunderbolt-and-usb4.md`. USB arrival, the hot-plug
  both benches have, is usbd's and comes with stage 4.
- Native PCIe hot-plug only. A machine that offers only ACPI hot-plug is a gap
  this track records, not one it serves.
- The device set is a level-triggered snapshot that the supervisor diffs, not
  an event ring.
- A driver instance reaches its consumer as a client of the consumer's port,
  wired as ports are today. It never serves a name for connect-by-name.
- The claim-slot limit is removed by stage 1 if cheap, otherwise by its own
  pull request before stage 1's guest smoke
  (`issues/a-cargo-run-boot-claims-every-pcidev-slot.md`).

**Ruled** (orchestrator, 2026-10-20, on the step-10 brief; it replaces #901's
`block@<location>` ports, `each`, `pci@` and several block services):

- One shared block service, diskserver, serves `block` to its clients
  (unchanged: grants are supervisor-minted badges) and `disk` to disk drivers.
  A disk driver is a `drives` row receiving `disk`: it reads its own GPT,
  offers each disk's partitions, and says when a disk is gone.
- On an open the service checks the grant and hands the client's connection
  and region to the offering driver, which answers `MSG_OPENED` on the
  client's connection and serves the session itself; data never passes the
  service (`toyos_blockring::disk`, `lib/blockserve`).
- The supervisor starts the `drives` processes before the storage rows and
  tells the block service how many disk drivers it started. An open of a
  partition no disk offers, and a listing, wait until every boot-started disk
  driver has settled, each wait bounded and naming the driver that did not.

**Measured**:

- **Releases versus the end**, before stage 1a (`usbd_drives_the_spare` on
  QEMU and `usbd_drives_the_type_c_controller` on the T14, each at
  `ed1ec1512` with a timing probe; evidence
  https://github.com/ToyOSOrg/evidence/tree/c5d84905cf952576a337ee5c76cc9a86ad438ff5/lifecycle-s1/release-vs-end
  and `f91e37932`, `lifecycle-s1/t14-release-bound/`, on the same branch): on
  QEMU a killed usbd's claim was released on another CPU about 1.4 ms after
  its end was publishable. A PCI claim's release hook took 0.26 and 3.9 ms
  under TCG. On the T14 the Type-C xHCI's hook, 162 µs (it advertises no
  reset), started on cpu3 before the killed usbd's end was publishable on
  cpu5 and finished after it.
- **PCIe hot-plug on QEMU** (QEMU 11.1.1 q35, OVMF, `ef684e4fc`; evidence
  https://github.com/ToyOSOrg/evidence/tree/309cdaf44da547b3b29742d9dc2a39b25e364954/lifecycle-s1/pcie-hotplug-qemu):
  - An empty `pcie-root-port` gets one bus, a 2 MiB non-prefetchable window
    and a 512 MiB prefetchable one.
  - A function `device_add`ed behind that port has every BAR unassigned.
  - Under q35's default ACPI hot-plug, the add reaches acpiserver as
    `Notify(\_SB_.PCI0.S18_.S00_, 0x1)`, served by nothing, and no
    `DEVICE_DELETED` follows a `device_del` within 6 s.
  - With `ICH9-LPC.acpi-pci-hotplug-with-bridge-support=off`, the delete
    completes at once.
- **QEMU's `pcie-root-port` slot** (QEMU 11.1.1 q35, OVMF, read from the UEFI
  shell; evidence
  https://github.com/ToyOSOrg/evidence/tree/f66e82195fdd3e318663530e3d665db1465dc97d/lifecycle-2/slot-qemu):
  - One MSI-X entry and no MSI, its hot-plug message entry 0; Slot
    Capabilities `0x001a007b`: button, power controller, both indicators,
    surprise, hot-plug capable, command completion. Link Changed Enable
    does not stick.
  - OVMF leaves the empty slot powered off, and a plugged function's
    configuration space reads all ones until the slot is powered.
  - With the slot powered and its power indicator on, `device_del` presses
    the button, and the device stays until the guest powers the slot off.

## Stages

1. **A holder's end is the event.**
   1. **Done** (#910): a process's end is published only after every release
      its handles owed, of every kind, has run on its last thread
      (`object::Releases`), so `CLAIM_RETURN` and the guest tests' claim
      retries went.
   2. **Done** (#926): a claim names a PCI function by its inventory
      record's `Arrival`, and `toyos_pci::claimed` and `DeviceRequest::Pci`
      went; `ClaimError::Ambiguous` stays only for a partition GUID carried
      twice. `lib/manifest` owns the matcher grammar, and two entries one
      device can answer are a render refusal naming both rows, which replaced
      `one_claimant_per_device`. A `drives` row has one process per function
      the kernel does not drive and holds no `serves`, `roles` or `devices`.
      The decisions are `toyos-supervisor` (`system/supervisor/lifecycle`),
      the refusal law among its host tests; the supervisor watches each
      child's `Process` handle and its waiter threads went; vdagent is a
      `drives` row. `a_drives_row_holds_one_process_per_function` is the
      guest arm, on `Profile::HeadlessTwoEdu`.
2. **Functions arrive and leave** (#934, which carries the apertures track's
   stage B, #935: its declaration carries each root bridge's `_OSC` commit).
   - Readers get a device-set object (`toyos_abi::devset`): a
     level-triggered snapshot of every PCI function the kernel hands out,
     minted from `DEVICE`, carrying `READ` and `WAIT` and neither `DUP` nor
     `TRANSFER`, so the supervisor alone holds it. Its header names the time
     by which a slot may change with no interrupt; the supervisor waits to it
     and reads again.
   - The kernel provides native PCIe hot-plug on a port whose root bridge's
     `_OSC` commit granted native hot-plug and capability structure control.
     acpiserver commits both where the query offered both and the image
     declares its ports internal (below). The port is the
     kernel's, its MSI-X entry on the claim path's remapped message; its
     handler reads and clears Slot Status, takes a lost or button-removed
     slot's functions out of the set, makes at most one Slot Control write
     and posts the set's watch. Powering a slot, §6.6.1's 100 ms and reading
     the functions below it run on the set's reader.
   - One record per requester, carrying its live arrival: a device that
     returns is a new arrival on the same record and domain. A slot is armed
     only once every function enumerated behind it at boot is on a domain of
     its own, and a slot with one behind it that a driver in this kernel
     drives or the unit cannot isolate is not armed: every requester first
     seen after boot is blocked at the unit until a claim attaches it, so a
     card seated behind an armed slot reaches nothing until it is claimed. A claim of an
     older arrival answers `Gone` on every call, and its release asks the
     device nothing. The last release behind a removed slot powers it off.
   - An arrived endpoint's BARs are assigned once, before it is listed: each
     one it leaves unassigned is given the lowest address inside its port's
     windows that no other function decodes in and that lies inside the
     declared root bridges' memory, kept where the register reads it back,
     with no probe through it. They enter `Machine::decoded`, which
     `pages::of_bar`'s sharing check reads. No BAR is moved (P1 of
     `issues/there-is-no-wifi.md`).
   - The supervisor diffs the set (`toyos-supervisor`): a departed arrival
     is never started again, and the restart budget is the address's.
   - `a_function_arrives_and_leaves` is the guest arm, on
     `Profile::HeadlessHotplugPort` (q35, `acpi-pci-hotplug-with-bridge-support=off`).

   Gaps it records:
   - **Physical arrival is QEMU's alone**: no bench has a hot-plug port the
     owner lets a driver claim behind, so a metal row reads only the boot set
     and each bridge's commit. That comes through
     `issues/toyos-supports-thunderbolt-and-usb4.md`.
   - QEMU's `device_del` presses the slot's button, so a surprise removal
     (presence lost) is reached by `toyos_pci::hotplug`'s host tests and no
     guest.
   - A machine whose `_OSC` keeps hot-plug for ACPI, q35's default, has no
     slot armed, and says so by port.
   - acpiserver commits native hot-plug before the kernel knows whether a
     slot can be armed; a machine whose units remap no interrupt has its
     slots left unserved after firmware gave them up.
   - **No machine's hot-plug slot is armed** (orchestrator, 2026-10-22,
     holding the owner's "refuse external"): no claim behind an
     external-facing port is refused in code yet
     (`issues/the-iommu-refuses-nothing-yet.md`), and no firmware signal says
     a port is internal. ACPI 6.5 defines none, and Microsoft's
     `ExternalFacingPort` `_DSD` property ("Kernel DMA Protection") marks
     the external ports alone. So acpiserver commits native hot-plug only
     where its image declares every hot-plug port internal
     (`--hot-plug-ports-internal`), which only the QEMU hot-plug test's image
     does, and even then not on a root with a port marked external or one
     whose `_DSD` it cannot read. Exit: claims behind an external-facing port
     are refused by name, and a machine's own ports are then armed.
   - §6.7.1.5's five seconds in which a second press cancels a removal are
     not offered: a press removes at once.
   - A bridge below a slot (a switch) is not read: the kernel assigns no bus
     numbers.
   - A powered, occupied slot whose link never comes up is owed nothing and
     says nothing: no settle time asks again, and no line names the stall,
     where its port enables no Link Changed interrupt (QEMU's does not).
     Exit: such a slot is given a deadline, and its stall is said once the
     deadline passes, with the bound cited from PCIe base spec 6.0.
   - §6.6.1's 100 ms after the link comes up is waited out on every port;
     Readiness Notifications (§6.23), which end it early where a port and
     device support them, are not read.
   - **A departed function's BAR mapping outlives it**: a departure drops
     its BARs from `Machine::decoded`, so the next arrival behind the port
     can be assigned the same addresses while a process still holds a
     mapping made through the departed claim
     (`issues/a-bars-mapping-outlives-the-claim-it-was-asked-on.md`). Its
     addresses are not reassigned while the claim is held, since `retire`
     waits for the release. Exit: that issue's revocation at a claim's
     release, and at departure from the set's reader (`slot::refresh`'s
     per-record loop, never the slot's interrupt), wired by whichever of the
     two lands second.
   - On AArch64's SMMUv3 a function that arrived after the unit was
     programmed is listed and refused a claim by name: its routes are read
     for the functions enumerated at boot.
3. **Drivers leave the shared services.** netstack, soundserver and diskserver
   each become a driver per function plus the service its clients use. The
   client port carries a per-device source and `gone`.
   - **Done for HDA.** `hda` is a `drives` row on `pci:04:03:*`, one process
     per controller; each walks its own codecs and offers soundserver the
     output it reaches on `sound-output` (`toyos-sound-output`), and one that
     reaches none ends by name. soundserver plays to the first offer it can
     mix into, beginning when no stream is open; it is the null sink until
     then, and refuses every later offer. A driver gone after soundserver
     began playing to it ends soundserver. QEMU's two-controller row is
     `hda_plays_to_the_controller_that_reaches_an_output`.
   - **Done for NVMe.** `nvme` is a `drives` row, one process per
     controller, offering its disk to diskserver, which is the block service
     alone (the ruling above).
   - Next: virtio-sound is still driven inside soundserver
     (`system/soundserver/src/virtio.rs`); it becomes a driver on the same
     port. Then the output set: a second output offered while one plays, a
     driver's `gone`, and a stream moved to another output, none of which
     soundserver serves yet; and netstack.
4. **usbd drives every xHCI**, one per controller, started by stage 1's `drives` rows.
   This is step 10 of
   `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`, whose
   `input` port is this track's per-device source and `gone` (`toyos-input`
   carries both), and whose sticks are disks offered on `disk` (the ruling
   above). **Done**, its metal rows that judge with a userland load
   still owed at that step. The test images name QEMU's boot controller by
   id and a bench's as `@boot`, the function firmware booted through, so the
   other controller stays free for the claim tests whichever port holds the
   stick.

## Folded here

- Stage 1 of `issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
  (the waiter threads) was this track's stage 1.2 (#926).
- Stage 2 of `issues/the-supervisor-is-host-tested-and-owns-the-stop.md`
  extends `toyos-supervisor`, the crate stage 1.2 made, which holds restart
  policy and the claims a restart is owed.
- Step 10 of `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`
  is stage 4.
