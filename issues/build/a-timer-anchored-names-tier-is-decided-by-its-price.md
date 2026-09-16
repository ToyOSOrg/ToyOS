---
status: open
kind: tooling
opened: 2026-09-07
---

# A timer-anchored name's tier is decided by its price, not by the classification

The track registered thirteen names with `UNMEASURED_MS` markers and no CI had
ever run them. Pull request #431's own `durations` job priced all thirteen on
twelve hosted shards, `tests/test-durations` carries those numbers, and the four
over `FAST_COMMIT_MS` moved to `Tier::Nightly` with a `Why::TimerAnchored` row
each: `panic_key_holds` (42,033 ms), `blackbox_panic_chain` (11,407),
`loader_watchdog_arms` (10,064), `panic_reboots` (9,768). The other nine are
Fast at 2,470 to 6,258 ms. **That part is closed.**

What is left is a rule the prices did not settle. `FAST_CEILING_MS`'s boundary
(2026-08-12) is a *classification*: a test whose verdict or duration is anchored
to real time — such that a 2x slower machine would change its verdict or price —
belongs Nightly, and `Why::TimerAnchored`'s own doc says a row's label may
measure anything at all "over the line or nowhere near it, and neither moves
it". Two of the nine that stayed Fast are anchored by exactly that test:

- **`panic_before_peripherals_reboots`** (3,227 ms) — the same verdict as
  `panic_reboots`, which is now Nightly: QEMU's stop reason arriving inside a
  bound the guest printed. The two differ in where the panic is taken, not in
  what decides them.
- **`job_deadline_reboots`** (4,806 ms) — its verdict waits out a staged window,
  the runner's job list running past `toyos_tco::JOB_BOUND_MS`.

So the tier of a timer-anchored name is currently decided by its price, and the
classification the boundary states is not what placed it. Either the two above
belong Nightly by the rule as written, or the rule means something narrower than
its words and says so. Nobody has decided which, and the cost of guessing is
real in both directions: relegating removes the only per-pull-request gate on a
panicked kernel ending its own boot, and leaving them Fast lets a slow shard
red a name for a reason its author cannot act on.

`src/tiers.rs`'s `RELEGATED` rows say what left the per-PR tier. The one price
this run could not enforce is a different name's — `sysret_ss_reload` measured
26,927 ms and remains Fast, which the merge printed as a warning because this
change neither registered nor re-tiered it.

**Exit condition**: the owner's answer on whether the boundary is a
classification or a cost rule, applied to `panic_before_peripherals_reboots` and
`job_deadline_reboots`, with the wording at `FAST_CEILING_MS` matching whichever
it is.
