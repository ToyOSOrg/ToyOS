---
status: expected-red
kind: defect
opened: 2026-09-28
---

# A swap's redial races a hard dial ceiling against an unbounded guest gap

One mechanism, six sightings across `lan_swap`, `swap_netd` and
`swap_crash_rolls_back`:

- `lan_swap`, Fast tier.
- `swap_netd` and `swap_crash_rolls_back`, Fast tier.
- `lan_swap`, PR #535's nightly (run 36314576406, `guest (1)`, `a4f68c5a`, KVM, QEMU 11.1.0).
- `swap_crash_rolls_back`, on origin/main's netd and on a branch's, under ten spinning host threads beside the run.
- `swap_crash_rolls_back`, main's nightly (`1ce71831`, run 36290616312), not seen on the nightly before #527 (run 36285169430).
- `swap_netd`, the rust-lld branch's fast tier (`e4317d3f`, PR #532), the one red of 400 besides `lan_mdns_answer`'s `SUN_LEN`.

The guest side finished on every sighting that shows a console: today's three
boots reached DHCP lease, `logd` back on port 41337, and init's own
`restored`/`in service` line; `lan_swap`'s nightly guest reached `logd: serving
this boot's log on port 41337` at 1.176 s and `init: swap netd: in service` at
6.141 s, with no second `serving this boot's log to 10.0.2.2:…` line; both
`swap_crash_rolls_back` sightings' consoles show the rollback completing
(`restored`, then `logd` serving again). Only the host's redial gave up first,
every time. Every sighting's `cargo run -- --known-red` answered NO (not
quarantined), and an alone re-run is reliably green: 4 dials turned away on
`lan_swap`'s nightly, `swap_netd` green in 10 s on the rust-lld branch,
`swap_crash_rolls_back` green twice on main's nightly and once with netd
reverted to origin/main.

## What the code shows

`metalswap::swap` arms `Stream::redial` after logd's `CARRIER_LEAVING` and the
`go` (`src/metalswap.rs`), and `Stream::redial` opens a plain TCP dial to
`logd`'s log-stream port (`toyos_logstream::CARRIER`). `serve`'s loop
(`src/metaltalk.rs`) counts every dial that is refused, reset, or closes
before a line, and redials **at once** — there is no wait between attempts,
the comment names the refusal itself as the event — until a line arrives or
`metalswap::TURNED_AWAY_CEILING` (64) is reached. So the redial spends a
**fixed count** of dials against a gap whose length the *guest* sets: the old
netd's exit, the new one's spawn and DHCP lease, and init's whole 5000 ms "in
service" probe before it falls back to the one it replaced. This is
`issues/diagnostics/a-swaps-redial-asks-again-with-no-event-to-wait-on.md`'s
compromise.

## What the measurement shows

In `swap_netd`, 64 dials were refused in 326 ms between logd's `netd is being
replaced` (3.061 s) and its re-listen (3.387 s), each taking 5 ms or less.
Green swaps in the same runs were refused 3, 6 and 22 times. The dials got
faster on the red runs, not slower, which points at the refusal window logd
holds open while the old netd is still up.

## Exit condition

`Stream::redial` gives up on its time bound alone, never on a dial count, and a
swap whose refusal window is staged long is green; the three rows come off with
it. Owner: `src/metaltalk.rs`'s `Stream::redial`; held by the orchestrator.
