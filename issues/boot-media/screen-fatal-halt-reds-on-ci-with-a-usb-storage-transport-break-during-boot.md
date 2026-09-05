---
status: open
kind: defect
opened: 2026-09-05
---

# `screen_fatal_halt` reds on CI with a usb-storage transport break during boot, and the runner never says READY

Merge-queue `ci` run 33996725574 (the queue's composition of #422 on main
5962c745), job 101388609567 `guest (10)`, KVM, one guest per machine:

```
FAIL screen_fatal_halt: [qemu] Boot timed out waiting for ===READY===; the console carried:
...
[kernel 0.635 cpu0] spawn: /system/bin/test-runner pid=6 tid=0 ...
init: started test-runner
logd: this boot's kernel log is /log/2026-09-05-230737.log ...
[kernel 2.718 cpu0] usb-storage: 00:02.0 slot 1 transport broke on SCSI 0x35: no answer in the status phase in 2000 ms
  FAIL  screen_fatal_halt  (31s)
  ALONE screen_fatal_halt: GREEN, and it was alone both times — nothing the harness controls differed, so it failed once and passed once. That is a rate and not a classification.
```

Two facts the log holds. test-runner was spawned at 0.635 s and its first act
is to print `===READY===`, which never reached the console in 31 s. The
SYNCHRONIZE CACHE (0x35) the boot issued to the USB disk got no status-phase
answer in 2000 ms and the transport was declared broken at 2.718 s. Whether the
second stalls the first is the question; the redlist row
(`src/redlist.rs`, `screen_fatal_halt`, `Instrument::Ci`) records the one
observation, and this file is its owner's: the usb-storage wait path
(`kernel/src/drivers/xhci/wait/msc.rs`) and whatever the runner's stdout
blocked on.
