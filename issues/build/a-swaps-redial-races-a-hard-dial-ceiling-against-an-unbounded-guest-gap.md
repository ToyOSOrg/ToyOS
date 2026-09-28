---
status: expected-red
kind: defect
opened: 2026-09-28
---

# A swap's redial races a hard dial ceiling against an unbounded guest gap

One mechanism, three tests, today
(`/private/tmp/claude-502/-Users-jan-Dev-jan-toyos/2280e09e-428b-4b81-bc00-1ede594b7247/scratchpad/orch-runs/`):

- `lan_swap`, `557r2-fast.log`
- `swap_netd` and `swap_crash_rolls_back`, `563r3-fast.log`

Each ended:

```
init's words on netd were ["accepted"] ending in None, where InService/Restored is owed (the stream had 1 connection(s) before the ask and 1 after)
the stream's redial was turned away 64 time(s), its ceiling of 64, and gave up
```

Each is green on about ten other Fast-tier runs the same day. On all three
boots the re-claimed netd worked end to end — DHCP lease, `logd` back on port
41337, and (on the two `Restored` boots) init's own `restored`/`in service`
line — so the guest side of every one of these three swaps finished. Only the
host's redial gave up first.

## What the code shows

`metalswap::swap` arms `Stream::redial` the moment init answers `accepted`
(`src/metalswap.rs:213-214`), and `Stream::redial` opens a plain TCP dial to
`logd`'s log-stream port (`toyos_logstream::CARRIER`). `serve`'s loop
(`src/metaltalk.rs:332-373`) counts every dial that is refused, reset, or
closes before a line, and redials **at once** — there is no wait between
attempts, the comment names the refusal itself as the event
(`src/metaltalk.rs:344-346`) — until a line arrives or
`metalswap::TURNED_AWAY_CEILING` (64) is reached. So the redial spends a
**fixed count** of dials against a gap whose length the *guest* sets: the old
netd's exit, the new one's spawn and DHCP lease, and — on the two
`Restored`/crash-rollback tests — init's whole 5000 ms "in service" probe
before it falls back to the one it replaced. This is
`issues/diagnostics/a-swaps-redial-asks-again-with-no-event-to-wait-on.md`'s
compromise, reached on all three names in one day rather than one.

Today's three consoles put a number on the race:

| test | old netd stopped | `logd` listening again | guest-side gap | dials spent |
|---|---|---|---|---|
| `lan_swap` | 2.975 s | 3.556 s | 581 ms | 64/64 |
| `swap_netd` | 3.171 s | 3.387 s | 216 ms | 64/64 |
| `swap_crash_rolls_back` | 2.983 s | 8.573 s | 5.59 s | 64/64 |

None of these three gaps is unusual — the same mechanism crosses gaps in this
range cleanly on the passing runs beside them — so 64 dials running out inside
216 ms to 5.6 s of guest time is not the guest arriving late; it is the fixed
budget of host-driven dials finishing before the guest reopens, at whatever
pace the host could drive TCP connects on that particular boot. Every one of
the three failing consoles carries a `[build-lock]` or `[host-builds]` line
naming another worktree's build holding a slot in the seconds around the swap
— the same host-contention shape `issues/build/swap-crash-rolls-back-reds-when-its-redial-spends-its-ceiling-under-load.md`
and `issues/build/lan-swap-redial-spent-its-ceiling-on-a-nightly-shard.md`
already record for this mechanism on other days.

## What is unknown

Nothing here measures the wall-clock cost of one redial round trip on a
contended host, and nothing distinguishes whether it is the host thread's own
scheduling (fewer redial attempts get to run at all in the same wall-clock
window) or the per-connect round trip through QEMU's user-mode network (each
attempt taking longer) that decides whether the 64th dial lands inside the
guest's gap or after it. Either explains a fixed count running out early under
load; nothing recorded here, or in the sibling issues above, measures a single
dial's cost to tell them apart.

## Exit condition

The mechanism named above, and a deterministic test red on it: a test that
forces the guest-side gap past what 64 dials cover at a stated, controlled
round-trip rate, reproducing this failure on demand rather than at a rate.
`issues/diagnostics/a-swaps-redial-asks-again-with-no-event-to-wait-on.md` is
the fix this exits into — the host waits on a guest-side event instead of a
dial count — and `lan_swap`, `swap_netd` and `swap_crash_rolls_back` come off
this file's `src/redlist.rs` rows with it.

Related sightings of the same mechanism, not folded in here:
`issues/build/lan-swap-redial-spent-its-ceiling-on-a-nightly-shard.md`,
`issues/build/swap-crash-rolls-back-reds-when-its-redial-spends-its-ceiling-under-load.md`,
`issues/build/swap-crash-rolls-back-redial-turned-away-once-on-mains-nightly.md`,
and the `swap_netd` bullet in
`issues/build/parallel-tests-red-under-other-suites.md`.
