---
status: open
kind: tooling
opened: 2026-09-18
---

# Four assertions render a reading with no unit beside it, so the `ALONE:` line reports one of them twice as two different failures

`src/alone.rs` decides whether the harness's isolated re-run found the failure
the wide run found. It takes a number out of a sentence only when a unit follows
it, because a number with no unit is an identity — `slot 1` against `slot 2`, an
opcode, a CPU, a count — and two identities are two observations, which is the
larger of the two findings. That rule errs toward "different" on purpose, and
this is the price: an assertion that prints its reading *without* a unit has
that reading read as an identity, so one defect reproduced at two readings is
reported as two failures.

Owned by `src/alone.rs` and the `ALONE:` line in `tests/toyos.rs` that calls it;
nobody is holding it.

## The four sites in this tree

Each writes a headline, so each reaches the classifier.

| site | what it renders | two runs read as |
|---|---|---|
| `tests/common/audio.rs`, `check_physical`'s wake-lateness fault | the same reading twice — `{}us` and `({:.1} pipeline depths)` | two failures: only the `us` copy is masked |
| `tests/toyos.rs`, the dither floor | `only {:.1}% of silent samples are non-zero` | two failures |
| `tests/toyos.rs`, the tone peak | `tone too quiet: peak {}` | two failures |
| `tests/toyos.rs`, the log-drain verdict | `stops at {} bytes` — `bytes` spelled out is not `B` | two failures |

`src/alone.rs`'s `a_reading_rendered_twice_keeps_the_copy_with_no_unit`,
`two_percentages_are_two_failures`, `two_bare_counts_are_two_failures` and
`bytes_spelled_out_is_not_a_unit` assert exactly this, so the limit is pinned
rather than latent: changing it reds those tests and lands as a decision.

## Exit

Either of these closes it, and the four tests above are rewritten or deleted by
whichever does:

- each of those four assertions renders its reading with a unit the scan already
  knows — `bytes` becomes `B`, the pipeline-depth rendering carries one or goes,
  and the percentage and the peak print the quantity they measure; or
- the classifier stops taking an assertion's identity from its text and takes it
  from the assertion's source location instead, which makes the units list and
  this whole class unnecessary.

The second is the real answer and the first is what one landing can do. Neither
has an owner yet.
