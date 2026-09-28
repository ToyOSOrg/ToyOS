---
status: open
kind: tooling
opened: 2026-09-28
---

# A stuck-but-chatty guest costs the full ceiling backstop before the harness ends it

`ceiling_verdict` (`tests/common/qemu.rs:672-707`) ends a test's wait early
only when the guest has both passed its own budget *and* gone silent for
`GUEST_QUIET` (15 s, `tests/common/qemu.rs:509`); a guest still printing
anything — including the kernel's own periodic idle-loop line on a 10 s
cadence, named at `tests/common/qemu.rs:505-508` as one of the periodic
speakers that keeps a guest "talking" with no live test progress behind it —
never trips that arm and runs on to the absolute backstop,
`ceiling.max(GUEST_WEDGED)` (`tests/common/qemu.rs:702`, `GUEST_WEDGED` =
300 s, `tests/common/qemu.rs:530`), scaled by the per-test timeout, the
phase's `WIDTH` and `host_scale`/oversubscription (`budget`,
`tests/common/qemu.rs:244-245`; `budget_smp`, `tests/common/qemu.rs:375-377`).

Sighting: `netd_refused_accept` (base timeout 120 s,
`tests/toyos.rs:10111`) hung on `2a9c77ee` and was reported only at
`FAIL netd_refused_accept: timed out after 2341s, with the guest still
talking 8s ago` (`orch-runs/562r5-fast.log` lines ~1160-1169) — 39 minutes of
wall clock before the harness ended it, because the guest's periodic
console lines never let `quiet >= GUEST_QUIET` hold.

`tests/CLAUDE.md` already states this as the intended design (a talking
guest is judged slow, not stuck, until the backstop). Nothing here says the
design is wrong; recorded because a real stuck guest, if only "chatty" for a
reason unrelated to progress, is priced at the full backstop rather than the
much smaller `GUEST_QUIET` silence window — a wall-clock cost a CI run pays
whenever this shape recurs.
