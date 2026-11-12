---
status: open
kind: finding
opened: 2026-11-12
---

# A wake sent on as its home goes idle can leave the home a timer for nothing

Since #1274 a remote claim sends a parked task straight to an idle CPU when
the home's published band (`CpuHandle::band`) says the home runs a task of
the wake's band (`CpuHandles::send_claimed_wake`, `kernel/pure/sched/cpu.rs`).
The home still holds the task's deadline, if it parked with one. It drops an
ended park's entry before it arms its timer (`Due::prune`), so an idle home
does not wake for a park another CPU has taken.

One case gets past the prune. The pass that takes the home idle stores the
band `None` (`Relaxed`) and then reads the task's word in the prune. The
claimant swaps the word to `WakeQueued` and then reads the band. Neither pair
is ordered against the other, so the home can read `Blocked` while the
claimant still reads the busy band. The claimant then sends the task on, and
the home arms its timer for the stale deadline. That timer fires once, and
the pass it wakes finds nothing to do.

This window is inferred from the orderings and has not been observed. The
simulator reproduced the wasted wake that the prune now removes. With the
prune in place, `the_two_cures_are_priced_against_the_pull_path` reads zero
wakes for nothing over its 20 `wakeup_storm` seeds. That suite does not
reorder the home's store past its load.

A candidate close is a sequentially consistent band store, prune load, claim
and band load, which forbids the store-buffering outcome. On x86-64 that adds
one `xchg` on a pass that goes idle. Loom does not model `SeqCst` fully, so
it cannot be the check for that close.

**Exit**: the window is closed by an ordering both sides keep, with the
argument at the site.

**Owner**: the scheduler's wake path (`kernel/pure/sched/`).
