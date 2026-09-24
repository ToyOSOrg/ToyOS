---
status: open
kind: tooling
opened: 2026-09-24
---

# The committed shard profile has no refresh path

`tests/test-durations` is what every nightly shard prices its partition
against (`tests/toyos.rs`'s `shard_pricing`), and nothing writes it any more:
the shards no longer upload their measurements and `--merge-durations` is
gone with `src/durations.rs`. A test added after the file's last refresh is
priced at the harness's default, and one whose cost moved keeps its old price.

What that costs is balance, never a verdict: every name still lands in exactly
one shard (`check_shard_partition`), and a stale price only makes one shard's
wall clock longer than another's. The nightly's `guest` jobs carry each
shard's elapsed time in their logs, which is the measurement that says when
the imbalance matters.

The exit is either a refresh that needs no CI machinery — a nightly shard's
own log carries every test's elapsed time — or a partition that does not need
a profile at all.
