---
status: open
kind: defect
opened: 2026-09-08
---

# A wedged boot's record outgrows both channels that carry it, and both drop the end

Measured on the T14, run 23 (`hard-lockup-probe` at 30816a47, readback in
`target/metal-lockup/hardlockup/`). The boot ended itself exactly as designed and
the record is the whole diagnosis, but **neither channel carried all of it, and
each lost the part written last** — which on a wedge is the part that says what
happened.

**1. The sealed tail keeps the oldest records that fit.** The page's own line
reads `older records dropped to fit this page: 94`, and what follows starts at
`[0.155 cpu0] file cache: budget …` and stops at `[1.211 cpu7] CPU 7: joining
scheduler`. The boot ran to 2.240 s and the two lines the control wrote about
itself — `hard-lockup: staged, …` and `hard-lockup-probe: cpu7 has a performance
counter of its own …`, both at ~2.3 s — are not on the page. The header of
`kernel/src/deadline.rs` calls the tail "what nothing was draining", which is the
*newest* records; the page holds the oldest that fit after a fixed drop.

**2. `loader.log` stops mid-record at 16,365 bytes.** The reporting pass's own
last line — `Loader log: the last boot is accounted for, so this pass resets the
machine`, which is how a reader tells a pass that ended the chain from one that
returned to the boot manager — is not in the file; the file ends inside the
tail. `bootloader/src/loaderlog.rs` writes and flushes line by line and disables
its sink on a short write, reporting the refusal with `println!` — **to a console
the T14 does not have**, so on this machine the file simply stops and nothing
says why. Whether that pass reset the machine or returned to firmware is
therefore unknown for run 23, and the difference is the one
`bootlog::CHAIN_ENDS_LINE` exists to state: a UEFI application that returns
leaves its `SIGNAL_EXIT_BOOT_SERVICES` callback registered for the next
operating system to call into.

Neither is visible on QEMU: the guest's serial console carries every line
whatever the file does, and a `jobcase` boot's ring is small enough that its
newest records fit the page. `power::hard_lockup_chain` therefore judges the
metal arm on the record's own fields and not on either witness line, and says so
at the site.

**Exit condition**: a wedged boot on the T14 whose page carries the records
either side of the wedge, and a `loader.log` that carries the pass's own last
line — or, where a channel genuinely cannot hold the report, a refusal on the
stick saying which part was dropped, since a truncation nothing declares is a
diagnosis a reader silently gets wrong.
