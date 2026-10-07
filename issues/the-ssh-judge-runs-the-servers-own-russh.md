---
status: open
kind: defect
opened: 2026-10-04
---

# The SSH judge runs the server's own russh

`tests/ssh-client-host` is the harness's SSH client, and the root manifest
keeps it out of the workspace as "a second implementation". It is not one:
it takes `russh` from `https://github.com/ToyOSOrg/russh`, branch `toyos`,
and its lock pins commit `389804d3b9908250282746c934c58c059b88fb7e`, the same
commit `Cargo.lock` pins for `sshserver` through the root
`[patch]`. A defect in the fork's protocol code is on both ends of every SSH
test, and the two can agree on it.

**Exit:** the judge's lock names no `ToyOSOrg` source, or the root
manifest's exclusion comment stops calling it a second implementation and
this file names what judges the server's protocol instead.
