---
status: open
kind: defect
opened: 2026-08-15
---

# The shard split prices a task's boot and not the image it builds first

`Shard::keep` partitions the twelve shards by `tests/test-durations`, and that
profile records what a *test* took — the number the ten-second Fast ceiling is
also read against. A machine test that boots a config the shared image does not
cover builds that image first, and nothing prices that build at all.

**Measured on run `31896922288`** (`main` at `e064a96`, twelve KVM shards, the
Fast tier), reading each shard's job log for the wall clock between one `PASS`
and the next:

| task | the gap before it | what the profile says it costs |
|---|---|---|
| `boot_partition_identity` (shard 7, `tests/metalcase`) | **197.8 s** | 5,511 ms |
| `sshd_fail_closed` (shard 4, `tests/sshdcase`) | **145.3 s** | 2,495 ms |
| `desktop_locale_detect` (shard 8, `tests/desktopcase`) | 47.4 s | 3,939 ms |
| `sched_check_build` (shard 8, the `sched-check` kernel) | 32.4 s | 5,879 ms |

The two large ones are configs carrying userland programs the shared test image
does not: `metalcase` starts `compositor`, `netd` and `sshd`, `sshdcase` starts
`netd` and `sshd`, and every one of our crates recompiles in a fresh checkout —
`actions/checkout` writes every source with the current time and cargo's
freshness for a path crate is an mtime comparison, which is deliberate, because
a fresh checkout makes cargo rebuild more than it needs to and never less.

**What it costs the partition.** The twelve `suite` steps of that run:

| shard | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s | 164 | 164 | 171 | **313** | 167 | 160 | **347** | 235 | 159 | 163 | 153 | 170 |

Sum 2,366 s, an even split of 197.2 s, a widest shard of 347 s. Shard 7 is
115.9 s of floor plus one 203.8 s slot the profile priced at 5.5 s. LPT cannot
place what it cannot see: every shard shows `6 parallel task(s)` because the
partition believes it is splitting six 3-to-7-second tests.

**And it believes it succeeded.** Summing each shard's own tests at their
committed prices, that run's twelve bins hold **30.2 s to 32.5 s** — a spread of
2.3 s. The clock's spread over the same twelve is **194 s**. Nothing in the
profile is wrong; the profile is simply not what a shard's wall clock is made
of.

**The A/B that shows the two objectives diverging.** Runs `31900045901` (`main`
at `e064a96`) and `31900050723` (the same tree plus the one-accumulator fix),
both twelve-shard `--nightly` dispatches, minutes apart on the same runner pool:

| | widest priced bin | priced spread | widest phase total |
|---|---|---|---|
| before | 471.6 s | 328.7 s | 369.8 s |
| after | **324.7 s** | **179.0 s** | **380.8 s** |

The partition got 147.0 s better by the number it optimises and 11.0 s worse on
the clock — inside that pair of runs' own noise, but not an improvement. Both
runs place the identical 316 names with no duplicate, and both sum to 2,180.6 s
priced, so this is one partition against another over the same work.

**The bound a correct price would reach is not the even split either.** The
image build is indivisible and attached to its task, so the widest shard is at
best floor + 203.8 + its own test ≈ 316 s against today's 347 s — about 31 s,
and the rest of the 150 s over the even split is the build itself rather than
where it landed.

Two directions, and they are not exclusive:

- **Price a task by build + boot.** The profile cannot simply absorb it: the
  same file is what the `durations` verdict reads against the 10,000 ms Fast
  ceiling, so a task priced at 203 s would red the tier gate it has nothing to
  do with. It wants a second profile — per *config*, not per test — that only
  `Shard::keep` reads.
- **Make the second image cheap.** 145 s to add `netd` and `sshd` to an image
  is a full recompile of those programs, not a relink.

Filed from the CI wall-clock task of 2026-08-15, which measured it while
landing the one-accumulator fix and did not touch it.

## What is missing is the attribution, not the clock

Looked at 2026-09-01, because two readings of this entry disagreed about whether
it is blocked on a ledger that does not exist.

**The clock exists.** `ARTIFACT_BUILD_TIME` (`src/build.rs:26`) is a thread-local
duration the suite already accumulates: `ArtifactBuildTimer::start()`
(`src/build.rs:1282`) charges every cache-missing part of `build_test_image` to
it, and `ArtifactBuildMark::execution_part` (`src/build.rs:48`) *subtracts* it
from a test's measured time, through `tests/common/clock.rs:65` and `:78`. So
the number this entry says nothing prices is measured on every run, deliberately
removed from the price, and then dropped on the floor. What is missing is two
smaller things: nothing attributes that duration to the *config* that caused it
rather than to the worker thread that happened to ask first, and nothing writes
it anywhere. `tests/test-durations` cannot be where it goes — the same file is
read against `FAST_CEILING_MS` (`src/tiers.rs:84`, 10,000 ms), which is this
entry's own reason for wanting a second profile.

**What no work on this host can supply is the seed and the verdict.** The cost
being priced is a *cold-checkout* rebuild on a hosted shard: `actions/checkout`
writes every source with the current time and cargo's freshness for a path crate
is an mtime comparison, so a fresh runner recompiles what a dev host does not.
The 197.8 s and 145.3 s above were read out of a hosted job log by hand, and
nothing in the tree produces them.

**And the verdict is a wall clock this host cannot resolve.** The entry's own
bound on a correct price is *"about 31 s"* off the widest shard, and its own A/B
moved the priced metric 147.0 s while moving the phase clock 11.0 s the wrong
way — so the priced metric is not the verdict and only the twelve hosted shards'
wall clocks are. Two full `cargo test` runs in one session on this dev host,
2026-09-01, over two trees whose diff touches no partitioning: 1012.7 s and
989.3 s. A 23.4 s spread from nothing, against a best case of about 31 s.

So a taker needs, in order: the build clock keyed by config rather than by
thread; a committed per-config profile that only `Shard::keep` reads, merged the
way `cargo run -- --merge-durations` merges the test profile; and **two
twelve-shard hosted runs** — one to fill it and one to say whether the widest
shard's wall clock moved. `Shard::keep`'s unit tests (`src/testargs.rs`) can
judge that a partition stays complete and unique and that the bins total, under
any cost function; they cannot judge whether the partition got better, which is
the whole of what this entry claims is wrong.

`issues/build/there-is-no-attributed-session-ledger.md` is where the first two
of those live: it names this entry as one of its consumers, wants *"image-build
spans with their content key and cache hit or miss"* among the intervals it
records, and puts the shard pricing first because `Shard::keep`'s partitioning
tests are an oracle that already exists. That is the right order and this entry
waits on it. The third — the two hosted runs — is nobody's ledger and is what
says whether any of it worked.
