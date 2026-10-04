---
status: open
kind: defect
opened: 2026-09-28
---

# `watch-window` spins out a hold whose poster is queued behind it on the same CPU

`kernel/src/watch.rs`'s `window::hold` spins in Ring 0 until the waiter's
notified bit is set or its 50 ms `WINDOW` lapses, and nothing in the loop is a
preemption point: a Ring 0 timer fire only sets `need_resched`, and an ordinary
wake to a CPU that is not asleep rings no IPI (`Urgency::Normal`) and is
drained at that CPU's next pass. So when the task that would post is queued on
the holding CPU, or its wake waits in that CPU's mailbox, the post cannot land
and the hold always lapses.

`blocking_read_stress` is a strict ping-pong, so this is self-sustaining once
its two processes share a CPU: each half-trip's reader holds 50 ms with its
writer queued behind it, then parks, and the writer runs. The idle CPU's steal
probe cannot break it, because `answer_steal_requests` hands over nothing at
`fair_len() <= 1` and at most one of the pair is runnable at any pass. The
recorded reds fit it: `only 27 of 500 round trips completed inside 3s` is
about 111 ms a round trip, two lapsed holds. In that run (#536 at
`06c6195f`) the echo child made 28 reads and spent `cpu=1471ms`, about
50 ms each; the parent spent `cpu=1665ms`.

Neither that the pair shared a CPU nor how it came to was observed; the guest
prints no placement for it. This is read from the code and one run's numbers,
not reproduced.

**Exit**: a hold gives its CPU up while that CPU owes a pass or has work queued
(`CpuHandle::doorbell().kick_pending()`, `CpuHandle::load()`), so a post from a
task on the same CPU lands in it; and `blocking_read_window` shown green at a
rate on a guest whose canary pair shares a CPU.

`watch-window` and `blocking_read_window` are deleted; `issues/blocking-read-window-reds-beside-other-guests.md` records the commit that restores them.
