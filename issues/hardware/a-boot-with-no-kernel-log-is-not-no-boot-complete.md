---
status: open
kind: tooling
opened: 2026-09-08
---

# A boot that left no kernel log is a verdict of its own, and the loop calls it "no Boot: complete"

Run 22's hung mkdir image left the stick holding `attempts` and `loader.log` and
**no kernel log file at all** — where every earlier hung boot (run 19's three)
left a 325-line file ending at a spawn. `loader.log` shows an ordinary handoff,
so that kernel started; `logd` either never created its file or its first write
never completed. The stick after the power cut is
`/Users/jan/.claude/jobs/2280e09e/tmp/t14-run22/stick-after-cycle.log`.

`src/metal.rs` reads that back as the absence of `Boot: complete`, which is the
same verdict it gives a boot that ran and stopped late. They are not the same
machine: a boot with no kernel log **and** no black-box report wedged before its
first durable record, which places the failure before `logd`'s first `SYS_FSYNC`
returned rather than anywhere in the job list — the single most useful thing the
readback could say about the run-22 class of hang.

**Exit condition**: the loop names that case as itself — a readback with no
`logd` file and no harvested report reported as "wedged before its first durable
record", distinct from "no `Boot: complete`" and from "no readback at all" — and
a metal-profile row that says which of the three a boot is.

Off the path of whoever finds this: it is `src/metal.rs`'s verdict and belongs
to the driver, not to the kernel side that produced the boot.
