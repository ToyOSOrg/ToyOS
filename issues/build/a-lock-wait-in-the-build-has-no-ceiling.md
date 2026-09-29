---
status: open
kind: tooling
opened: 2026-09-29
---

# A lock wait in the build has no ceiling

Every blocking lock in `src/dirlock.rs` says what it waits for every 30 s and
waits for as long as its holder lives. A killed holder releases it, but a live
one that hangs — a bootstrap stuck in a fetch, a maker blocked on a terminal —
holds every build waiting for that key, that checkout or that store forever,
each of them saying so every 30 s and none of them failing. A legitimate hold
lasts anywhere from a record's write (`store::record`) to an LLVM's making, so
no one ceiling fits them all.

Exit: a wait whose holder has made no progress past a bound its kind of hold
declares fails loudly, naming the holder's pid and what it holds.
