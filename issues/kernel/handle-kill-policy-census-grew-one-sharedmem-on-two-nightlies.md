---
status: open
kind: defect
opened: 2026-09-27
---

# `handle_kill_policy`'s census grew by one `SharedMem` on two nightlies in a row

Main's nightly at 16d2e645 (run 36306830048, `guest (8)`) and PR #535's at
a4f68c5a (run 36314576406, `guest (8)`), KVM, QEMU 11.1.0, both red with
byte-identical text, numbers included:

```
16 more killed processes left more live objects behind: [("SharedMem", 9, 10)] — first PipeRead 6, PipeWrite 5, Connection 2, Device 1, Acceptor 5, Inbox 6, SharedMem 9, ...
```

Both had `ALONE handle_kill_policy: GREEN`. It was green on the nightlies at
1ce71831 (run 36290616312) and at c2715880 (run 36297455432). c2715880 already
carries 16d2e645, so this is a rate on `main`'s code. The actuator boot it
shares carried the same eight tests in all four runs, and test-runner runs
one job at a time.

Dev host, QEMU 11.1.1, TCG, one named run each: `nightly-green2` at 877b8c95,
`EXIT=0`; `main` at 16d2e645, `EXIT=0`.

What is known: every holder the test kills holds one `SharedMem` region and
one pipe, and `settled_census` answers once two readings 10 ms apart agree.
The mechanism
`issues/kernel/deferred-release-outlives-its-syscall.md` records is a release
still in flight on another CPU when both readings are taken. That would read
exactly like this: one killed holder's region is not yet released at the
second census. The red boot's kernel reports a TLB shootdown wait of up to
13667 us (`tlb: … max=13667us`), longer than the settle's 10 ms.
Not shown: which process held the tenth `SharedMem`, or that its release was
the one in flight.

`cargo run -- --known-red handle_kill_policy` answers NO.

**Exit**: the census names the owner of a grown kind, and the red is
attributed or the release is shown to finish before `wait` returns. Until then
`handle_kill_policy` reds on main's nightly at a rate. It should go on #542's
disabled list when that lands, citing this file.
