---
status: open
kind: defect
opened: 2026-09-27
---

# A file held across a file server's restart answers Gone

No volume records an object id, so nothing tells the held file from another
put at its path since, and a handle held across a restart answers `Gone`
(`std`'s `sdk/std/sys/fs.rs`); `logd` loses `/log` for the boot if LOG's server
restarts.

**Exit**: DATA's entry carries an object id and a generation that nothing
reuses, a held handle reopens only when both match, and a host test that
makes a same-length file in a held file's freed block within the second has
the reopen refused.
