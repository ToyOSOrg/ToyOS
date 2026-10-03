---
status: open
kind: track
opened: 2026-08-10
---

# A disk plugged in after boot is bound inside a scheduling pass

The BOT round trip and the SCSI bring-up above it are pure machines,
`toyos_xhci::bot::RoundTrip` and `toyos_xhci::scsi::BringUp`, and the kernel
drives both blocking, in place. For the read/write entry points that is the
caller's own time. For the bind it is not: `msc::bind` is the one call site
where a scheduling pass can still spend its transfer budget inside xHCI — for a
disk arriving *after* boot, which is one greppable path.

**What to build**: a stepped driver for the bind over the same two machines,
one act per pass, as enumeration has. Moving the bind to a thread that may
block — usbd, step 10 of
`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md` — ends
this file as well.

After it, the pass-duration proof costs no new code: one guest gate measuring a
scheduling pass across a plug, plus the existing check-build's pass-cost
distribution. Both premises are now spent.

Constraints the machine has to preserve:

- **One outstanding command, not a queue.** The command ring is one queue and
  the driver is strictly serial; a completion event must be matched against the
  command TRB's *physical address*, or a timed-out command hands its code to
  whoever waits next.
- **A control transfer with a data stage is two completions** — the data stage
  carries ISP and IOC, and the status stage is a second event on the same
  (slot, dci) — so a submission takes a stage count and the answer carries the
  residue as well as the code.
- The lock disables preemption for the guard's whole life, warns at 50M spins
  and panics at 500M, against a 2 s per-command budget. **The ticket lock is
  excluded as the T14 freeze's mechanism** — the owner saw neither the warning
  nor the panic. Do not re-litigate that.
- Rust makes a module's private items visible to its **descendants**, so a
  *view* handed to the poll enforces nothing. The split had to be a module, with
  the poll outside it.
- **There is no gate for the 100 ms unplug window and it cannot be aimed** — a
  QEMU `device_del` cannot land inside it. That is the answer, not an omission.
  Relatedly, QEMU's xHC has no link training and no inactive state: its
  SuperSpeed ports read enabled the moment they are touched, so warm-reset
  correctness lives in the host model only.

`issues/hardware/pulling-the-boot-stick-freezes-the-t14.md` is open and is not
closed by this.
