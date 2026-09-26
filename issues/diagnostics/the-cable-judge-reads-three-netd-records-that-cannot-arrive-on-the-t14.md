---
status: open
kind: defect
opened: 2026-09-13
---

# The cable judge reads three netd records that cannot arrive on the T14

The cause was that userland stdout and stderr were console writes and `/log`
carried kernel records only; every program's lines reach `/log` now, through
its log ring (`issues/kernel/logging-records-from-every-producer-and-a-kernel-that-waits-on-nobody.md`).
This is what that cost the one judge written as if it were otherwise.

`tests/common/lan.rs`'s `on_metal` refuses a boot that carries no `netd: MAC …`,
no `netd: … link up` and no `netd: ready` record, and judges
`lan.lancase.link_up_ms` and `lan.lancase.lease_ms` out of those lines. **It
cannot go green on the machine it was written for**, whatever the driver does.

Metal run 36 held `tests/lancase` open for twenty seconds with netd alive and
driving the I219, and its readback carries exactly one line with `netd` in it —
the kernel's own `spawn:` record
(`/Users/jan/.claude/jobs/2280e09e/tmp/readbacks/lancase-placement-run36/kernel.log:314`):

```
$ grep -a -c 'netd' .../lancase-placement-run36/kernel.log
1
```

The readings that do cross are the kernel's: the hand-over record,
`pcidev: slot N took its first message on vector 0xNN`, and the end-of-boot
`irq:` census.

## What would close it

Either §1 of the track above lands, or the judge stops reading netd's lines and
asks netd through its port — the `metalprobe` pattern, a job that exits with a
code encoding the link and the lease. The first is a kernel change and the
second is the harness's; neither is the I219 driver's.
