---
status: open
kind: defect
opened: 2026-09-25
---

# A swap's redial asks again with no event to wait on

A swap of netd ends the host's log stream with no FIN and no reset, and
`src/metaltalk.rs`'s `Stream::redial` dials `logd` again. Nothing the machine
sends says when `logd` listens again: the stream and the ssh channel both die
with the old netd, so init's words are unreachable until a dial succeeds. So
every dial turned away — refused with a reset by the old netd whose `logd`
listener is closed, or by the new netd before `logd` binds; or, through QEMU's
forward, accepted and closed before a line — is asked again at once. On a LAN
that is a `getaddrinfo` and a `connect` per round trip for the whole gap.

What bounds it: every dial turned away is counted, refusals included
(`Stream::turned_away`), and a redial gives up at
`metalswap::TURNED_AWAY_CEILING`, which the swap's judge reds on by name
(`a_redial_counts_every_refusal_and_gives_up_at_its_ceiling`). The T14 is
unmeasured, and a refusal there costs a LAN round trip rather than QEMU's
forward's.

## Exit condition

The host waits on a guest-side event instead of asking again: something the
machine sends when `logd` can admit a reader again after a swap of netd —
for example `logd` keeping its listener across the swap and holding the
connections it accepts until the new netd serves, or netd announcing its
exit on a channel that outlives it — and `Stream::redial` dials once per
such event, with the ceiling and the count deleted.
