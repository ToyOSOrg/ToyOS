---
status: open
kind: tooling
opened: 2026-09-28
---

# A forward's redial negative control counts a live listener's transient refusal

`metaltalk::tests::a_redial_on_a_forward_that_refuses_ends_at_the_refusal`
(PR #566) asserts `stream.turned_away() == 1` after dropping a live listener
and redialling it. On a host carrying other concurrent suites, `cargo test
--lib` reds on it intermittently: `assertion left == right failed: left: 2,
right: 1` (`src/metaltalk.rs:1620`), reproduced twice in six runs at
`f8283ca0` on a host at load average 6.6–15.1 across 14 cores, running another
worktree's `cargo test --lib` and a QEMU guest at the same time; the same test
passes 5/5 run alone and passes on an otherwise-quiet host.

`open`'s very first dial (`again` false) retries a failed connect with no
ceiling check below `TURNED_AWAY_CEILING`, so a transient refusal from a
listener that is genuinely live and about to accept — an accept-queue drop
under host contention, not the machine going away — is still counted in
`turned_away` (`count_or_give_up` increments before its match). If that
happens once before `read_first`'s connection is finally accepted, the test's
later, deliberate refusal is the *second* count, and `turned_away() == 1`
does not hold even though the redial itself still ended at its own first
failed connect, as designed.

Evidence: `flake-red-1.log`, `flake-red-2.log` (both `assertion left == right
failed: left: 2, right: 1`, this test only, all other 385 tests green); five
consecutive isolated runs of the same test green (`retry-1.log`…`retry-5.log`);
two more full-suite runs green (`gate-lib-2.log`, `gate-lib-3.log`). All at
`f8283ca0` on `wt/toyos-redial`, in the job scratchpad.

## Exit condition

The test's count no longer conflates a retry against a listener that was
still live when dialled with the deliberate refusal it means to measure — for
example by asserting on the redial's own turned-away count (taken right
before `redial()`, subtracted at the end) rather than the stream's lifetime
total — and is shown green across several runs made to overlap another
`cargo test --lib`.
