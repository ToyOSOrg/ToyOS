---
status: open
kind: defect
opened: 2026-09-24
---

# `usb_transport_break`'s FlushedStick case can stage its break after the boot's last word

`usb-transport-break-flushed` breaks the first WRITE(10) after a write that
completed and a SYNCHRONIZE CACHE that succeeded over it. The case's job is
`reboot`, and nothing makes a write go out between the stage being armed and
the shutdown: when logd's writes before the job are all covered by flushes the
stage has not yet seen, the first write that qualifies is the one carrying
the shutdown's records, and the break lands after `Rebooting.`:

    [kernel 0.472 cpu3] Rebooting.
    [kernel 0.473 cpu0] usb-storage: the WRITE(10) going out breaks next (usb-transport-break-flushed): a write was reported complete and a SYNCHRONIZE CACHE succeeded after it, with no write since

and the judge says `FlushedStick: after the break, no line reads "Rebooting.", in order`.

Measured on `origin/main` at `7a5a98d3` in a worktree of its own, alone:
red 1 of 4 `--nightly usb_transport_break` runs, green on the harness's own
re-run. The logd branch, its tree as committed at `ebf2f76a`, measured the same words
1 of 3.

## Exit condition

The case's premise is arranged rather than raced: a write after a verified
flush is made to go out before the job ends the boot (or the case waits on the
break before it starts the job), and ten consecutive `--nightly
usb_transport_break` runs are green.
