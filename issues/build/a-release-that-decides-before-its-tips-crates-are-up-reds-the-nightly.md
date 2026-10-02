---
status: open
kind: tooling
opened: 2026-10-02
---

# A release that decides before its tip's crates are up reds the nightly

The nightly's `release` puts a toolchain up only where crates.io's newest SDK
crates are the tree's own (`release::sdk_at_tip`). Where HEAD is main's tip
and they are not, it refuses: "crates.io holds no `<crate>` of this tree,
main's tip, so no sdk alias can name it". That refusal does not tell a tip
`publish.yml` failed to publish from one whose `publish` run has not finished,
and on the second the nightly is red for no defect of the tree. Nothing is put
up, and a re-run of that one job after the publish recovers it.

The decision follows the nightly's `toolchain` job, 2:38 at its fastest
measured (run 36988764929, attempt 2), and the `release` job's checkout,
restore and driver build. Main's six green `publish` runs of 2026-10-02 took
between 1:04 and 1:54 from creation (36985281365, 36986108744, 36986326239,
36991892914, 36995618637, 37000495781). `publish` runs one at a time
(`publish.yml`'s `concurrency`), and after each crate it reads the index up to
60 times, 5 s apart (`ci::publish`). A publish that queues or waits longer
than that margin, behind a landing made just before the nightly, is read as a
missing one.

Owner: the release module (`src/release.rs`).

**Exit**: a `release` at main's tip is red for crates crates.io does not hold
only once that tip's `publish` run has concluded: a test stages a tip whose
crates come up after the release's first read of crates.io, and sees the
release put up.
