---
status: expected-red
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
message wide and alone).

## Two shapes, one length

`$TMPDIR/toyos-tmp-<pid>-<n>/tests-0/lane-<i>/tap-{in,out}-<k>.sock` is
104 bytes with a five-digit pid and a one-digit lane, and 105 with a
two-digit lane. At 104 QEMU binds the socket and the host's
`UnixStream::connect` refuses it (`path must be shorter than SUN_LEN`); at
105 QEMU refuses its own `-chardev` (`UNIX socket path … is too long / Path
must be less than 104 bytes`) and exits 1 with nothing on the console or
the UART, which the harness reports as `QEMU died before ===READY===
(status: … 256)`. Those two numbers are the fast-tier lines of PR #536's
logs, lane-5 and lane-11.

An A/B at `16d2e645` against PR #536, two runs per arm per `$TMPDIR`:
the default reds with the first shape on both, a `$TMPDIR` three bytes
longer reds with the second on both, and a `$TMPDIR` under the worktree's
`target/` passes on both.

The first shape is quarantined in `src/redlist.rs`. The second is not
and cannot be: its failure text is the harness's boot-death framing and
nothing of this defect, so a row quoting it would excuse every guest
death of this test. A wide run that lands the test on lane 10 or 11 on
this host reds the suite.

**Exit**: the socket paths fit a `sockaddr_un` wherever the scratch
directory is, with `lan_mdns_answer` green on this host and its row gone
from `src/redlist.rs`.
