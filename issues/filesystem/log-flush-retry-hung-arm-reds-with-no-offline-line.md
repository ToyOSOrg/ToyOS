---
status: open
kind: defect
opened: 2026-09-30
---

# `log_flush_retry`'s hung arm reds with no " is offline: " line

Nightly run 36709239346, guest shard 10 of 12, at `886cee668` (a head of
`wt/toyos-castore`, which touches nothing under `kernel/`, `userland/logd/` or
`tests/common/volumes.rs`: `git diff --stat 84471bc58...886cee668` over those
paths is empty), reds `log_flush_retry`. Main's nightly 36696295750, at
`ace064f9`, has guest (10) green. The run's log, from its first failure line
to the verdict:

```
2026-09-30T14:14:48.7585381Z FAIL log_flush_retry: no " is offline: " in the log, so the staged hung device never met its recovery:
2026-09-30T14:14:48.7585953Z what it said:
2026-09-30T14:14:48.7586444Z [kernel 0.195 cpu0] gpt: the boot volume names 74F67C14-767C-42F3-AF92-65F64C089D73 as the log partition
2026-09-30T14:14:48.7587214Z [kernel 0.196 cpu0] gpt: firmware booted us from partition D432BA2E-E295-40D8-9E6F-CB4B0A98DF78 at LBA 2048+69632
2026-09-30T14:14:48.7589989Z [kernel 0.257 cpu0] gpt: device 1 carries the DATA candidate AD636D9A-EFA6-49BD-A166-3780FFE2E253 at LBA 2048+258048
2026-09-30T14:14:48.7591038Z [kernel 0.259 cpu0] gpt: device 1 has 1 partitions and none of them is ours
2026-09-30T14:14:48.7591946Z [kernel 0.397 cpu0] usb-storage: slot 1 vendor "QEMU    " product "QEMU HARDDISK   "
2026-09-30T14:14:48.7592809Z [kernel 0.399 cpu0] usb-storage: slot 1 serial number "TOYOS0BOOTSTICK1"
2026-09-30T14:14:48.7593777Z [kernel 0.401 cpu0] usb-storage: disk 0 ready on slot 1, 28672 blocks of 512 B (112 MiB), msc_block +0x10000
2026-09-30T14:14:48.7594959Z [kernel 0.405 cpu0] usb-storage: 1 device(s)
2026-09-30T14:14:48.7595921Z [kernel 0.412 cpu0] gpt: device 16 carries the log partition 74F67C14-767C-42F3-AF92-65F64C089D73 at LBA 73728+69632, entry 2 of 5
2026-09-30T14:14:48.7597468Z [kernel 0.414 cpu0] gpt: device 16 carries the boot partition at LBA 2048+69632 (512-byte blocks), entry 0 of 5 on disk BD204DF6-758B-42D6-B62B-052F4FED7BD6
2026-09-30T14:14:48.7598993Z [kernel 0.427 cpu0] boot-volume: partition mounted from device 16, 35651584 bytes of a 35651584-byte partition at device offset 1048576, 512-byte sectors, 512-byte clusters, 68552 clusters
2026-09-30T14:14:48.7600219Z [kernel 0.431 cpu0] log-volume: partition mounted from device 16, 35651584 bytes of a 35651584-byte partition at device offset 37748736, 512-byte sectors, 512-byte clusters, 68552 clusters
2026-09-30T14:14:48.7601302Z [kernel 0.499 cpu1] usb-storage: 00:02.0 slot 1 transport broke on SCSI 0x2a: a staged break skipped the data phase wait; break 1 of 3 running
2026-09-30T14:14:48.7602354Z [kernel 0.499 cpu1] usb-storage: 00:02.0 slot 1 is owed the data of the command that broke, so nothing can be asked of it on the Bulk-Out: its port is reset with no class reset before it
2026-09-30T14:14:48.7603384Z [kernel 0.550 cpu1] usb-storage: 00:02.0 slot 1 would not take SET_CONFIGURATION(1) after its port reset: a staged break skipped the status stage wait
2026-09-30T14:14:48.7604196Z [kernel 0.550 cpu1] usb-storage: 00:02.0 slot 1 the port reset was not answered; break 2 of 3 running
2026-09-30T14:14:48.7605208Z [kernel 0.550 cpu1] usb-storage: 00:02.0 slot 1 broke 2 times running; its port reset did not bring the transport back
2026-09-30T14:14:48.7605816Z {0.556 logd} logd: cannot create /log/2026-09-30-141447.log: other error
2026-09-30T14:14:48.7606412Z {0.556 logd} logd: no /log on this machine - this boot's kernel log is on the console only (2026-09-30 14:14:47 UTC)
2026-09-30T14:14:48.7606917Z   FAIL  log_flush_retry  (8s)
```

The staged break (`usb-transport-break`, `usb-reset-break`) ran its first two
breaks, and the kernel said the port reset did not bring the transport back;
logd's give-up line follows 6 ms of guest time later, and no line holding
" is offline: " is in the log. The hung arm (`tests/common/volumes.rs`) reads
the console only until the first line holding "on the console only", and then
requires all three of "transport broke on SCSI", "the port reset was not
answered; break 2 of 3 running" and " is offline: ".

`issues/filesystem/log-flush-retry-deadman-arm.md` records the test's other
ways to red; this shape is not among them.

**Exit**: the hung arm reads " is offline: " whenever the staged break runs,
or the test waits for the line the kernel's recovery actually ends on.
