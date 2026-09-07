---
status: open
kind: defect
opened: 2026-09-07
---

# `quiesce` syncs and resets while every other CPU is still running userland

`kernel/src/arch/syscall/machine.rs`'s `quiesce` disarms the watchdog, drains
write-back, syncs every filesystem and then resets the machine. **Nothing in it
stops anything else first.** No process is killed, no CPU is parked, no
scheduler is stopped: the caller is one thread on one CPU, and every other CPU
is still running whatever it was running when `SYS_REBOOT` arrived.

So the two facts `quiesce` establishes are true only of the instant it
established them. A process that issues a `write` after `sync_all` returns has
dirty pages nothing will flush; one that is inside a block-layer operation when
`acpi::reboot()` fires is cut off mid-transfer.

## Where it was seen

The metal loop's run 18 (2026-09-07) booted `tests/metaldevicecase` on the T14,
which writes 6 MiB to `/log`, fsyncs it, reads it back and reboots. The boot
stick did not come back: Ubuntu's next boot answered
`usb 3-1: device descriptor read/64, error -71` twice, took the port through its
own power cycle, and gave up with `unable to enumerate USB device`. A firmware
POST did not clear it. Every earlier run — `tests/jobcase`, whose only job is
`reboot` — came back with the stick enumerated, so what the machine had not seen
before was megabytes of traffic followed by a reset.

The bound over the runner's whole job list is one more way in: when
`toyos_tco::JOB_BOUND_MS` expires, `userland/test-runner`'s deadline thread calls
`SysCap::reboot` from *inside* the runner while the job it was watching is still
running — so the reset that ends an overrunning boot lands wherever that job
happens to be, by construction.

## What has been done, and what has not

`kernel/src/drivers/xhci/stop.rs` closed it for the one device class it cost a
stick, and closed it at the reset rather than at the caller: `acpi::reboot` and
`acpi::shutdown` — the only two resets this kernel performs — reset every
connected port, halt every controller, reset it, take the ports' power away and
stop its bus mastering before they write their register. The shutdown adds the
disk-cache flush above the boot's last word and the controller lock below it,
and `userland/test-runner`'s deadline kills the job it was watching and waits for
it before it asks for the reboot.

That is one device's answer, not the general one. **Nothing bounds what other
CPUs do between `sync_all` and the reset**, and the next device class given a
real workload on this machine will need its own version of the same argument.
For USB the argument is the controller lock plus a port reset that ends a
transfer whatever state it was in; NVMe, HDA and the GPU have neither. The
general fix is for `quiesce` to stop the machine before it claims anything about
it — every CPU but the caller parked, and userland off the run queue — and that
is a scheduler change, not a driver one.

## What would show it

A guest whose job list writes continuously while a second thread reboots, with
the block layer counting operations that were in flight at the reset. The count
is zero if the machine was stopped first and non-zero if it was not, and no
existing test asks. `usb_reset_hands_devices_back` asks the narrower question —
whether the reset left a device mid-command — and answers it for USB only.
