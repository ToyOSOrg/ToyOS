---
status: open
kind: tooling
opened: 2026-09-21
---

# `deps` against the pinned archive costs five minutes, not one

Run `35576936014`, the first run of the dated image/archive pair, measured
`deps` at 295, 299, 329, 331, 333, 376, 426 and 428 s across eight guest
shards. The figure that job was priced at is 52-59 s, measured on run
`31896922288` and recorded in
`issues/build/building-the-image-once-and-shipping-it-cannot-shorten-the-matrix.md`,
which also holds what a shard's first verdict costs — so this is five or six
minutes added to the critical path of thirteen jobs, three times a day, and it
moves that entry's arithmetic.

Both arms of the comparison install the same eight packages from
`snapshot.debian.org`, so the archive is not new. What changed with the pair:
the step fetches over `http` instead of `https`, it no longer installs
`ca-certificates` from `deb.debian.org` first, and the date moved from
20260831 to 20260824. Candidates, unseparated: the transport, or a snapshot
date whose files no CDN edge had been asked for in weeks against one that
thirteen jobs had been pulling three times a day.

Cheap to separate, and nobody has: one dispatch of `probe-green.yml` with the
URL back on `https` reads the transport directly, and the same date's second
day of runs reads the cold-cache hypothesis for free. Do that before paying for
the cutover `issues/build/the-published-ci-image-is-pulled-by-no-job.md`
describes, since a baked image would hide the cause rather than answer it.
