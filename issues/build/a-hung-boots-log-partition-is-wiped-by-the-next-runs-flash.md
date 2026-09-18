---
status: open
kind: tooling
opened: 2026-09-16
---

# A hung boot's log partition is wiped by the next run's flash, and survives only if something outside the loop read it first

`src/metal.rs`'s `run` (`:1416`) is one sequence: `driver.flash(&image)` at
`:1471`, the reboot at `:1475`, `ride_the_reboot` at `:1483`,
`wait_for_the_stick` at `:1487`, `read_log` at `:1489` and, under
`--fat32-check`, `raw_log` at `:1496`. `flash` (`:1000-1002`) is `wipefs --all`
and then `dd` over the whole disk (`Job::Wipe` and `Job::Flash`, `:519-520`),
unconditionally, before anything in the same invocation reads a byte. So the
loop reads a partition only after its own boot came back, and a run whose boot
does not come back reaches neither read: `wait` returns `Refusal::Silent` at
`:1121` and `wait_for_the_stick` returns `Refusal::Stick` at `:1107`, both
before `:1489`. The next invocation opens with `flash` again. The previous
boot's partition therefore survives to be read exactly when something outside
the loop reads it between the two invocations — and nothing in the loop does.

**And the loop cannot read a wedged stick at all.** `read_log` `?`s on the
`mkdir` and the mount (`:1131-1132`) and `read_mounted` on the listing and
every `cat` (`:1152-1162`); `raw_log` (`:1173`) refuses a short read by name
(`Refusal::Landed`, `:1184-1190`). Both are reached only after
`wait_for_the_stick` has seen the block node for up to `STICK_SECS` (`:68`,
30 s) — which is not tolerance of an unreachable stick but the reason neither
is asked to tolerate one. A pre-flash call to either, as written, would refuse
every run whose predecessor left the stick unenumerable (run 49's mode, below)
instead of recording that it did.

## What was and was not lost

Five `lancase`-family boots on the bench did not come back: runs 36, 52, 54
and 55 past the loop's 420 s bound (`Refusal::Silent`), and run 49 with the
stick unenumerable (`Refusal::Stick`, back at 303 s). `lancase` is PR #442's
boot (branch `lan-metal`) and exists on no branch that has merged; every one
of these runs was of an unmerged branch's binaries, and the loop's sequence
above is the same in this tree.

Run 55's partition is the one that was read. Its loop's transcript ends at
21:02:01Z with

    toyos-metal: the machine did not come back within 420 s, which is longer than every watchdog a boot runs under plus the time coming back costs; why it did not is what the panel and the log partition say, and neither is readable from here

and run 56's transcript opens, at 21:29:39Z, with

    run56 = repeat of run 55 (i219del lancase, unarmed, head 4d604c86); owner photographs the screen at hang; previous partition saved at […]/t14-run55/run55-sda3.img

— a copy of `/dev/sda3` taken from outside the loop at 21:15Z, 35651584 bytes,
which is the log partition's 69632 sectors (`log p3 at 151552+69632`, the
loop's own line about the image) whole. Run 56's `flash` then overwrote it.
The copy held two files, `loader.log` and `attempts`, and no file `logd`
wrote. Its `loader.log` is the loader's first pass alone, 105 lines ending in
`Loader log: the kernel handoff begins, so this file ends here`, and its line
20 reads

    Black box: 0x8000000 armed at 2026-09-14-205457 for [7a, cb, db, a9, 04, 52, 10, 4a, b2, ac, 8c, 55, b4, 43, b5, 90], and the kernel is told so on its parameter line

— the boot whose `WEDGED` record run 56's loader pass then printed
(`issues/kernel/a-120000-ms-boot-deadline-fired-132859-ms-late-on-the-t14.md`).
So for the one hung boot whose partition was captured, the flash would have
destroyed nothing `logd` wrote, because `logd` wrote nothing: that boot's ring
tail carries `exit: logd pid=4 code=-1 cpu=2143ms` at 3.471 s. What the other
four partitions held is unknown, and the sequence above is why.

`clear_readback` (`:1646`), called at `:1438-1440` before the flash, removes
the previous run's readback files for the opposite reason — so a refusal does
not leave them for a judge to read as this run's — and copies nothing.

**Exit condition**: before `flash`, the loop saves the stick's existing log
partition where the stick answers, and where it does not — the block node
absent, the read short — records that in the run's own output rather than
refusing; a run that hangs then leaves a partition the next invocation
captures, and a run that wedges the stick leaves a line saying so.
