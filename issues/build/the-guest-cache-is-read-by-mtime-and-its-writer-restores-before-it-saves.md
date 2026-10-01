---
status: open
kind: tooling
opened: 2026-10-01
---

# The guest cache is read by mtime, and its writer restores before it saves

`nightly.yml`'s `tcg` restores the newest `guest-` entry, builds on it and saves
the result: nothing prunes what no step rebuilt, so every write keeps the last
one's artifacts and adds its own (3,281,375,938 B on 2026-10-01, beside the
host entry in the repository's 10 GB). And every guest job restores its targets
under a checkout that dated every source at the checkout, so cargo calls every
path crate in them stale.

Done when the guest entry is written cold, read by content, and bounded.
