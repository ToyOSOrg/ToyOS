---
status: open
kind: defect
opened: 2026-09-25
---

# A job's exit record landed under the boot's last word

`quiesce_wakes_on_the_last_park` (`tests/common/power.rs`'s `stopped_boot`)
was red twice in one fast-tier run on PR #492, once beside other guests and
once alone, with the dev host also running another worktree's suite:

```
1 line(s) reached the console after the boot's last word:
  [kernel 2.546 cpu0 tid=2] exit: test_rs_quiesce_last tid=2 code=0 cpu=2014ms
```

(`2.468` on the rerun.) Three runs alone right after, on the same tree, were
green. The round under test changed no kernel, init, test-runner or
`tests/quiescelastcase` file, so the record was the kernel's own: the job that
parked last exited, and its `exit:` record reached the console after
`Rebooting.`.

Not known: whether the job's exit is a transition the stop is meant to have
seen before it writes the last word, or a record the stop should keep from
the console once it has.

## Exit condition

A job's exit record either precedes the boot's last word or never reaches the
console after it, and the test is green under a loaded host.
