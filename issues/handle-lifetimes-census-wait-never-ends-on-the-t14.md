---
status: open
kind: defect
opened: 2026-10-28
---

# `handle_lifetime`'s census wait never ends on the T14

In the T14's shared-debug boot, `test_rs_handle_lifetime` reaches
`kill_releases_ring` and never leaves it. The ring holder is killed at once,
then the test sleeps in `census_wait::released_to` until the 62.1 s list
bound ends it. `test_rs_ring0_timer_in_syscall`, `test_rs_shm_release_reclaims`,
`test_rs_spawn_child_ends_first` and `test_rs_spawn_lands_claimed` come after
it and never run.

Evidence (T14 readbacks):
- 2026-10-28: every boot at 576bebf3a, 7ef0a48c8, e33b248a2 and the
  igpu-fault head does this, nine boots in all.
- The last pass on record is 1595bbbcd (2026-10-23). There the same wait
  returned within a millisecond of the kill.

What differs between them is the machine: every failing boot's log has
acpiserver taking embedded-controller query `0x4f` on an SCI about once a
second (`acpiserver: 31 SCIs; embedded controller queries taken: 0x4f x14`).
The passing boot has no such line. Nothing in acpiserver's SCI path keeps an
object per event.

The likely cause is the oracle, and that is an inference. The census is
machine-wide, as `kill_releases_ring`'s own doc says. A daemon holding one
transient object while it waits between events reads as a leak whenever the
first reading caught it holding none. `released_to` then returns only if a
later reading also falls in that gap.

`census_wait::released_to` now names the kinds still grown, once a second,
so the next T14 shared-debug boot shows which kind and how many.

Owner: the tests that read the census (`tests/guest/rust-tests/src/census_wait.rs`,
`handle_lifetime`, `shm_release_reclaims`).

**Exit**: the grown kind is named from a T14 boot. The reclaim check then
judges what the killed holder made, and nothing a daemon makes beside it
reaches the verdict. Five consecutive T14 shared-debug boots run all five
members.
