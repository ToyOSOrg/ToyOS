---
status: open
kind: finding
opened: 2026-09-01
---

# Three tracker citations name issue files that no longer exist

Found by looping every `issues/<area>/<slug>.md` string in `issues/` against the
filesystem. Three do not resolve:

- `issues/kernel/two-i8042-verdicts-red-together-on-one-ci-shard.md` and
  `issues/build/i8042-keyboard-pays-a-lost-sentinel-and-reds-the-durations-gate.md`,
  both cited by `issues/build/defect-events.md` in the entry that records PR #143
  closing them.
- `issues/kernel/scheduler-policy-behavior-has-no-quantified-suite.md`, cited by
  `issues/build/the-swarm-is-not-yet-falsifiable.md`.

`issuegate` passes, so nothing in the tree checks this today, and CLAUDE.md's
rule — a merge that deletes a document deletes every citation to it, searched by
bare name as well as by path — was not applied to these.

**The two cases are not the same, and the fix is not the same.**
`defect-events.md` is append-only by its own instruction: an entry naming what a
landing closed is a record of a closure, and rewriting it to remove the name
would destroy the thing the ledger exists to keep. If that is right, the ledger
needs an exemption stated in its own header rather than a silent one. The swarm
track's citation is an ordinary forward reference and is simply stale.

At its next review this is promoted to a defect — with the loop above as the
gate to add — or folded into `issues/README.md` and closed.
