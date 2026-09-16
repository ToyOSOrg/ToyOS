---
status: open
kind: defect
opened: 2026-09-16
---

# A 120000 ms boot deadline fired 132859 ms late on the T14, and the hard-lockup bound armed beside it was silent too

`kernel/src/deadline.rs` reads the bound off the parameter line (`claim`,
`:150-162`), turns it into a TSC deadline once there is a clock (`start`,
`:168-183`), and `poll` (`:194-200`) compares one relaxed load of `AT_TSC`
against `rdtsc` from the timer interrupt entry — `kernel/src/arch/idt/timer.rs:71`
(`:79`) in Ring 0 and `:96` in Ring 3 — and calls `expire` (`:208-225`), which
seals the record and writes the reset register through `acpi::reset_now`.
`start` arms the other half of the same parameter at `:182`,
`crate::hardlockup::start(ms)`: a bound of `toyos_tco::hard_lockup_bound_ms`,
`deadline_ms / 2` (`toyos-tco/src/lib.rs:154-156`), sampled by an NMI every
second on every CPU (`kernel/src/hardlockup/mod.rs:1-31`). The two share one
seal (`claim_the_reset`, `deadline.rs:69-71`).

A boot armed with `boot-deadline=120000` prints both arms at 0.060 s — quoted
from bench run 42's kernel log, the text `deadline.rs:174-177` and
`hardlockup/mod.rs:193-198` write:

    boot deadline: 120000 ms, after which this kernel seals a WEDGED record and writes the reset register itself
    hard lockup: 60000 ms, sampled every 1000 ms by each cpu's own performance counter, after which a cpu that has taken no interrupt seals a WEDGED record and resets the machine

Run 55's boot — armed at 2026-09-14 20:54:57Z, the `lancase` image of the
unmerged branch `i219-delivery` at `4d604c86`, whose `kernel/src/deadline.rs`,
`kernel/src/hardlockup/mod.rs` and `kernel/src/drivers/acpi.rs` are
byte-identical to this tree's (`git diff --stat 4d604c86 HEAD -- <those>` is
empty) — did not come back inside the loop's 420 s, and its own kernel log
never reached the stick, so its print of the two lines is inferred from
`start` and not read. Its `WEDGED` record was read by run 56's loader pass:
a pass that printed the record and ended the chain without booting a kernel
(`bootloader/src/blackbox.rs:149-154`, `bootloader/src/main.rs:857-860`), so
that the machine came back to Ubuntu after 100 s and the loop read the
stick's `loader.log`, which reads, whole above the ring tail:

    --- the pass after the reset, reading what the boot above left
    ToyOS Bootloader 1.0
    Boot attempts: this image has had the machine 0 time(s) without reporting; now 0
    Black box: the record below is from the boot armed at 2026-09-14-205457
    Previous boot's panic: the last boot read WEDGED, so a bound of its own ended it and this chain ends here
    | the boot deadline expired: a bound of 120000 ms, reached at 252859 ms, with this machine in `complete`. The tail of the log ring follows ... which is what nothing was draining.
    | older records dropped to fit this page: 227

252859 − 120000 = 132859 ms: `poll` first ran past its bound 132.859 s after
it. In the same boot the 60000 ms hard-lockup bound sealed nothing — the seal
is the deadline's — and that detector seals only for a CPU whose interrupt
count is stale for its bound *and* whose sampled frame has `IF` clear
(`hardlockup/mod.rs:21-31`). So for 133 s past the bound no CPU's timer entry
ran `poll`, and no CPU met both of the detector's conditions for 60 s, or none
was sampled; which of those is unmeasured.

## The sealed tail, whole where it bears on this

The tail's `irq:` census, every line as sealed — printed twice, by two dying
processes' fault reports 8 ms apart:

    | [3.471 cpu4] irq: cpu0 total=1741 timer=9 xhci=1729 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=0 nmi=3 spurious=0 unclaimed=0
    | [3.471 cpu4] irq: cpu1 total=32 timer=30 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0
    | [3.471 cpu4] irq: cpu2 total=2 timer=0 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0
    | [3.471 cpu4] irq: cpu3 total=3 timer=1 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0
    | [3.471 cpu4] irq: cpu4 total=219 timer=215 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=2 spurious=0 unclaimed=0
    | [3.471 cpu4] irq: cpu5 total=4 timer=2 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0
    | [3.471 cpu4] irq: cpu6 total=2 timer=0 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0
    | [3.471 cpu4] irq: cpu7 total=2 timer=0 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0
    | [3.479 cpu5] irq: cpu0 total=1741 timer=9 xhci=1729 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=0 nmi=3 spurious=0 unclaimed=0
    | [3.479 cpu5] irq: cpu1 total=50 timer=48 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0
    | [3.479 cpu5] irq: cpu2 total=2 timer=0 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0
    | [3.479 cpu5] irq: cpu3 total=3 timer=1 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0
    | [3.479 cpu5] irq: cpu4 total=219 timer=215 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=2 spurious=0 unclaimed=0
    | [3.479 cpu5] irq: cpu5 total=5 timer=3 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0
    | [3.479 cpu5] irq: cpu6 total=2 timer=0 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0
    | [3.479 cpu5] irq: cpu7 total=2 timer=0 xhci=0 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=2 nmi=0 spurious=0 unclaimed=0

A snapshot at 3.471 s and 3.479 s, 249 s before the expiry. cpu0 had taken
nine timer interrupts against 1729 xHCI ones; cpu4 had taken 215 and cpu1 30
in the same 3.47 s. So cpu0's state explains nothing about the bound: `poll`
runs on every CPU's timer entry, and two CPUs were taking theirs at a normal
cadence when the census was taken. What every CPU did between the tail's last
record and the expiry is unrecorded — the records run from 3.471 s to 5.556 s
and stop, and the machine wrote nothing for the next 247 s.

What the tail names as the machine's state when the records stop:

    | [3.471 cpu4] exit: logd pid=4 code=-1 cpu=2143ms
    | [3.479 cpu5] fault: 0x1000004e5c0 is backed by a file byte 0 that the device would not read; leaving the fault unhandled
    | [3.479 cpu5] exit: netd pid=5 code=-1 cpu=22ms
    | [3.479 cpu1] pcidev: PCI 00:1f.6 [8086:15fc] released from slot 0
    | [3.493 cpu0] spawn: /system/bin/test-runner: ELF: fewer bytes than a file header
    | [3.556 cpu0] xHCI: 00:14.0 slot 5 endpoint 3 is Stopped, recovering
    | [5.556 cpu0] xHCI: Set TR Dequeue timed out
    | [5.556 cpu0] usb-storage: 00:14.0 slot 5 reset recovery failed; disk is offline
    | [5.556 cpu0] root: read of block 3 failed

Between those, cpu5 and then cpu0 repeat `usb-storage: 00:14.0 slot 5
transport broke on SCSI 0x28` and `SCSI 0x28 broke 3 times running; the
transport is not coming back on its own`: the boot stick's READ(10) transport
broke at 3.47 s, the root filesystem stopped answering, `logd` and `netd` died
on page faults over file bytes the device would not read, the test runner
could not be loaded, and the disk went offline at 5.556 s. A boot with no
runner asks for no reboot; the deadline was the only bound left, and it fired
133 s late.

## The instrument that measures this already exists

`src/metal.rs:1591-1597`'s `deadline_lateness_ms` computes exactly
`reached − bound` out of the `DEADLINE_EXPIRED` line (`src/bootlog.rs:28`);
`tests/common/metal.rs:274-275` reads it off every readback and `:895-919`
hands it to `profile.judge` as `boot.<label>.deadline_lateness_ms`;
`tests/metal-profile.toml:409-414` prices that row for `deadlinewedge` —
ceiling 10000 ms, "the widest true bound before [the timer period] has been"
measured, `measured = 61`. 132859 is 13x that ceiling, on a boot the profile
does not name: `lancase` is not among the labels the file prices, and is not
in this tree. The arithmetic above is the instrument's own, done by hand
because the run was `toyos-metal` invoked directly and not the harness. No
second instrument is owed; a cause is.

## What is known and what is not

- `issues/kernel/an-xhci-storm-starves-the-cpu-that-takes-it.md` carries the
  same shape, over the 34 s the stick was being written, on run 20's
  `metaldevicecase` boot at tip `f46f91eb`
  (`[2026-09-07 08:51:28 34.609 cpu1] irq: cpu0 total=103171 timer=10 xhci=103161 net=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=0 nmi=0 spurious=0 unclaimed=0`)
  and closes with the question this run does not answer: whether an xHCI
  storm can by itself hold a CPU out of its timer for the whole of a bound.
  Run 55's census shows cpu0 in that state at 3.47 s and other CPUs not; it
  does not show what held every CPU's timer entry off `poll` for 133 s.
- The earlier deadline-ended T14 boot — run 25's `ccorpus`, back after 187 s
  with no record because the reset left its stick unenumerable — was tracked
  as a defect whose exit condition was a `loader.log` from a boot that ran to
  its deadline with the ring tail naming what the machine was doing; this
  record is that, and the commit that added this file closed it. Its
  inference — that a deadline which fires is a machine on which some CPU was
  still taking a timer interrupt — holds for the instant it fired; this record
  shows the 133 s before it in which none did.
- `issues/hardware/a-t14-boot-wedges-after-a-jobs-exit-and-nothing-said-why.md`
  waits for a `WEDGED` record of a different shape — a `testcases-mkdir` or
  `-readdir` boot stopping between a job's `exit:` and the next `spawn:` —
  and names a 2 MiB symbol-table read of 512 SCSI READ(10) commands as the
  one device call in its window; run 55's tail names READ(10) transport
  breaks on the same stick. Whether those are one defect is unmeasured, and
  this record is not that file's exit condition.
- `issues/kernel/the-deadline-header-promises-any-interrupt-and-only-the-timer-entry-polls.md`
  is the tree-checkable half: the module header's coverage claim is wider
  than the poll's one call site.

**Exit condition**: the cause of a `poll` that ran 132859 ms past its bound is
named with evidence and either removed or priced, so that a T14 expiry's
lateness is held to the row that already exists.
