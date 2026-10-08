---
status: open
kind: tooling
opened: 2026-10-08
---

# A host reader on another runner image downloads the entry and discards it

`src/cicache.rs` deletes an entry built on another runner image, and the cache
key cannot carry the image. GitHub serves two `ubuntu-24.04` images at once
while it rolls one out: on 2026-10-07 run 37601225884 (09:31Z) and `host`
readers 37646740767 (15:47Z) and 37687777630 (21:12Z) ran on 20261004.327.1,
and runs 37685714260 (20:55Z) and 37693139619 (21:59Z) on 20260927.320.1. So
for the days of a rollout a reader meets an entry of the other image by chance,
whichever image the writer drew that night: both readers above restored
1,813,819,887 B, deleted it and ran cold, 15 and 10 minutes of steps against
the 5 a reader of its own image's entry took in run 37693139619.

Owner: the host cache (`src/cicache.rs`).

**Exit condition.** A reader restores an entry only of its own image, or the
share of `host` runs that discard one is measured over a rollout and accepted
here.
