---
status: open
kind: defect
opened: 2026-10-24
---

# `test_rs_socket_export` hangs at its race

`netstack_holders_exported_to_children` (`tests/toyos.rs`) runs
`test_rs_socket_export`, which says its first claim — "a listener and a
datagram socket exported to a child: it accepted and spoke on them, and so
did the parent after it" — spawns its racer, and then neither the job nor the
guest says anything more: the run's guard expires with the guest silent for
its whole span (`STALLED`). The host's peer is waiting on the two `racing`
datagrams of `sockets_exported_to_a_child`'s race.

**Evidence** (q35 TCG, host load averages 20 to 28): on `origin/main`
`2a5b446b0` it hung 1 run of 1 (965 s, silent throughout); on PR #1062's
branch it hung 2 runs of 3 and passed once. The capture of the base run is in
PR #1062's evidence (`pr-1062`, `guest/socket-export-hang-base.log`). Nothing
in either says which side is waiting: the datagram, the racer's spawn or its
stdout line.

**Owner:** unheld; whoever next holds the socket-sharing track
(`issues/a-socket-libc-or-socket2-has-not-bound-is-not-shared-with-a-child.md`).

**Exit:** the race is read to its end — which party waits on what at the
hang — and `netstack_holders_exported_to_children` passes 20 of 20 under the
load it hung at.
