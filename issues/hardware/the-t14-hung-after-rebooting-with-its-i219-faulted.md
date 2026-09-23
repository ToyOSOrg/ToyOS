---
status: open
kind: defect
opened: 2026-09-24
---

# The T14 hung after `Rebooting.` with its I219 left faulted

T14 run 122 (the first service swap, image adf58bb4…, head 89061fcd) ended its
hold at 54.000 s, ran the runner's `reboot`, wrote `Syncing filesystems...` and
`Rebooting.` at 54.430 s — and the machine froze. The owner powered it off,
which erased the black-box page, so nothing after `Rebooting.` survives.

**It is not measured to be an xHCI storm.** cpu0 took 2,327 xHCI interrupts
between the census at 54.000 s and the one at 54.430 s. Run 121's boot, which
rebooted cleanly, took 2,244 between its last census and its own
`Syncing filesystems...` 381 ms later (`t14-run122/sda3-before-flash.img`, the
stick as run 121 left it): the same rate, because that span is the log's last
writes to the stick. `issues/kernel/an-xhci-storm-starves-the-cpu-that-takes-it.md`
is a different shape — ten timer interrupts in thirty-four seconds; here cpu0
took 43 in the same 430 ms.

**What a clean reboot has and this one lacks** is everything after
`Rebooting.`: the loader's next pass reads the black box and reports the
kernel's quiesce (ports reset, controllers halted, bus mastering off) under `|`
lines. Run 122 has none of that because the page is gone, so the hang is
somewhere between the kernel's last record and the firmware's next pass, and
which side is not known.

**The one state this boot had and the clean ones did not** is the I219: the
swapped-in netd's first grant started it mastering while it still ran the old
netd's rings, its write faulted at the unit, and the fault handler cleared its
bus mastering with receive still enabled. It stayed that way to the reset. The
swap's fix (the kernel resets a released function, netd stops the part before
its first grant) removes that state; the next swap run says whether the hang
goes with it. If it does not, the reboot path is owed a quiesce of every
function a process holds, as it already does for its own xHCI controllers.
