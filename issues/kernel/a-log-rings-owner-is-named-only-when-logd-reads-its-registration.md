---
status: expected-red
kind: defect
opened: 2026-09-27
---

# A log ring's owner is named only when `logd` reads its registration, so a child that floods first takes the owner's slots

`toyos::log::Ring::push` keeps `CHILD_KEEP` shared slots free for the ring's
owner, but only once the owner word is set. `logd` sets it (`origin.rs`,
`ring.own(pid)`) when it reads init's `REGISTER` frame. init sends that
frame after it spawns the program (`userland/init/src/main.rs`, `register`).
Until `logd` reads the frame the owner word is 0, and `push` then keeps
nothing for anyone. A child that starts flooding in that window can take
every slot, the owner's included.

`log_ring_keeps_the_owners_slots` (fast tier) is red this way beside the
other `log_` guests and green alone. Its `/log` holds `===READY===`,
`===TEST_START test_rs_log_flood===` and 1917 flood lines: exactly the ring's
1919 shared slots, with no slot left for test-runner's `===TEST_END`.
`logd: reading test-runner again ... with 1919 of its ring's 1919 records
waiting`.

Rates on the dev host (TCG), `cargo test --test toyos-build -- --nightly
log_`, interleaved per round against `origin/main`'s kernel and tests:
- the branch that filed this, `nightly-green2`: 3 red of 14 (3 of 9 before
  its merge of 16d2e645, one of those in a run before the interleaving
  began; 0 of 5 after);
- `origin/main`: 2 red of 13 (0 of 8 at 1ce71831, 2 of 5 at 16d2e645).
Each red was this failure. The race is on `main`.
Also red on the orchestrator's Fast tier for PR #563 at `d6716fc7`: the same failure.

The fix belongs where the owner is decided:
- init names the owner itself, after the spawn and before the frame. That
  narrows the window and does not close it.
- Or `Ring::push` treats an unowned ring as one in which every writer leaves
  `CHILD_KEEP`, which closes it. That is `toyos/src`, the SDK.

**Exit**: a child writing before the ring's owner is named cannot take the
slots the owner is kept, shown by a test that makes it write in that window.
No test today covers that owner decision in `Ring::push`
(`toyos/src/log/region.rs:202`) or logd's `ring.own(pid)`
(`userland/logd/src/origin.rs:176`); `let keep = 0;` there, or deleting
`ring.own(pid)`, passes every test in the tree, so the exit's test must turn
both mutations red. Owner: `toyos/src/log/region.rs`'s `Ring::push`; held by
the orchestrator.
