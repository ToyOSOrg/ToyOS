---
status: open
kind: tooling
opened: 2026-09-27
---

# `swap_crash_rolls_back` gives up its log stream inside the swap's probation

The test swaps netd for a binary that panics at once, and judges init's words
on the swap off logd's network stream, which netd carries. The stream is down
for the whole of the failed binary's probation (`toyos_swap::PROBATION_MS`,
5 s) until init restores netd, and the harness redials it with a ceiling of
64 refusals. On a loaded dev host, run as one named test, the ceiling was
spent before the restore twice: "the stream's redial was turned away 64
time(s), its ceiling of 64, and gave up", with init's words read as
`["accepted"]`, while the console of the same run carries every word on time
— stopping, started, failed at 5.953 s, restored at 5.985 s. Run alone right
after, both times green in 8–9 s.

**Exit**: the redial's bound is the probation plus the restore, stated in time
and not in refusals, and the wide run green on a loaded host.
