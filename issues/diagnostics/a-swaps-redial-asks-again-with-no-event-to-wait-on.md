---
status: assigned
kind: defect
opened: 2026-09-25
---

# A swap's redial asks again with no event to wait on

Held by the orchestrator.

A swap of netd ends the host's log stream with no FIN and no reset, and
`src/metaltalk.rs`'s `Stream::redial` dials `logd` again. Nothing the machine
sends says when `logd` listens again: the stream and the ssh channel both die
with the old netd, so init's words are unreachable until a dial succeeds. So
every dial turned away — refused with a reset by the old netd whose `logd`
listener is closed, or by the new netd before `logd` binds; or, through QEMU's
forward, accepted and closed before a line — is asked again at once. On a LAN
that is a question for the name on the link, which the old netd answers at
once, and a `connect` per round trip for the whole gap.

The T14 is unmeasured, and a refusal there costs a LAN round trip rather than
QEMU's forward's. Its redial asks the name on the link again after every dial
turned away, as soon as the old netd answers the last ask, so its questions
may go out faster than RFC 6762 §5.2's floor between two queries
(`ASK_WAIT`); that rate is unmeasured on metal.

The forward's cost is measured: a hold-green run turned away 8125 dials in
7019 ms, and the guest logged 16231 of its 19034 interrupts over that boot, all
on cpu0.

## Exit condition

The host waits on a guest-side event instead of asking again: something the
machine sends when `logd` can admit a reader again after a swap of netd —
for example `logd` keeping its listener across the swap and holding the
connections it accepts until the new netd serves, or netd announcing its
exit on a channel that outlives it — and `Stream::redial` dials once per
such event, with the count deleted, so no redial asks the link faster than
§5.2's floor.
