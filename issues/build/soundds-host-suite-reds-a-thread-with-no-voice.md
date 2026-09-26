---
status: open
kind: defect
opened: 2026-09-26
---

# soundd's host suite reds: a test's `flush` says a line from a thread with no voice

`cargo test --manifest-path userland/soundd/Cargo.toml --target <host>` exits
101 on `origin/main` (checked on a `git archive` of it) as on #511. One test
fails, `mix::tests::published_totals_are_exactly_the_sum_of_every_window`:

```
panicked at soundd/src/say.rs:100:28:
soundd: a thread with no voice said "soundd: wakes=0 completions=0 submitted=10 underruns=1 drains=2 ..."
```

`mix::flush` says its window's line through `say!`, and `say::said` panics
unless the calling thread was made a voice (`say::start` or `say::speak_as`).
The test thread was made neither. soundd is in `src/ci.rs`'s
`USERLAND_HOST_CRATES`, so the nightly's `host-full` reds on this, and the PR
`host` job does not run it.

Exit condition: the suite exits 0 with the test still reading the totals
through `flush`. Either the test gives its thread a voice, or `flush`'s line
leaves through a seam a test can hold.
