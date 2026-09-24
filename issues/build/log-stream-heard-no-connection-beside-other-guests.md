---
status: open
kind: defect
opened: 2026-09-23
---

# `log_stream` heard no connection in 90 s beside four other guests

`cargo test --test toyos-build -- --nightly log_stream` on branch `t14-talk`
(stacked on `i219-phy`) failed once: `log_stream` said `"Boot: complete" never
arrived on the stream in 90s: 0 connection(s), 0 line(s), ended=false`, while
the four other `log_stream_*` guests of the same parallel pass were green. The
re-run alone passed (`Boot: complete` 30 ms into the listener's life, 249 lines),
and a second whole run of the five minutes later was 5 of 5 green. That pass was
the first after `userland/logd` was rebuilt, so every guest's image was being
built at once (`artifact staging acquired after 8.8s` on three of them).

Zero connections means virtio netd never accepted `logd`'s connect, or `logd`
never asked: neither is visible, because the arm keeps the guest's console only
on a pass. The branch's one change to `logd` widens `OPEN_BOUND` from 30 s to
60 s, which cannot remove a connection made in the first 30 s. Not
established: whether the base shares it — no same-session A/B was run.

Owed: the guest's console on this red (what `logd` and netd said), and a rate
beside other guests against the base.
