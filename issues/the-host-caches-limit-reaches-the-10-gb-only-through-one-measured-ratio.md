---
status: open
kind: tooling
opened: 2026-10-01
---

# The host cache's limit reaches the 10 GB only through one measured ratio

`src/cicache.rs` refuses a tree whose `PATHS` hold more than `LIMIT`,
8,000,000,000 B, uncompressed. The repository evicts its caches past 10 GB of
what actions/cache stores, compressed, and two host entries must fit beside
every toolchain layer a scope holds (2H + T): main's, each pull request's that
moved one, and a merge group's sysroot.

One ratio ties `LIMIT` to H, measured once. Run 36878222090 (e0ced587c) sealed
5369 MiB of targets, at least 5,629,804,544 B. That head's own archive of the
cache's paths, made with the runner's `tar` and `zstdmt`, held 1,403,326,566 B:
a ratio of 4.01. At that ratio `LIMIT` stores at most 1,994,138,951 B. One set
of the toolchain's four layers is 918,708,556 B (run 36934214557's saves), so
with main's set alone 2H + T is 4,906,986,458 B, and five more sets fit beside
it. With that one set the ratio may fall to 1.76 before 2H + T reaches 10 GB.
The lowest measured is 3.08: run 36844536500
sealed 9553 MiB on macOS and saved 3,250,736,567 B.

Two more Linux ratios, both above it: run 37190643147 (d47b383cf) sealed
7,605,844,073 B and saved 1,813,819,887 B, 4.19; run 37693139619 sealed
8,608,140,775 B, and `tar | zstd -T0` of its paths held 2,020,684,970 B, 4.26.

T is not one set. On 2026-10-07 the repository's cache list held 10,948,823,786 B
in 25 entries, past the 10 GB already: one host entry, main's four layers
(912,484,904 B) and twenty sysroot layers of about 420 MB, one per pull request
or merge group that moved the sysroot's key. GitHub evicts by last access and
removes an entry unread for seven days, so what goes first is a layer whose
pull request has merged; nothing here measures that it is.

The seal counts more than the save stores. A hard link is counted once per
name, and `tar` stores its bytes once: in run 37693139619 the driver's binary
(262,085,512 B), the build system's and the harness's binaries under
`target/debug` (150,775,984 B) and the apps under `userland/target`
(108,795,168 B) were each counted twice, 521,656,664 B of the 8,608,140,775 B.

The entry stores files every read rewrites: the driver's binary and its path
crates' libraries, 414,270,068 B in that run with the driver's full debuginfo,
since a checkout dates every source after the entry and cargo builds the driver
before the read.

No gate reads a stored size. If the targets compress worse, H moves toward the
floor and nothing reds.

`LIMIT` is checked only by nightly's `host`, the one job that saves a host entry,
before it seals its tree. A pull request's run and the merge queue's, warm or
cold, never seal and are never refused by `LIMIT`. So a landing that takes the
cold tree past `LIMIT` is first refused by the next nightly's seal, loudly:
that run is red and saves nothing. Until an entry is sealed again, a pull
request restores the last sealed one, or runs cold once the runner image has
moved; either way its verdict is its steps'.

Owner: the host cache (`src/cicache.rs`).

**Exit condition.** Two gates that red:
- after nightly's `host` saves, a step reads that entry's stored bytes and every
  toolchain layer's from the repository's cache list, and fails when 2H + T
  passes 10 GB;
- the merge queue's `host` reds a landing whose cold tree passes `LIMIT`,
  before `main` moves;
- the seal counts a file's bytes once however many names it has, and the entry
  holds no file every read rewrites.
