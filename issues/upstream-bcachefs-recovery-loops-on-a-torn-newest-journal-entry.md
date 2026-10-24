---
status: open
kind: defect
opened: 2026-10-24
---

# Upstream bcachefs's recovery loops on a torn newest journal entry

bcachefs-tools v1.39.4 cannot recover an unclean volume in one crash state: its
newest journal entry is a torn flush entry whose header is whole, and that header's
last sequence is its own. ToyOS's writer used to leave that state.

**What upstream does** (storage analyst, A2-torn-newest A2.T1–T6, reproduced on
the judge):
1. While reading the journal, each flush entry's header last sequence discards
   every lower entry, whether or not the entry's checksum holds.
2. Only afterwards is the torn entry skipped. Nothing is left, so it prints
   "journal read done, but no entries found after dropping non-flushes".
3. Recovery falls back to the torn entry itself. A zeroed region inside one of its
   key records gives a key of size 0, and building the journal key list never
   advances past it. `fsck -n -K` spun at 100 % CPU with 4 GB resident until it
   was stopped.

Upstream's own writer records last sequence = seq whenever every older entry is
reclaimed (A2.T7). So, the analyst infers with medium-high confidence, it can
leave the same state itself; this was not reproduced.

**How ToyOS avoids it** (`close` and `write_entries` in
`lib/bcachefs/src/engine/mount/mod.rs`):
- **A2.24.** No flush entry names a last sequence past the newest entry already
  durable, so a torn newest entry leaves upstream the one before it to replay.
- **A2.25.** The superblock goes unclean only once an entry of the unclean span is
  durable.
- Two entries must name themselves: the first after a clean state (A2.25) and a
  clean unmount's last (A1.42). Each is written with its header sector last,
  after a flush, so a crash leaves it whole or absent.

The host tests `a_torn_newest_entry_leaves_upstream_an_entry_to_replay` and
`the_superblock_goes_unclean_only_once_an_entry_is_durable` hold this against
upstream's reading rule. ToyOS's own reader keeps A2.13's window, and recovers
the old state to the entry before.

**No upstream report is sent**: the owner's policy for now is to send none.

**Exit:** an upstream release that recovers the state, at a re-pin of the judge.
ToyOS's writer rules stay either way. A2.24 costs no write; writing a
self-naming entry header last costs one flush and one 4 KiB write more, each
time going read-write from an empty journal and at each clean unmount.
