---
status: open
kind: defect
opened: 2026-09-25
---

# A thread ended after the boot's last word, and no record can say so now

`quiesce_wakes_on_the_last_park` (`tests/common/power.rs`'s `stopped_boot`)
was red twice in one fast-tier run on PR #492, once beside other guests and
once alone, with the dev host also running another worktree's suite:

```
1 line(s) reached the console after the boot's last word:
  [kernel 2.546 cpu0 tid=2] exit: test_rs_quiesce_last tid=2 code=0 cpu=2014ms
```

(`2.468` on the rerun.) Three runs alone right after, on the same tree, were
green. The round under test changed no kernel, init, test-runner or
`tests/quiescelastcase` file, so the record was the kernel's own: the thread
the job parked last ended, and the record of its end reached the console
after `Rebooting.`.

**The record that showed it is deleted, and what it showed is not answered.**
A thread's end writes nothing now (`kernel/src/process.rs`'s header), so that
line cannot follow the last word again and no boot can show this. The sighting
does not say which of two things happened: the thread ran its exit after the
stop had counted it stopped, or it ended before the last word and its record
was committed behind it. The first is a thread running kernel code on a
machine the stop has declared still; the stop's `stop:` record carries counts
taken at its last sweep and nothing later.

Owner: the stop path, `kernel/src/quiesce.rs`.

## Exit condition

- The stop says how many threads ended between its `stop:` record and the
  reset, in a record the next loader pass prints, and `stopped_boot` reds on
  one that is not zero.
- `quiesce_wakes_on_the_last_park`, deleted with the commits
  `issues/quiesce-wakes-on-the-last-park-gave-up-on-one-thread-beside-the-held-one.md`
  names, is back and green beside other guests on a loaded host with that
  check in it.
