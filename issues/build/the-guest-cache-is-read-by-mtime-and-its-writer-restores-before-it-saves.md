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

The guest entry's ceiling is what H + 2G ≤ 10 GB leaves it, and that sum binds
every night: 4,002,930,524 B at the host's `LIMIT` (`src/cicache.rs`), or
4,298,336,717 B at run 36878222090's H. Above it, `tcg`'s save evicts that
night's host entry unless a pull request has read the entry since the guest
restore; every pull request then runs cold, and nothing reds.

Its jobs are not held to the allow-list `src/ci.rs`'s
`each_cache_has_one_writer` holds the host cache's jobs to: `|| true` on
`tcg`'s run line would let its save store a red run's tree, and the gate stays
green.

Done when the guest entry is written cold, read by content, and bounded, and
its jobs are held to that allow-list, which #671 carries.
