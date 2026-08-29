---
status: open
kind: defect
opened: 2026-08-29
---

# A released thread's `publish_released` post can strand a parked joiner, and the exit path's early post is what masks it

`process::thread_exit` posts `Gone(Closed)` on the exiting thread's watch
before `exit_current`, and `TaskHandle::publish_released` posts the same
outcome on the same watch when the exit pass drops the payload
(`kernel/src/sched/payload.rs`).
`issues/kernel/thread-exits-completion-post-is-the-second-one.md` asks which of
the two is load-bearing, and `toyos-proclife`'s model answers that the release
post alone releases every joiner in every interleaving it can build.

The machine answers otherwise. Deleting the early post — so a
`SYS_THREAD_JOIN` rests solely on `publish_released` — stalled
`port_poll_churn` in 2 of 2 twelve-wide fast-tier runs on the dev host
(2026-08-29), and the same tree with the deletion reverted ran the same
composition green in the same session:

- run 1: timed out after 300 s "still talking" — the talking was the kernel's
  periodic reporter, repeating `sched: cpu=0 ready=0 dying=0 parked=3
  current=None` for minutes: three tasks parked, nothing runnable, nobody
  woken again.
- run 2: STALLED, the guest silent for 179 of its 180 s guard; the last kernel
  line is `exit: test_rs_port_poll_churn tid=2 code=0` — a churn thread's
  clean exit, after which its joiner never ran.

So in some interleaving the release-side post is not delivered to, or not
armed by, a parked joiner, and the early post is today the only cover. The
model cannot exhibit the loss: `World::retire` posts unconditionally, which
encodes the *intent* of `Hw::release` + `publish_released` and not whatever
the kernel's pass and inbox path actually drops. Where the wake is lost — the
arm/post race in `completion`, the release pass's context, or delivery into
the task inbox — was not localized; the A/B and the `parked=3` signature are
the whole of the evidence.

What a fix owes: localize the lost-wake window on the release path and close
it there, with a control that reds without the early post — after which the
sibling entry's question answers itself and both files close together.
