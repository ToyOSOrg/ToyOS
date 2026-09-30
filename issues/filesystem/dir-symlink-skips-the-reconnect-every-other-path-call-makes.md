---
status: open
kind: defect
opened: 2026-09-27
---

# `Dir::symlink` skips the reconnect every other path call makes

`toyos/src/fs.rs`'s `Dir::symlink` writes its own request instead of going
through `Dir::path_call`, so it answers a server's restart `Gone` where every
other path call reconnects once and asks again. Its only difference of
substance is that the target is stored, not resolved, and so is not held to
`canonical`.

Owner: the SDK's file-server client (`toyos/src/fs.rs`).

**Exit**: `Dir::symlink` is one `path_call`, with the target exempt from
`canonical`, and a test that makes a symlink across a server's restart.
