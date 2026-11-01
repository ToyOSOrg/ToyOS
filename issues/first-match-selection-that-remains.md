---
status: none
kind: rejected
opened: 2026-08-01
---

# First-match device selection that remains, and why

`pci::enumerate` returns every function now, so a driver taking the first match
does so visibly. Two do, and both are deliberate:

- **NVMe.** blockd serves one controller, the first its row's claim names
  (`userland/blockd`); a machine with two NVMe drives loses the second.
  Making this an enumerate-all is a block-service change, not a PCI one.
- **The virtio drivers but the NIC's.** Each takes the first device with its
  (vendor, device) pair, and a second GPU would be dropped. These are
  QEMU-only devices — no virtio function appears on the T14 — so the exposure
  is a test-shape one. The NIC's driver is one process per function, and
  netstack runs every link they bring: `Profile::HeadlessTwoNics` has two.

Neither is a defect today. Both become one the moment a second such device is
reachable, and the enumerate-all they would need now exists.
