---
status: open
kind: tooling
opened: 2026-09-27
---

# A ROOT image rebuilt at the blocks its estimate used can run out by one

`src/image.rs`'s `create_root_image` formats ROOT twice: once at an estimate,
then again at the estimate's used blocks rounded up to `PARTITION_ALIGN`, on
the premise that "fewer blocks need no more bitmap, so the second build fits in
what the first used". The premise is not held: nightly run 36281465192's
`guest (1)` lane, on PR #532's branch at `e4317d3f`, built a shared C-corpus
boot of 47 binaries (40 MiB) whose second build ran out on the last entry it
wrote:

```
thread '<unnamed>' panicked at src/image.rs:77:33:
root: failed to symlink 'bin/tone' -> '/system/bin/toybox': NoSpace { requested: 1, available: 0 }
```

Every test on that boot then failed without a guest (`30_hanoi` onward, 50 in
the lane). The same branch's local fast tier grouped the corpus as 48 binaries
and passed it, so whether a set trips it depends on the exact bytes and
grouping. Why the second layout needs a block the first did not is not
measured; the rounding to `PARTITION_ALIGN` gives most sets slack. PR #532's
binaries, linked by rust-lld, are the first set seen to run out.

Exit: the second build's size is one the first proves sufficient (or the
first build's image is kept and trimmed), and a host test builds a set that
lands on the boundary.
