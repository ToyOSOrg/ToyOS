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

`kernel/src/drivers/xhci`'s `flush_disks`/`hand_back` close the hole for the one
device class this cost a stick: the shutdown now empties every USB disk's write
cache above the boot's last word, and below it takes the controller lock — which
this driver never holds across a transfer — before halting the controller,
resetting it and taking the ports' power away. Holding that lock is what makes
"no transfer is in flight" true rather than likely.

That is one device's answer, not the general one. **Nothing bounds what other
CPUs do between `sync_all` and the reset**, and the next device class to be
given a real workload on this machine will need its own version of the same
argument. The general fix is for `quiesce` to stop the machine before it claims
anything about it — every CPU but the caller parked, and userland off the run
queue — and that is a scheduler change, not a driver one.

## What would show it

A guest whose job list writes continuously while a second thread reboots, with
the block layer counting operations that were in flight at the reset. The count
is zero if the machine was stopped first and non-zero if it was not, and no
existing test asks.
