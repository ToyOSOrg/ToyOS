---
status: open
kind: defect
opened: 2026-09-21
---

# `metal_device_probe`'s `usbread` answered `IoFailed` once beside other guests

One full fast tier on the dev host (`cargo test`, `wt/toyos-govcut` at
`2bd5076a`, a branch whose kernel diff is two comment lines): 343 passed, 1
failed, 344 total in 225.8 s, with another worktree's suite
(`toyos-usbtransport: usb_transport_break`) holding guest slots on the same
host.

```
  [devices] usbwrite: 737075 us
FAIL metal_device_probe: usbread: the job refused — IoFailed (the volume was there and an operation on the open file failed)
  FAIL  metal_device_probe  (11s)
...
  [devices] usbread: 4998 us
  PASS  metal_device_probe  (3s)
  ALONE metal_device_probe: GREEN — it fails only beside other guests, so its Sched::Parallel is wrong. The run stays red on the classification.
```

The `usbwrite` before it took 737 ms where the alone run's read took 5 ms, so
the guest was slow when the read failed; which operation on the open file
answered `IoFailed`, and whether the transport broke under it, is not in the
capture the summary kept. One sighting, no rate, no mechanism.

Owed: the read's failing operation named from the full capture on the next
sighting, and whether `usb-storage` logged a transport break before it.
