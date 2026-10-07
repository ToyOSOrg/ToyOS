---
status: open
kind: defect
opened: 2026-09-27
---

# A link on a kernel mount to a served path leads nowhere

The kernel resolves a symlink on its own mounts — ROOT and `/tmp` — in its own
tree (`kernel/src/vfs.rs`), and its tree holds nothing under `/apps`,
`/config`, `/home`, `/state`, `/log` or `/boot` but ROOT's empty directories:
those are file servers', reached through a directory capability
(`sdk/std/sys/fs.rs`). So a link in `/tmp` whose target is
`/home/toy/notes` opens nothing (`NotFound`), where the same link on a served
directory is followed — a file server hands an absolute target back to the
client, which resolves it in its own table.

**Exit**: the kernel answers a path that crosses a link into a directory it
does not serve with the target and the rest of the path, as a file server's
`LINK` reply does, and std resolves it again in the caller's table; with a
guest test that opens a `/home` file through a `/tmp` link.
