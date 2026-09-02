---
status: open
kind: defect
opened: 2026-08-27
---

# A twelve-wide suite reds a FAT-volume checker at a rate, on the placement branch and on `main` alike, and one session could not separate them

Measured 2026-08-27 on the dev host as a same-session A/B, six full `cargo test`
runs an arm, interleaved, `wt/toyos-freeze` at `8a7b82ee` against `origin/main`
at `16c05999`. Every run in both arms completed in 58-79 s, which is the same
instrument twice; every red below reported `ALONE … GREEN` from the harness's
own re-run.

| arm | reds |
|---|---|
| branch | `fat_backing_revoked` ×3, `device_claim_lifetime` ×1 |
| `main` | `esp_filesystem` ×1 |

`fat_backing_revoked` says the same thing each time:

```
the unlink-and-reallocate cycle left the log volume breaking the format:
1 cluster(s) from 20 are marked allocated and no directory entry reaches them
```

— one leaked cluster on the `/log` volume, found by `toyos-fat32-check` after
the guest has shut down. `device_claim_lifetime` is `exit code Some(101)` from
its guest binary. `esp_filesystem` is the third name of the same shape: a volume
checker complaining after a loaded run.

## What this does and does not establish

**Not separable at this sample size.** 3 of 6 against 0 of 6 on one name is
p ≈ 0.18 by Fisher's exact test, and the two arms produce the *class* at
1 of 6 and 4 of 6 — a difference no six-run pair can decide. Nobody may read
this file as either "the placement change caused it" or "the placement change is
clear".

**The coupling is real and is named rather than denied.** The change under test
(`CpuHandle::answering`) refuses a CPU whose doorbell edge has stood longer than
a pass may take, and at boot several programs are spawned before any CPU has run
a pass — so on a two-CPU guest the boot burst spreads differently than it did.
A volume test whose verdict depends on when `iod` drains relative to an unlink
can change phase on that alone. That is a mechanism, not evidence; what it means
is that this name cannot be waved through on a scheduler branch.

**A decisive A/B needs a quiet host and this one stopped being quiet.** Six more
runs an arm were started and abandoned: the first took **417.4 s** against the
58-79 s of every run above, and `pgrep` found three other worktrees' suites on
the box. `tests/CLAUDE.md`'s rule is that a block which gains company mid-run is
discarded and re-run, never corrected, so it was discarded.

## What to do with it

Re-take the A/B on a quiet host, six runs an arm or more, and record the rates
here. If the branch arm separates, the question is which of the two mechanisms
below it is; if it does not, `fat_backing_revoked` inherits
`issues/build/parallel-tests-red-under-other-suites.md`'s standing class and
this file folds into that one.

The two mechanisms worth naming before anybody measures again:

- **A leaked cluster is a write-back ordering question**, and the write-back
  queue is young: `iod` drains dirty pages after the closing thread has already
  returned, so an unlink that races the drain has a window that did not exist
  before the queue landed.
- **Or it is the host.** Three of the five reds across both arms are on tests
  that read a volume back *after* QEMU has gone, so anything that shortens the
  guest's shutdown under load reaches all of them the same way.

## A fourth name of the shape, on one arm of a two-arm pair

**2026-09-01, `w5b5-host-build` at `dbc7d610` against `627e5f0f`**, two full
`cargo test` runs on this dev host, one an arm, back to back. Each arm reported
exactly one red and each red was `ALONE … GREEN`:

| arm | red | the host it ran on |
|---|---|---|
| branch | `fs_dirs_durable` | fastest boot 1629 ms, 1.23x width |
| `627e5f0f` | `i8042_undecoded_bytes` | fastest boot 3517 ms, 2.66x width |

`fs_dirs_durable` is the fourth name of the shape this entry is about, and it
says the most of any of them:

```
the staged directories left the log volume breaking the format:
FAT 1 differs from FAT 0 at entry 34: 0x00000000 against 0x00000023. BPB_ExtFlags
has mirroring on, so every copy must carry every update
/2026-09-01-202502.log: DIR_FileSize is 14307 bytes, which needs 28 clusters, and
the chain holds 29
2 cluster(s) from 34 are marked allocated and no directory entry reaches them
FSInfo: FSI_Free_Count is 68522 and the FAT has 68519 free clusters
```

Four complaints at once, and every one of them is `/log` mid-write: an
unmirrored FAT entry, a chain one cluster longer than the size, two clusters
allocated and unreachable, and an `FSI_Free_Count` three off. That is the
signature of a volume read back before its writer finished, not of four
independent format faults --- which is the first of the two mechanisms this
entry already names, seen from a fourth angle.

One run an arm decides nothing about a rate, and neither arm was quiet in the
same way, so this is a sighting and not a measurement. **It is not a both-arms
pair for this name**: the volume checker reds on the branch arm only, and what
the base arm produced is the wider *parallel-classification* red under a name
that is not a volume checker at all. Two runs cannot tell the two apart.
