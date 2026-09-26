---
status: open
kind: finding
opened: 2026-09-26
---

# `quiesce_wakes_on_the_last_exit` lost its serial READY beside other guests

Fast tier at `a4b44a91` (PR #510's branch; `toyos-sshupload`'s suite held build
slots at the same time): `QEMU died before ===READY=== (status: exit 0)`. The
guest did everything the test reads: `quiesce-last-exit: quiesce-last is held`,
`Syncing filesystems...`, `stop: 4 of 4 userland thread(s) stopped ... over 3
sweep(s)`, `usb-quiesce: disk 0 SYNCHRONIZE CACHE ok`, `Rebooting.` But the uart
captured `nothing at all`. Before it, `usb-storage: 00:02.0 slot 1 transport
broke on SCSI 0x28: no answer in the data phase in 2000 ms`, and the
test-runner's spawn reported `layout=2069ms`. The harness's re-run alone was
green in 3 s, 2 sweeps. `cargo run -- --known-red` answers NO.

Not shown: why the uart saw none of the test-runner's output in a boot whose
console carried the whole stop. The READY wait reports the missing marker, not
its cause.

**Exit**: a cause for the empty uart on a boot that rebooted as designed, or
the marker waited for where the boot's reboot cannot race it.
