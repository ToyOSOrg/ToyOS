---
status: open
kind: defect
opened: 2026-08-01
---

# What the adapter does *not* re-check about the partition table

`toyos-gpt`'s own residuals — a `last_usable_lba` that may cover the backup GPT,
and two entries in one table sharing a unique GUID resolving first-wins — are
the parser's to fix. The
adapter deliberately does not duplicate them: it cannot know whether an extent
is *right*, only whether it is being respected, and two copies of a rule that
can disagree is worse than one. What it does enforce is that no I/O leaves the
extent it was given — and, tighter, that none leaves the FAT volume inside it,
since `Fat32::probe` reads the sector count before anything can write. That
stance is stated at the site now, in `kernel/src/fat32_adapter.rs`'s module
header under gate 2.

**Both parser residuals were re-checked on 2026-08-24 and both reproduce**,
against `toyos-gpt/tests/parse.rs`'s builder on its default 2048-LBA disk with
`backup: true` (probes written, run, and reverted — they are witnesses, not
tests):

- `last_usable_lba` set to 2046 covers the backup entry array, which sits at
  2015..=2046 below the backup header at 2047. `locate` returns
  `Ok(first_lba: 300, last_lba: 2046)` — a partition the adapter would then
  faithfully confine writes to, and which contains the backup GPT.
  `parse_header` bounds `last_usable_lba` only by `lba_count`; the
  `entry_array_lba <= last_usable_lba` check applies to the header being parsed,
  so it fires for a backup header and never for a primary one. `check_no_overlap`
  compares the matched partition against other *partitions* and not against the
  table's own blocks.
- Two entries carrying the same `unique_guid` still resolve first-wins:
  `scan_entries` takes the match only `if found.is_none()`, and `locate`
  answered `index: 0` with no refusal.

## Promoted 2026-08-25

The adapter's own stance is already stated at its site
(`kernel/src/fat32_adapter.rs`'s module header). What makes this a defect is
the two `toyos-gpt` residuals verified reproducing on 2026-08-24 — owed to
whoever owns `toyos-gpt`.
