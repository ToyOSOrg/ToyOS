---
status: open
kind: defect
opened: 2026-09-27
---

# `lan_swap`'s redial spent its ceiling on a nightly shard

PR #535's nightly (run 36314576406, `guest (1)`, a4f68c5a, KVM, QEMU 11.1.0):

```
FAIL lan_swap: 2 finding(s):
  init's words on netd were ["accepted"] ending in None, where InService is owed (the stream had 1 connection(s) before the ask and 1 after)
  the stream's redial was turned away 64 time(s), its ceiling of 64, and gave up
```

The guest completed the swap. Its console has `init: swap netd: in service`
at 6.141 s, and `logd: serving this boot's log on port 41337` at 1.176 s
through the new netd. After that `logd` admitted no reader: no second
`serving this boot's log to 10.0.2.2:…` line. So none of the host's 64 dials
reached `logd` once it listened again. That is consistent with all of them
ending inside the guest's gap, which runs from `logd`'s `netd is being
replaced` (1.000 s), through init stopping the old netd (1.080 s), to `logd`
listening again (1.176 s). The alone re-run was green with 4 dials turned away.

This is `issues/build/swap-crash-rolls-back-reds-when-its-redial-spends-its-ceiling-under-load.md`
on the 82574 bench: the compromise
`issues/diagnostics/a-swaps-redial-asks-again-with-no-event-to-wait-on.md`
records, reached. A redial asks again at once, and the ceiling counts dials,
not time. `lan_swap`'s path (`Ssh::swap`, `metalswap::swap`, `Stream::redial`,
`logd`, netd, init) has no change on #535. Main's nightly at 16d2e645 (run
36306830048) was green. Main's nightly at 1ce71831 (run 36290616312) had the
same two findings on `swap_crash_rolls_back`.

Dev host, QEMU 11.1.1, TCG, one named run each: `nightly-green2` at 877b8c95,
`EXIT=0`, 12 dials turned away; `main` at 16d2e645, `EXIT=0`, 11. Not measured:
how fast a KVM guest turns a dial away, and so how many dials fit inside the
gap there.

`cargo run -- --known-red lan_swap` answers NO.

**Exit**: the redial waits on a guest-side event (the diagnostics issue's exit).
Until then `lan_swap` reds on the nightly at a rate. It should go on #542's
disabled list when that lands, citing this file.
