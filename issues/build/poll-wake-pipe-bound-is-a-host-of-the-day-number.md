---
status: open
kind: defect
opened: 2026-08-31
---

# `poll_wake_pipe`'s three-second bound is a host-of-the-day number, and a shared shard is a different host

Seen on CI 2026-08-31, pull-request `ci` run 33429908117, job 99613928630
(`guest (1)`), headSha `bd533bd0283266c1e3202f4d3f6bb2b9839cf074`:

```
FAIL rs::poll_wake_pipe: exit code 101
thread 'main' (1) panicked at src/bin/poll_wake_pipe.rs:68:5:
the 300 rounds took 3.007165755s, past the 3s bound — a wake was slow enough to be a lost one recovered by a later edge
  FAIL  poll_wake_pipe  (3s)
```

then, alone in the same job:

```
  PASS  poll_wake_pipe  (1s)
  ALONE poll_wake_pipe: GREEN, and it was alone both times — nothing the
  harness controls differed, so it failed once and passed once. That is a rate
  and not a classification.
```

The shard was otherwise green: `196 passed, 1 failed, 197 total (104.2s)`.

**Nothing was lost, and the capture says so.** The test owes two assertions and
only the second fired. `assert_eq!(woken, ROUNDS, …)` at
`tests/toyos-rust-tests/src/bin/poll_wake_pipe.rs:63` is the lost-wake
assertion, and it passed — all 300 readable edges woke the armed ring. What
failed is `assert!(elapsed < BOUND, …)` at line 68, on a run that took
`3.007165755s` against `const BOUND: Duration = Duration::from_secs(3)` at
line 20. That is 7.2 ms over, 0.24% of the bound. The kernel priced the same
process at `exit: test_rs_poll_wake_pipe pid=155 code=101 cpu=3004ms`.

**The bound is a measurement, not a derivation.** Three seconds for 300 rounds
is what the host that wrote it did, and no host fact widens it: `BOUND` lives
inside the guest binary, so neither the boot-derived host speed nor the guest's
own `vcpus/cores` oversubscription reaches it. The same job's own summary
priced the machine it ran on:

```
host: fastest boot 1890 ms against the reference 1320 ms — liveness ceilings paid at 1.43x width
host: 4 core(s); a guest wider than that waits vcpus/cores longer again
--- irq census: 6 guest(s) reported, 29251 interrupt(s), 25771 of them on cpu0 (88.1%)
```

So the harness measured this host at 1.43x and widened every host-side liveness
ceiling by it, while the one ceiling that decided this verdict was widened by
nothing. A bound that has to be met on the *fastest* host the suite ever runs on
is a coin toss on every other, and 0.24% is the width of the toss.

**Not about the branch it appeared on.** PR #351 (`w4e-namespace-keep-all`) adds
a flags word to `NamespaceBuild`; it touches neither the pipe, the poller, nor
the io_uring ring this test is the canary for.

**Not the typing family.** It shares a day and an `ALONE: GREEN` with
`console_locale_detect` and `desktop_locale_detect` and nothing else: different
subsystem, different failure, and its own capture names its own cause.

Exit: `BOUND` becomes a bound the guest can defend — derived from what a wake
costs rather than from what one host's 300 of them cost, and widened by the
same two host facts every other liveness ceiling in this suite is widened by —
or the timing half is dropped and the lost-wake assertion, which is the thing
this canary exists for, stands alone.
