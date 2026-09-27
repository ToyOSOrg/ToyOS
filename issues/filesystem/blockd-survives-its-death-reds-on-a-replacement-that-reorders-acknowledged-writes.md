---
status: open
kind: defect
opened: 2026-09-27
---

# `blockd_survives_its_death` reds on a replacement that rewrites acknowledged writes out of order

The nightly-tier test kills blockd with writes in flight and judges QEMU's
block trace. It has gone red on one signature, the same sectors each time:

```
FAIL blockd_survives_its_death: QEMU's trace, after the reset: Ok([411648]); after the kill: Err("blockd died with the writes at sectors [282600, 281592] acknowledged and no flush after them (and 280584 withheld); the blockd after it wrote [281592, 282600] first")
```

- PR #524's branch nightly at `8c5be843` (run 36273557690), wide and alone,
  with x86-64 linked by toyos-ld;
- PR #532's branch nightly at `a55d62c6` (run 36287592139), wide and alone;
- PR #532's branch on the dev host, `cargo test --test toyos-build -- --nightly
  blockd_survives_its_death`: 1 red in 5.

It passed in main's nightly at `fd62f567` (run 36278449733) and in #532's
nightly at `e4317d3f` (run 36281465192), whose guest binaries are the ones
`a55d62c6` booted. So it is a rate, it predates the rust-lld switch, and
`cargo run -- --known-red` answers NO. Whether the defect is blockd's replay
order or the test's reading of the trace is not measured.

Exit: the ordering the verdict names cannot happen, or the verdict is shown
wrong about it, with the test green across a run of repeats.
