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
      child's `Process` handle and its waiter threads went; the virtio-serial
      driver (`virtio-console`) is a `drives` row. `a_drives_row_holds_one_process_per_function` is the
      guest arm, on `Profile::HeadlessTwoEdu`.
2. **Functions arrive and leave.**
   - Readers get a device-set object, minted from `DEVICE`.
   - The kernel provides native PCIe hot-plug on a port whose host bridge
     `_OSC` granted it.
   - A removed function's claim answers `Gone`.
   - An arrived endpoint's BARs are placed inside its port's window. No
     BAR-moving path is left to build on (P1 of `issues/there-is-no-wifi.md`):
     `place_unassigned` on `wt/toyos-lifecycle-2` is rewritten as one
     assignment inside the port's window with no probe, and an arrived
     function's BARs enter `Machine::decoded`, which `pages::of_bar`'s
     sharing check reads.
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
