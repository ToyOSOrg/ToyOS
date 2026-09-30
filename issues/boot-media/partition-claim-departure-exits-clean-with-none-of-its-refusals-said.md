---
status: assigned
kind: tooling
opened: 2026-09-27
---

# `partition_claim_departure` exits 0 having said none of its refusals

Seen on 2026-09-27 in the nightly run 36336701867, job `guest (5)`, on PR
#541's head `1c0f0c753fb2c4c2d01caf51dc67b7f92ca4cd8e` — a diff that touches
none of `tests/common/partclaim.rs`, `tests/toyos-rust-tests/src/bin/partition_claimant.rs`
or the kernel's partition-claim code:

```
FAIL partition_claim_departure: departure: the guest exited 0 having said 0 of its 1 refusals:

  FAIL  partition_claim_departure  (2s)
```

Re-run alone, immediately after, in the same session: green, with every role's
own line printed —

```
  [partclaim] departure: 1 told; [kernel 0.757 cpu0] usb-quiesce: disk 0 SYNCHRONIZE CACHE ok
  [partclaim] silent: 1 told; [kernel 0.710 cpu0] usb-quiesce: disk 0 SYNCHRONIZE CACHE ok
  [partclaim] untold: 0 told; ...
  PASS  partition_claim_departure  (7s)
  ALONE partition_claim_departure: GREEN, and it was alone both times — nothing
  the harness controls differed, so it failed once and passed once. That is a
  rate and not a classification.
```

## What is known

The failure is `guest_verdict`'s (`tests/common/partclaim.rs`), on the
`departure` role — the first of the three `departed()` iterations in
`partition_claim_departure`. Its message, `"the guest exited 0 having said
{said} of its {refusals} refusals:\n{stdout}"`, prints with an empty tail: the
guest's own captured `stdout` held nothing at all — not one of the `departure`
role's own `println!`s (`"a write is reported and not flushed"`, `"the write
the device left under completed"`, the `refused with` lines `said()` prints,
or `"partition_claimant: PASS"`), yet the guest's exit code was 0.

An exit of 0 rules out a panic on a wrong assertion (`departure()`'s
`assert_eq!`s all `panic!` on mismatch, and a panic does not exit 0), so the
guest's own logic is not shown to have run into an unexpected state.

This is the same family flagged in
`issues/boot-media/partition-claim-gives-up-reds-beside-other-guests-and-is-green-alone.md`
— same test area, and that file already widens its scope to
`partition_claim_departure`. It is filed separately rather than folded in because the failure signature
differs: that file's evidence is a kernel-log line count coming up short
(`"{count} flushes were told of the loss, not {told}"`, matched against
lines the kernel actually printed) on the `silent` role, attributed to a
hypothesised unscoped global fsync deadman race; this is a `guest_verdict`
failure on the `departure` role with the guest's entire stdout capture
missing, which that hypothesis does not by itself explain.

## Exit condition

`guest_verdict`'s "exited 0 having said" refusal (`tests/common/partclaim.rs`)
carries the kernel window, as its non-zero-exit refusal already does, so the
next sighting is not blind; the mechanism named and fixed; and a test that
turns red on it deterministically. Then the test is restored and this file is
deleted.

## Owner

The partition-claim code, held by the orchestrator.

**Its test is deleted**: `0e19bf898` took `partition_claim_departure` out,
host and guest halves, and `git revert 0e19bf898` brings it back as it stood
before #536; `git show 84471bc58:tests/common/partclaim.rs` holds #536's
adaptation of it.
