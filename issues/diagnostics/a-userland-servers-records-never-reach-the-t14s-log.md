---
status: open
kind: defect
opened: 2026-09-13
---

# A userland server's records never reach the T14's log, and the cable judge is written as if they do

`tests/common/lan.rs`'s module header says: "On the T14 a userland `println!`
reaches `Backend::None`, so what crosses to the stick is the kernel's log — into
which netd's `say!` writes, being a `write` to a console object."

The second half is false. Metal run 36 held `tests/lancase` open for twenty
seconds with netd alive and driving the I219, and its readback carries exactly
one line with `netd` in it — the kernel's own `spawn:` record
(`/Users/jan/.claude/jobs/2280e09e/tmp/readbacks/lancase-placement-run36/kernel.log:314`):

```
$ grep -a -c 'netd' .../lancase-placement-run36/kernel.log
1
```

netd printed its MAC, its driver's register window and its DMA grant on that
boot. None of them crossed. A userland `write` reaches the console backend,
which on a machine with no serial port is `Backend::None`; the kernel's record
ring, which is what logd reads and what ends up on the stick, is `log!` and
nothing else.

## What it costs

`lan::on_metal` refuses a boot that carries no `netd: MAC …`, no
`netd: … link up` and no `netd: ready` record, and judges
`lan.lancase.link_up_ms` and `lan.lancase.lease_ms` out of those lines. **The
judge cannot go green on the machine it was written for**, whatever the driver
does — and every diagnostic a userland driver has about why a device stayed
silent is invisible on the one machine that has the device.

The three readings that do cross are the kernel's: the hand-over record,
`pcidev: slot N took its first message on vector 0xNN`, and the end-of-boot
`irq:` census. A bring-up that wants to say more than those three things has
nowhere to say it.

## What would close it

Either a userland record reaches the same ring the kernel's does on a machine
with no serial port, or the judge stops reading netd's lines and asks netd
through its port — the `metalprobe` pattern, a job that exits with a code
encoding the link and the lease, which the placement worker named and left to
the lan branch's round. The first is a kernel change and the second is the
harness's; the stage-2 I219 worker filed this and did neither.

Until one of them lands, a `lancase` boot's whole reading of its own driver is
those three kernel records plus whether the loop's ping was answered.
