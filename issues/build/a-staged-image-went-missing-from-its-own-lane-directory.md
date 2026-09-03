---
status: open
kind: tooling
opened: 2026-08-03
---

# A staged image went missing from its own lane directory, and nothing explains it

Two tests in one gate failed on an artifact that is not there, in a run where
238 of 240 passed: `usb_flush_optional` with `read the image: No such file or
directory` and `usb_transport_break` with the same `NotFound`. Both pass alone
(8 s and 4 s). Both are a staged disk image missing from the lane directory
that the same test wrote it to.

What is established, and it rules the obvious explanation out: `lane::dir()` is
`$TMPDIR/toyos-tests-{pid}[/lane-N]`, keyed on the *test process* id, and
**nothing in the tree removes a `toyos-tests-*` directory** — `git grep
toyos-tests-` finds the constructor (`tests/common/lane.rs:47`) and one doc
mention (`src/qemu.rs:31`), and no remover. So a second suite on the host cannot
be deleting the first's scratch by name, and a stale directory from a dead suite
cannot be collected onto a live pid.

That leaves the failure without a mechanism. It is worth an hour from whoever
next touches the harness's staging, because "re-run it" stops being an adequate
answer once the failure can be a missing file rather than a slow one — a slow
test reports the content it was going to assert, and this one reports nothing
about the tree at all.

## What this entry used to be, and why the rest of it went

Filed against `cargo run -- --land`, whose gate ran `cargo test` inside the
integration lock and so made a landing a 14-minute suite serialised against
nothing in another worktree. That framing is retired: `--land` is gone
(`src/main.rs:93` answers with `pr::dispatch_retired_land`), `main` moves
through a merge queue, and the entry's method note about matching `pgrep -f
"toyos-build --land"` went with it.

Its other two shapes have owners now. The boot timeout (`screen_fatal_halt`,
`[qemu] Boot timed out waiting for ===READY===` at 11 s against 3.3 s alone) and
the host-staged timing window the guest slid past (`late_storage_connect`
refusing rather than measuring an ordinary boot) are both
`issues/build/parallel-tests-red-under-other-suites.md`'s class, which carries
the sightings and the two legitimate fix shapes. And the premise the entry
closed on — "the fix is the counting semaphore and nothing yet hands out slots"
— is false: `buildlock::guest_slot` bounds guests to twelve and
`buildlock::build_slot` bounds compiles to four, across every worktree, each
announcing its wait. What neither reaches is stated at `src/buildlock.rs`'s
module header.
