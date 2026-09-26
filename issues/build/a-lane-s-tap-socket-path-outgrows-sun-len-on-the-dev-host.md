---
status: open
kind: defect
opened: 2026-09-26
---

# A lane's tap socket path outgrows `SUN_LEN` on the dev host

`lan_mdns_answer` reds wide and alone with `connect to QEMU's
/private/var/folders/gr/mr4_fg4n34jb417sx1g5cgxc0000gp/T/toyos-tmp-70685-0/tests-0/lane-7/tap-out-0.sock:
path must be shorter than SUN_LEN`. `common::segment::Tap::in_lane` puts the
two sockets in the lane's scratch directory, and on this macOS host that
directory sits under `$TMPDIR`, so the path is 104 bytes, past the 103 a
`sockaddr_un` holds before its terminating NUL on macOS.

Seen in the fast tier twice in one session: at `origin/main` checked out in
the `toyos-guiplat` worktree (alone with this message, wide as `QEMU died
before ===READY===`), and at PR #528's head after it merged `e48604c0` (this
message wide and alone). `cargo run
-- --known-red lan_mdns_answer` answers NO.

**Exit**: the socket paths fit a `sockaddr_un` wherever the scratch
directory is, with `lan_mdns_answer` green on this host.
