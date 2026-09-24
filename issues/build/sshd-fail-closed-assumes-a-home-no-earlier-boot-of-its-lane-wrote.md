---
status: open
kind: tooling
opened: 2026-09-24
---

# `sshd_fail_closed` assumes a `/home` no earlier boot of its lane wrote

`sshd_fail_closed` owes `sshd: minted a new host identity at
/home/root/.ssh/host_ed25519`, and boots `tests/sshdcase` on the lane's shared
scratch disk (`tests/common/qemu.rs`: "Reused across the boots of one lane").
Any earlier boot of the same lane whose sshd ran leaves its identity on that
disk, and this boot then reads it and mints nothing.

Seen on CI (run 35984557404, shard 5): `lan_talk` ran first in the lane and its
sshd minted a key; `sshd_fail_closed` then said `host identity SHA256:yYw2…`
with the same fingerprint on both of its runs and no `minted` line, red twice.
On main the two had not shared a shard. `lan_talk` now boots a disk of its own,
which removes this one neighbour and not the premise.

## Exit condition

`sshd_fail_closed` boots a disk of its own, as `tests/common/pkg.rs` does, so
its verdict does not depend on which tests the shard split put before it.
