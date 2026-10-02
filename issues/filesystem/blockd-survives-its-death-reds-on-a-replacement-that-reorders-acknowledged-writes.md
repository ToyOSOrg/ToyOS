---
status: open
kind: defect
opened: 2026-09-27
---

# `blockd_survives_its_death` reds on a replacement that rewrites acknowledged writes out of order

The test killed blockd with writes in flight and judged QEMU's
block trace. It went red on one signature, the same sectors each time:

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
`a55d62c6` booted. So it is a rate, and it predates the rust-lld switch.

Each of those reds is the verdict of a judge that held the replacement to the
order of every write. `17bb26416` (#534) holds it to the order of the writes
that overlap, and none of `8c5be843`, `a55d62c6` and `59bd29ff2`, where this
was filed, contains it. The two writes are 1008 sectors apart and a request is
at most 256 long: #534's comparison passes the trace above, and the one before
it does not. The client puts two such writes on the wire together, and NVM
Express 1.4 §6.3 orders no two commands outstanding at once; `64f58547c`'s
message has how QEMU reorders them. A pass prints the order the first blockd
wrote in and not the replacement's, so no recorded run shows #534's judge a
reordered trace.

`520c0d129` (#660) cut the test and its judge from the guest suite, and nothing
that runs reads a device's trace. Stage D of
`issues/build/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md` owes
blockd's restart to a host test: that is the test this exit waits on.

Exit: the ordering the verdict names cannot happen, or the verdict is shown
wrong about it, with the test green across a run of repeats.
