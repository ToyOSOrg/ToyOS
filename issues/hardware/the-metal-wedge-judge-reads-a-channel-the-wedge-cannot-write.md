---
status: open
kind: tooling
opened: 2026-09-14
---

# The metal wedge judge asserts two records on a channel a wedge cannot write to, so its arm has never passed

`tests/common/power.rs`'s `deadline_wedge_chain` — the judge
`boot_deadline_ends_a_wedge`'s metal arm runs (`tests/toyos.rs`, `arms:
&[metal::once("deadlinewedge", ...)]`) — opens with

```rust
kernel.must_say(bootlog::WEDGE_STAGED)?;
kernel.must_say(bootlog::WEDGE_ARRIVED_DEAF)?;
```

`kernel` is `Readback::kernel()`, which is the concatenation of `logd`'s files
off the stick. Both records are written by `kernel/src/deadline.rs` after every
CPU has stopped taking scheduler passes, so `logd` is already wedged and neither
can reach a file it writes. The two records do cross, on the sealed page, and
the same function asserts them there four lines further down —
`after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::WEDGE_STAGED)` — which
is the channel that carries them.

## Measured

Three readbacks, one command each
(`cargo test --test toyos-build -- --metal --metal-readback <dir>
boot_deadline_ends_a_wedge`):

| readback | `wedge: staged` in `kernel.log` | in `loader.log` | judge |
|---|---|---|---|
| run 50 `deadlinewedge` | 0 | 1, at line 253, under `Previous boot's panic:` at line 34 | EXIT=1 |
| run 44 `metal-r2-wedge-deadlinewedge` | 0 | 1 | EXIT=1 |
| run 44 `metal-ctl-wedge-deadlinewedge` | 0 | 0 | — |

The refusal is the same on both: `"wedge: staged, and only the boot deadline
ends this machine" never reached the deadlinewedge's kernel log`. The boots
themselves are sound — run 50 read `PASS: the machine booted ToyOS in 1164 ms`,
EXIT=0, and every number the profile prices for that boot passed its ceiling.

The control arm's zero in both columns is a second fact and not this one: that
page kept the oldest records that fit, which is
`issues/diagnostics/a-wedged-boots-record-outgrows-both-channels-that-carry-it.md`.

`cargo run -- --known-red boot_deadline_ends_a_wedge` answers `NOT ON THE LIST`,
so nothing declared this. `src/tiers.rs`'s relegation record for the name says
"the metal arm is the machine's own", which reads as an arm that runs.

`power::hard_lockup_chain` beside it was already moved off that channel for this
reason and says so at the site; the wedge judge was not.

## Exit condition

A `deadlinewedge` readback on which `boot_deadline_ends_a_wedge`'s metal arm
passes, with the two records asserted on the channel that carries them — and
nothing asserted on `kernel.log` that a wedge cannot write to it.
