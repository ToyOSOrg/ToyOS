---
status: assigned
kind: track
opened: 2026-10-13
---

# A device function is driven from its arrival to its end

Held by the orchestrator. Stage 1's design is the `toyos-lifecycle` draft
pull request.

Today the kernel enumerates PCI once (`pcidev::publish`), the supervisor mints
every claim only when it spawns a `[boot] start` row, a function a row names by
vendor and device is refused `Ambiguous` when the machine has two, a restart
retries a claim the dead holder's release has not given back yet for up to 2 s
(`CLAIM_RETURN`, `system/supervisor/src/main.rs`), one thread per serving
service parks in `SYS_PROCESS_WAIT`, and nothing reaches userland when a device
arrives or leaves.

**Measured** (QEMU 11.1.1 q35, OVMF, `ef684e4fc`; evidence on the stage 1
pull request): an empty `pcie-root-port` is given one bus, a 2 MiB
non-prefetchable window and a 512 MiB prefetchable one; a function
`device_add`ed behind it has every BAR unassigned. Under q35's default ACPI
hot-plug the add reaches acpiserver as `Notify(\_SB_.PCI0.S18_.S00_, 0x1)`,
served by nothing, and `device_del` never completes; with
`ICH9-LPC.acpi-pci-hotplug-with-bridge-support=off` the delete completes at
once. The T14's only hot-plug ports are its Thunderbolt root ports, whose
functions the owner ruled unclaimable for now
(`issues/the-iommu-refuses-nothing-yet.md`).

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

```
 kernel                 supervisor                    drivers             shared services
 PCI functions ─┐
 ISA rows      ─┼─► device set ──► decide(system.toml) ──► one driver per ──► fileserver, netstack,
 platform      ─┘    (readable      (pure, host-tested)     function, its       input owner,
                      on change)         │                  claim endowed       compositor: one
      ▲ claim by arrival ◄───────────────┘                  at its spawn        port, a source and
      │ removal: the claim answers Gone ──────────────────► driver ends         `gone` per device
      │ a process ends: its handle reads ready ──► start again while present
                                         usbd per xHCI: every USB device stays inside it
```

## Stages

1. **The supervisor answers the device set.** Its lifecycle decisions in a
   pure crate, `toyos-supervisor`; the kernel's device set readable on change,
   each function carrying an arrival number a claim names; one instance of a
   row per matching function; a child's end read as readiness on its handle.
   The waiter threads, `CLAIM_RETURN` and the kernel's claim by vendor and
   device go. Whether PCIe hot-plug is this stage or the next is open with the
   orchestrator.
2. **PCIe hot-plug** (if not in stage 1): arrival and removal on a port whose
   native control the platform granted (`\_SB` host bridge `_OSC`), a removed
   function's claim answering `Gone`, an arrived endpoint's BARs placed inside
   its port's window.
3. **Drivers leave the shared services.** netstack, soundserver and
   diskserver each split into a driver per function and the service its
   clients use, the client port carrying a per-device source and `gone`.
   Blocked on the claim slots
   (`issues/a-cargo-run-boot-claims-every-pcidev-slot.md`) and on
   `issues/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`
   only as defects, never as design inputs.
4. **usbd drives every xHCI**, one per controller through stage 1's rows:
   step 10 of `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`,
   whose `input` port is this track's per-device source and `gone`.

## Folded here

- Stage 1 of `issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
  (the waiter threads) is this track's stage 1; that issue's exit stands.
- Stage 2 of `issues/the-supervisor-is-host-tested-and-owns-the-stop.md`
  extends the crate stage 1 makes: restart policy and the claims a restart is
  owed are stage 1's, the rest of that stage's list stays there.
- Step 10 of `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`
  is stage 4; its exit stands and its usbd is started by stage 1's rows.
