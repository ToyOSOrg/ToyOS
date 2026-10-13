---
status: assigned
kind: track
opened: 2026-10-13
---

# A device function is driven from its arrival to its end

Held by the orchestrator. #910 carries stage 1's design.

Today the kernel enumerates PCI once (`pcidev::publish`), and the supervisor
mints each claim only when it spawns a `[boot] start` row. A row names a PCI
function by vendor and device, and the kernel refuses the claim `Ambiguous` on
a machine with two. A process's end is published before the releases its
handles queued have run (`issues/deferred-release-outlives-its-syscall.md`), so
a restart retries a claim for up to 2 s (`CLAIM_RETURN` in
`system/supervisor/src/main.rs`) and the `isa` tests retry for up to 5 s
(`claim_after` in `tests/guest/rust-tests/src/isa_row.rs`). One thread per
serving service parks in `SYS_PROCESS_WAIT`. Nothing reaches userland when a
device arrives or leaves.

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

**Measured**:

- **Releases versus the end** (QEMU, `usbd_drives_the_spare` at
  `ed1ec1512` with a timing probe; evidence
  https://github.com/ToyOSOrg/evidence/tree/c5d84905cf952576a337ee5c76cc9a86ad438ff5/lifecycle-s1/release-vs-end): the killed usbd's claim
  was released on another CPU and finished about 1.4 ms after its end was
  published. Each PCI claim's release took 0.26 ms and 3.9 ms under TCG,
  including two log lines, with no reset advertised.
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
   - The kernel publishes a process's end only once every claim it held, of
     every kind, has been released, by the ending thread. The supervisor
     restarts on that end.
   - `CLAIM_RETURN` and `claim_after` go.
   - The supervisor's lifecycle decisions move into a pure crate.
   - A row starts one instance per matching function, and the claim names its
     function by arrival number.
   - The kernel's claim by vendor and device goes.
   - The waiter threads go.
2. **Functions arrive and leave.**
   - Readers get a device-set object, minted from `DEVICE`.
   - The kernel provides native PCIe hot-plug on a port whose host bridge
     `_OSC` granted it.
   - A removed function's claim answers `Gone`.
   - An arrived endpoint's BARs are placed inside its port's window.
3. **Drivers leave the shared services.** netstack, soundserver and diskserver
   each become a driver per function plus the service its clients use. The
   client port carries a per-device source and `gone`.
4. **usbd drives every xHCI**, one per controller, started by stage 1's rows.
   This is step 10 of
   `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`, whose
   `input` port is this track's per-device source and `gone`.

## Folded here

- Stage 1 of `issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
  (the waiter threads) is this track's stage 1.
- Stage 2 of `issues/the-supervisor-is-host-tested-and-owns-the-stop.md`
  extends the crate stage 1 makes. Restart policy and the claims a restart is
  owed move with stage 1.
- Step 10 of `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`
  is stage 4.
- The pull request that takes up stage 1 updates all three.
