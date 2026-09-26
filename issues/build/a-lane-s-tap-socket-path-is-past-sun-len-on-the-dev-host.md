---
status: open
kind: tooling
opened: 2026-09-26
---

# A lane's tap socket path is past `SUN_LEN` on the dev host

`lan_mdns_answer` reds on the macOS dev host, wide and alone:

```
connect to QEMU's /private/var/folders/gr/mr4_fg4n34jb417sx1g5cgxc0000gp/T/toyos-tmp-89085-0/tests-0/lane-3/tap-out-0.sock: path must be shorter than SUN_LEN
```

That path is 104 bytes, and macOS's `sun_path` holds 104 including the NUL.
`tests/common/segment.rs`'s `Tap::in_lane` puts both sockets in
`lane::dir()`, which since `toyos-tmpdir` is
`$TMPDIR/toyos-tmp-<pid>-<n>/tests-<n>/lane-<i>/`, and the dev host's
`$TMPDIR` resolves to 57 bytes (`/private/var/folders/…/T/`) before any of
that. A five-digit pid is enough to cross the limit. Seen on
`wt/toyos-layout` after it merged `origin/main` at `e48604c0`; nothing on that
branch touches the lane or the tap.

## Exit condition

A tap socket's path fits `sun_path` on every host the suite runs on, and
`lan_mdns_answer` is green on the dev host.
