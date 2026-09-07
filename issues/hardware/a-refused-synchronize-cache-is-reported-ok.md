---
status: open
kind: defect
opened: 2026-09-07
---

# The T14's boot stick refuses SYNCHRONIZE CACHE and the shutdown reports it `ok`

Every T14 boot on run 19 — the three that hung and the two that passed — logs
this at about 1.9 s:

```
usb-storage: disk 0 does not implement SYNCHRONIZE CACHE (sense 0x05/0x20/0x00);
its writes are durable once they complete
```

and every boot that reached its shutdown then logs:

```
usb-quiesce: disk 0 SYNCHRONIZE CACHE ok
```

about the **same** refusal. `msc_flush`
(`kernel/src/drivers/xhci/wait/msc.rs:414`) maps ILLEGAL REQUEST / INVALID
COMMAND OPERATION CODE to `Ok(())` — correctly, since a device with no write
cache made nothing durable by refusing — and `flush_disks`'s `Flushed` display
(`kernel/src/drivers/xhci/mod.rs:1478`) has no word for "there was nothing to
flush", so it prints the one it has.

Two consequences, and the second is the one that matters:

1. **The latch gates the log line and not the command.** `no_write_cache`
   (`msc.rs:83`) is set on the first refusal and consulted only to suppress the
   second log line; the CDB is built and issued unconditionally on every flush
   after it. On this stick every `logd` batch therefore pays a full Bulk-Only
   round trip for 0x35 plus a REQUEST SENSE round trip to learn it was refused,
   both under `XHCI` with preemption off, on the same device the log is going
   to.

2. **Two assertions pass on metal for the wrong reason.**
   `tests/common/devices.rs:166` asserts the `usb-quiesce: disk 0 SYNCHRONIZE
   CACHE ok` line and `src/metaldevices.rs:181` counts its presence. Under QEMU
   the emulated disk implements 0x35 and the line means what it says; on the
   T14 the identical line is produced by a refusal, so neither can tell the two
   machines apart and a genuine flush failure on hardware is not distinguishable
   from a device that has no cache.

**Exit condition**: `msc_flush` consulting its own latch before it issues, and
a shutdown line that says which of the two happened — with the metal assertion
reading the distinction rather than the word `ok`.
