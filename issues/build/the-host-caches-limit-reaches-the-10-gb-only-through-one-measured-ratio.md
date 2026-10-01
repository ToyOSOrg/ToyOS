---
status: open
kind: tooling
opened: 2026-10-01
---

# The host cache's limit reaches the 10 GB only through one measured ratio

`src/cicache.rs` refuses a tree whose `PATHS` hold more than `LIMIT`,
8,000,000,000 B, uncompressed. The repository evicts its caches past 10 GB of
what actions/cache stores, compressed, and both two host entries beside a guest
one (2H + G) and one beside two (H + 2G) must fit.

One ratio ties `LIMIT` to H, measured once. Run 36878222090 (e0ced587c) sealed
5369 MiB of targets, at least 5,629,804,544 B. That head's own archive of the
cache's paths, made with the runner's `tar` and `zstdmt`, held 1,403,326,566 B:
a ratio of 4.01. At that ratio `LIMIT` stores at most 1,994,138,951 B. With the
guest entry's 3,281,375,938 B (run 36696295750's `tcg`), 2H + G is
7,269,653,840 B and H + 2G is 8,556,890,827 B. The ratio may fall to 2.38
before 2H + G reaches 10 GB. The lowest measured is 3.08: run 36844536500
sealed 9553 MiB on macOS and saved 3,250,736,567 B.

No gate reads a stored size, and the tree no longer runs that `tar` or
`zstdmt`. If the targets compress worse, H moves toward the floor and nothing
reds.

Owner: the host cache (`src/cicache.rs`).

**Exit condition.** A gate reds on the stored size itself: after nightly's
`host` saves, a step reads that entry's stored bytes and the newest guest
entry's from the repository's cache list, and fails when 2H + G or H + 2G
passes 10 GB.
